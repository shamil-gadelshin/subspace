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
use sp_runtime::traits::{BlakeTwo256, IdentifyAccount, Verify};
use sp_runtime::{generic, MultiSignature, OpaqueExtrinsic};
use sp_std::prelude::*;
use subspace_core_primitives::{crypto, Sha256Hash};

#[cfg(all(feature = "std", test))]
mod mock;
#[cfg(all(feature = "std", test))]
mod tests;

pub type BlockNumber = u32;
pub type Header = generic::Header<BlockNumber, BlakeTwo256>;

#[derive(PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug)]
pub struct Block {
    /// The block header.
    pub header: Header,
    /// The accompanying extrinsics.
    pub extrinsics: Vec<OpaqueExtrinsic>,
}

pub type SignedBlock = generic::SignedBlock<Block>;
pub type Signature = MultiSignature;
pub type AccountPublic = <Signature as Verify>::Signer;
pub type AccountId = <AccountPublic as IdentifyAccount>::AccountId;
pub const ASSIGNMENT_KEY_TYPE_ID: KeyTypeId = KeyTypeId(*b"asgn");
mod assignment_app {
    use application_crypto::{app_crypto, sr25519};
    app_crypto!(sr25519, super::ASSIGNMENT_KEY_TYPE_ID);
}
pub type AssignmentId = assignment_app::Public;
pub const PARACHAIN_KEY_TYPE_ID: KeyTypeId = KeyTypeId(*b"para");
mod validator_app {
    use application_crypto::{app_crypto, sr25519};
    app_crypto!(sr25519, super::PARACHAIN_KEY_TYPE_ID);
}
pub type ValidatorId = validator_app::Public;

