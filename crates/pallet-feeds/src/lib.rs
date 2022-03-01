// Copyright (C) 2021 Subspace Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Pallet feeds, used for storing arbitrary user-provided data combined into feeds.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(rust_2018_idioms, missing_debug_implementations)]

use application_crypto::KeyTypeId;
use codec::{Decode, Encode};
use core::mem;
pub use pallet::*;
use sp_core::RuntimeDebug;
use sp_runtime::traits::{Header as HeaderT, Verify};
use sp_runtime::{generic, MultiSignature, OpaqueExtrinsic};
use sp_std::prelude::*;
use subspace_core_primitives::{crypto, Sha256Hash};

#[cfg(all(feature = "std", test))]
mod mock;
#[cfg(all(feature = "std", test))]
mod tests;

pub type BlockNumber = u64;

#[derive(PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug)]
pub struct Block<Header> {
    /// The block header.
    pub header: Header,
    /// The accompanying extrinsics.
    pub extrinsics: Vec<OpaqueExtrinsic>,
}

#[derive(Debug, Encode, Decode, Clone)]
pub struct FinalityProof<Header: HeaderT> {
    /// The hash of block F for which justification is provided.
    pub block: Header::Hash,
    /// Justification of the block F.
    pub justification: Vec<u8>,
    /// The set of headers in the range (B; F] that we believe are unknown to the caller. Ordered.
    pub unknown_headers: Vec<Header>,
}

pub type SignedBlock<Header> = generic::SignedBlock<Block<Header>>;
pub type Signature = MultiSignature;
pub type AccountPublic = <Signature as Verify>::Signer;
pub const ASSIGNMENT_KEY_TYPE_ID: KeyTypeId = KeyTypeId(*b"asgn");
pub const PARACHAIN_KEY_TYPE_ID: KeyTypeId = KeyTypeId(*b"para");

#[frame_support::pallet]
mod pallet {
    use super::SignedBlock;
    use bp_header_chain::InitializationData;
    use bp_runtime::Chain;
    use frame_support::pallet_prelude::*;
    use frame_system::pallet_prelude::*;
    use hex_literal::hex;
    use sp_core::crypto::UncheckedInto;
    use sp_finality_grandpa::AuthorityId as GrandpaId;
    use sp_runtime::traits::Header as HeaderT;
    use sp_std::prelude::*;

    #[pallet::config]
    pub trait Config: frame_system::Config + pallet_bridge_grandpa::Config {
        /// `pallet-feeds` events
        type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;
    }

    /// Pallet feeds, used for storing arbitrary user-provided data combined into feeds.
    #[pallet::pallet]
    #[pallet::generate_store(pub(super) trait Store)]
    #[pallet::without_storage_info]
    pub struct Pallet<T>(_);

    /// User-provided object to store
    pub(super) type Object = Vec<u8>;
    /// ID of the feed
    pub(super) type FeedId = u64;
    /// User-provided object metadata (not addressable directly, but available in an even)
    pub(super) type ObjectMetadata = Vec<u8>;

    /// Total amount of data and number of objects stored in a feed
    #[derive(Debug, Decode, Encode, TypeInfo, Default, PartialEq, Eq)]
    pub struct TotalObjectsAndSize {
        /// Total size of objects in bytes
        pub size: u64,
        /// Total number of objects
        pub count: u64,
    }

    #[pallet::storage]
    #[pallet::getter(fn metadata)]
    pub(super) type Metadata<T: Config> =
        StorageMap<_, Blake2_128Concat, FeedId, ObjectMetadata, OptionQuery>;

    #[pallet::storage]
    #[pallet::getter(fn totals)]
    pub(super) type Totals<T: Config> =
        StorageMap<_, Blake2_128Concat, FeedId, TotalObjectsAndSize, ValueQuery>;

    #[pallet::storage]
    #[pallet::getter(fn current_feed_id)]
    pub(super) type CurrentFeedId<T: Config> = StorageValue<_, FeedId, ValueQuery>;