#[frame_support::pallet]
mod pallet {
    use super::AccountId;
    use super::AssignmentId;
    use super::SignedBlock;
    use super::ValidatorId;
    use bp_header_chain::InitializationData;
    use frame_support::pallet_prelude::*;
    use frame_system::pallet_prelude::*;
    use grandpa::AuthorityId as GrandpaId;
    use hex_literal::hex;
    use pallet_im_online::sr25519::AuthorityId as ImOnlineId;
    use sp_authority_discovery::AuthorityId as AuthorityDiscoveryId;
    use sp_consensus_babe::AuthorityId as BabeId;
    use sp_core::crypto::UncheckedInto;
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
            who: T::AccountId,
            object_size: u64,
        },
        /// New feed was created.
        FeedCreated { feed_id: FeedId, who: T::AccountId },
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
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;

            let object_size = object.len() as u64;

            let block = SignedBlock::decode(&mut &object[..]).unwrap();

            log::info!("decoded: {:?}", block);

            // only Kusama blocks for now
            if feed_id == 0 {
                // no justifications - PoA block
                if block.justifications.is_none() {
                    log::info!(
                        "No justifications, assume valid: {:?}",
                        block.block.header.number
                    );

                    Self::deposit_event(Event::ObjectIsValid {
                        metadata: metadata.clone(),
                    });
                } else {
                    log::info!("justifications: {:?}", block.justifications);

                    // Self::deposit_event(Event::ObjectIsInvalid {
                    //     metadata: metadata.clone(),
                    // });
                }
            }

            let initial_authorities: Vec<(
                AccountId,
                AccountId,
                BabeId,
                GrandpaId,
                ImOnlineId,
                ValidatorId,
                AssignmentId,
                AuthorityDiscoveryId,
            )> = vec![
                (
                    // 5DD7Q4VEfPTLEdn11CnThoHT5f9xKCrnofWJL5SsvpTghaAT
                    hex!["32a5718e87d16071756d4b1370c411bbbb947eb62f0e6e0b937d5cbfc0ea633b"].into(),
                    // 5GNzaEqhrZAtUQhbMe2gn9jBuNWfamWFZHULryFwBUXyd1cG
                    hex!["bee39fe862c85c91aaf343e130d30b643c6ea0b4406a980206f1df8331f7093b"].into(),
                    // 5FpewyS2VY8Cj3tKgSckq8ECkjd1HKHvBRnWhiHqRQsWfFC1
                    hex!["a639b507ee1585e0b6498ff141d6153960794523226866d1b44eba3f25f36356"]
                        .unchecked_into(),
                    // 5EjvdwATjyFFikdZibVvx1q5uBHhphS2Mnsq5c7yfaYK25vm
                    hex!["76620f7c98bce8619979c2b58cf2b0aff71824126d2b039358729dad993223db"]
                        .unchecked_into(),
                    // 5FpewyS2VY8Cj3tKgSckq8ECkjd1HKHvBRnWhiHqRQsWfFC1
                    hex!["a639b507ee1585e0b6498ff141d6153960794523226866d1b44eba3f25f36356"]
                        .unchecked_into(),
                    // 5FpewyS2VY8Cj3tKgSckq8ECkjd1HKHvBRnWhiHqRQsWfFC1
                    hex!["a639b507ee1585e0b6498ff141d6153960794523226866d1b44eba3f25f36356"]
                        .unchecked_into(),
                    // 5FpewyS2VY8Cj3tKgSckq8ECkjd1HKHvBRnWhiHqRQsWfFC1
                    hex!["a639b507ee1585e0b6498ff141d6153960794523226866d1b44eba3f25f36356"]
                        .unchecked_into(),
                    // 5FpewyS2VY8Cj3tKgSckq8ECkjd1HKHvBRnWhiHqRQsWfFC1
                    hex!["a639b507ee1585e0b6498ff141d6153960794523226866d1b44eba3f25f36356"]
                        .unchecked_into(),
                ),
                (
                    // 5G9VGb8ESBeS8Ca4or43RfhShzk9y7T5iTmxHk5RJsjZwsRx
                    hex!["b496c98a405ceab59b9e970e59ef61acd7765a19b704e02ab06c1cdfe171e40f"].into(),
                    // 5F7V9Y5FcxKXe1aroqvPeRiUmmeQwTFcL3u9rrPXcMuMiCNx
                    hex!["86d3a7571dd60139d297e55d8238d0c977b2e208c5af088f7f0136b565b0c103"].into(),
                    // 5GvuM53k1Z4nAB5zXJFgkRSHv4Bqo4BsvgbQWNWkiWZTMwWY
                    hex!["765e46067adac4d1fe6c783aa2070dfa64a19f84376659e12705d1734b3eae01"]
                        .unchecked_into(),
                    // 5HBDAaybNqjmY7ww8ZcZZY1L5LHxvpnyfqJwoB7HhR6raTmG
                    hex!["e2234d661bee4a04c38392c75d1566200aa9e6ae44dd98ee8765e4cc9af63cb7"]
                        .unchecked_into(),
                    // 5GvuM53k1Z4nAB5zXJFgkRSHv4Bqo4BsvgbQWNWkiWZTMwWY
                    hex!["765e46067adac4d1fe6c783aa2070dfa64a19f84376659e12705d1734b3eae01"]
                        .unchecked_into(),
                    // 5GvuM53k1Z4nAB5zXJFgkRSHv4Bqo4BsvgbQWNWkiWZTMwWY
                    hex!["765e46067adac4d1fe6c783aa2070dfa64a19f84376659e12705d1734b3eae01"]
                        .unchecked_into(),
                    // 5GvuM53k1Z4nAB5zXJFgkRSHv4Bqo4BsvgbQWNWkiWZTMwWY
                    hex!["765e46067adac4d1fe6c783aa2070dfa64a19f84376659e12705d1734b3eae01"]
                        .unchecked_into(),
                    // 5GvuM53k1Z4nAB5zXJFgkRSHv4Bqo4BsvgbQWNWkiWZTMwWY
                    hex!["765e46067adac4d1fe6c783aa2070dfa64a19f84376659e12705d1734b3eae01"]
                        .unchecked_into(),
                ),
                (
                    // 5FzwpgGvk2kk9agow6KsywLYcPzjYc8suKej2bne5G5b9YU3
                    hex!["ae12f70078a22882bf5135d134468f77301927aa67c376e8c55b7ff127ace115"].into(),
                    // 5EqoZhVC2BcsM4WjvZNidu2muKAbu5THQTBKe3EjvxXkdP7A
                    hex!["7addb914ec8486bbc60643d2647685dcc06373401fa80e09813b630c5831d54b"].into(),
                    // 5CXNq1mSKJT4Sc2CbyBBdANeSkbUvdWvE4czJjKXfBHi9sX5
                    hex!["664eae1ca4713dd6abf8c15e6c041820cda3c60df97dc476c2cbf7cb82cb2d2e"]
                        .unchecked_into(),
                    // 5E8ULLQrDAtWhfnVfZmX41Yux86zNAwVJYguWJZVWrJvdhBe
                    hex!["5b57ed1443c8967f461db1f6eb2ada24794d163a668f1cf9d9ce3235dfad8799"]
                        .unchecked_into(),
                    // 5CXNq1mSKJT4Sc2CbyBBdANeSkbUvdWvE4czJjKXfBHi9sX5
                    hex!["664eae1ca4713dd6abf8c15e6c041820cda3c60df97dc476c2cbf7cb82cb2d2e"]
                        .unchecked_into(),
                    // 5CXNq1mSKJT4Sc2CbyBBdANeSkbUvdWvE4czJjKXfBHi9sX5
                    hex!["664eae1ca4713dd6abf8c15e6c041820cda3c60df97dc476c2cbf7cb82cb2d2e"]
                        .unchecked_into(),
                    // 5CXNq1mSKJT4Sc2CbyBBdANeSkbUvdWvE4czJjKXfBHi9sX5
                    hex!["664eae1ca4713dd6abf8c15e6c041820cda3c60df97dc476c2cbf7cb82cb2d2e"]
                        .unchecked_into(),
                    // 5CXNq1mSKJT4Sc2CbyBBdANeSkbUvdWvE4czJjKXfBHi9sX5
                    hex!["664eae1ca4713dd6abf8c15e6c041820cda3c60df97dc476c2cbf7cb82cb2d2e"]
                        .unchecked_into(),
                ),
                (
                    // 5CFj6Kg9rmVn1vrqpyjau2ztyBzKeVdRKwNPiA3tqhB5HPqq
                    hex!["0867dbb49721126df589db100dda728dc3b475cbf414dad8f72a1d5e84897252"].into(),
                    // 5CwQXP6nvWzigFqNhh2jvCaW9zWVzkdveCJY3tz2MhXMjTon
                    hex!["26ab2b4b2eba2263b1e55ceb48f687bb0018130a88df0712fbdaf6a347d50e2a"].into(),
                    // 5FCd9Y7RLNyxz5wnCAErfsLbXGG34L2BaZRHzhiJcMUMd5zd
                    hex!["2adb17a5cafbddc7c3e00ec45b6951a8b12ce2264235b4def342513a767e5d3d"]
                        .unchecked_into(),
                    // 5HGLmrZsiTFTPp3QoS1W8w9NxByt8PVq79reqvdxNcQkByqK
                    hex!["e60d23f49e93c1c1f2d7c115957df5bbd7faf5ebf138d1e9d02e8b39a1f63df0"]
                        .unchecked_into(),
                    // 5FCd9Y7RLNyxz5wnCAErfsLbXGG34L2BaZRHzhiJcMUMd5zd
                    hex!["2adb17a5cafbddc7c3e00ec45b6951a8b12ce2264235b4def342513a767e5d3d"]
                        .unchecked_into(),
                    // 5FCd9Y7RLNyxz5wnCAErfsLbXGG34L2BaZRHzhiJcMUMd5zd
                    hex!["2adb17a5cafbddc7c3e00ec45b6951a8b12ce2264235b4def342513a767e5d3d"]
                        .unchecked_into(),
                    // 5FCd9Y7RLNyxz5wnCAErfsLbXGG34L2BaZRHzhiJcMUMd5zd
                    hex!["2adb17a5cafbddc7c3e00ec45b6951a8b12ce2264235b4def342513a767e5d3d"]
                        .unchecked_into(),
                    // 5FCd9Y7RLNyxz5wnCAErfsLbXGG34L2BaZRHzhiJcMUMd5zd
                    hex!["2adb17a5cafbddc7c3e00ec45b6951a8b12ce2264235b4def342513a767e5d3d"]
                        .unchecked_into(),
                ),
            ];

            let init_data = InitializationData {
                header: Box::new(block.block.header),
                authority_list: initial_authorities,
                set_id: 0,
                is_halted: false,
            };

            // pallet_bridge_grandpa::Pallet::<T>::initialize(origin, init_data);

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