    /// `pallet-feeds` events
    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        /// New object was added.
        ObjectSubmitted {
            metadata: ObjectMetadata,
            who: <T as frame_system::Config>::AccountId,
            object_size: u64,
        },
        /// New feed was created.
        FeedCreated {
            feed_id: FeedId,
            who: <T as frame_system::Config>::AccountId,
        },
        /// Submitted object is valid
        ObjectIsValid { metadata: ObjectMetadata },
        /// Submitted object is not valid
        ObjectIsInvalid { metadata: ObjectMetadata },
    }

    /// `pallet-feeds` errors
    #[pallet::error]
    pub enum Error<T> {
        /// `FeedId` doesn't exist
        UnknownFeedId,
        /// Failed to decode finality proof
        FailedDecodingProof,
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        // TODO: add proper weights
        /// Create a new feed
        #[pallet::weight(10_000)]
        pub fn create(origin: OriginFor<T>) -> DispatchResult {
            let who = ensure_signed(origin)?;

            let feed_id = Self::current_feed_id();

            CurrentFeedId::<T>::mutate(|feed_id| *feed_id = feed_id.saturating_add(1));

            Totals::<T>::insert(feed_id, TotalObjectsAndSize::default());

            Self::deposit_event(Event::FeedCreated { feed_id, who });

            Ok(())
        }

        // TODO: add proper weights
        // TODO: For now we don't have fees, but we will have them in the future
        /// Put a new object into a feed
        #[pallet::weight((10_000, Pays::No))]
        pub fn put(
            origin: OriginFor<T>,
            feed_id: FeedId,
            object: Object,
            metadata: ObjectMetadata,
            proof: Option<Vec<u8>>,
        ) -> DispatchResult {
            let who = ensure_signed(origin.clone())?;

            let object_size = object.len() as u64;

            let block = SignedBlock::<
                <<T as pallet_bridge_grandpa::Config>::BridgedChain as Chain>::Header,
            >::decode(&mut &object[..])
            .unwrap();

            log::info!("decoded: {:?}", block);

            let remote_proof = proof.unwrap_or_default();

            let proof = super::FinalityProof::<T::Header>::decode(&mut &remote_proof[..])
                .map_err(|_| Error::<T>::FailedDecodingProof)?;

            log::info!("proof {:?}", proof);

            let block_number = *block.block.header.number();

            // only Kusama blocks for now
            if feed_id == 0 {
                log::info!("Is Kusama feed");
                // init bridge at genesis block
                if block_number == 0u32.into() {
                    log::info!("Kusama block number {:?}", block_number);
                    // TODO: check if authority weights should be 1
                    let kusama_initial_authorities: Vec<(GrandpaId, u64)> = vec![
                        (
                            hex![
                                "76620f7c98bce8619979c2b58cf2b0aff71824126d2b039358729dad993223db"
                            ]
                            .unchecked_into(),
                            1,
                        ),
                        (
                            hex![
                                "e2234d661bee4a04c38392c75d1566200aa9e6ae44dd98ee8765e4cc9af63cb7"
                            ]
                            .unchecked_into(),
                            1,
                        ),
                        (
                            hex![
                                "5b57ed1443c8967f461db1f6eb2ada24794d163a668f1cf9d9ce3235dfad8799"
                            ]
                            .unchecked_into(),
                            1,
                        ),
                        (
                            hex![
                                "e60d23f49e93c1c1f2d7c115957df5bbd7faf5ebf138d1e9d02e8b39a1f63df0"
                            ]
                            .unchecked_into(),
                            1,
                        ),
                    ];

                    let init_data = InitializationData::<
                        <<T as pallet_bridge_grandpa::Config>::BridgedChain as Chain>::Header,
                    > {
                        header: Box::new(block.block.header),
                        authority_list: kusama_initial_authorities,
                        set_id: 0,
                        is_halted: false,
                    };

                    pallet_bridge_grandpa::Pallet::<T>::initialize(origin, init_data.clone())
                        .map(|_| init_data);
                }

                // pallet_bridge_grandpa::Pallet::<T>::submit_finality_proof(
                //     origin,
                //     Box::new(block.block.header),
                //     justification,
                // );

                // // no justifications - PoA block
                // if block.justifications.is_none() {
                //     log::info!("No justifications, assume valid: {:?}", block_number);

                //     Self::deposit_event(Event::ObjectIsValid {
                //         metadata: metadata.clone(),
                //     });
                // } else {
                //     log::info!("justifications: {:?}", block.justifications);

                //     // Self::deposit_event(Event::ObjectIsInvalid {
                //     //     metadata: metadata.clone(),
                //     // });
                // }
            }

            log::debug!("metadata: {:?}", metadata);
            log::debug!("object_size: {:?}", object_size);

            let current_feed_id = Self::current_feed_id();

            ensure!(current_feed_id >= feed_id, Error::<T>::UnknownFeedId);

            Metadata::<T>::insert(feed_id, metadata.clone());

            Totals::<T>::mutate(feed_id, |feed_totals| {
                feed_totals.size += object_size;
                feed_totals.count += 1;
            });

            Self::deposit_event(Event::ObjectSubmitted {
                metadata,
                who,
                object_size,
            });

            Ok(())
        }
    }
}

/// Mapping to the object offset and size within an extrinsic
#[derive(Debug)]
pub struct CallObject {
    /// Object hash
    pub hash: Sha256Hash,
    /// Offset of object in the encoded call.
    pub offset: u32,
}

impl<T: Config> Call<T> {
    /// Extract the call object if an extrinsic corresponds to `put` call
    pub fn extract_call_object(&self) -> Option<CallObject> {
        match self {
            Self::put { object, .. } => {
                // `FeedId` is the first field in the extrinsic. `1+` corresponds to `Call::put {}`
                // enum variant encoding.
                Some(CallObject {
                    hash: crypto::sha256_hash(object),
                    offset: 1 + mem::size_of::<FeedId>() as u32,
                })
            }
            _ => None,
        }
    }
}
