//! Data structures shared between node and node runner, facilitating exchange and creation of
//! queries, subscriptions, various events and shared information.

use crate::multimess;
use crate::pieces_by_range_handler::PiecesByRangeRequest;
use crate::request_responses::RequestFailure;
use bytes::Bytes;
use core::panic;
use event_listener_primitives::Bag;
use futures::channel::{mpsc, oneshot};
use generic_array::GenericArray;
use libp2p::core::multihash::Multihash;
use libp2p::gossipsub::error::{PublishError, SubscriptionError};
use libp2p::gossipsub::Sha256Topic;
use libp2p::kad::kbucket::{KeyBytes, PreimageIntoKeyBytes};
use libp2p::kad::record;
use libp2p::multihash::{Code, MultihashDigest};
use libp2p::{identity, Multiaddr, PeerId};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use typenum::U32;

#[derive(Debug)]
pub(crate) struct CreatedSubscription {
    /// Subscription ID to be used for unsubscribing.
    pub(crate) subscription_id: usize,
    /// Receiver side of the channel with new messages.
    pub(crate) receiver: mpsc::UnboundedReceiver<Bytes>,
}

#[derive(Debug)]
pub(crate) enum Command {
    GetValue {
        key: Multihash,
        result_sender: oneshot::Sender<Option<Vec<u8>>>,
    },
    Subscribe {
        topic: Sha256Topic,
        result_sender: oneshot::Sender<Result<CreatedSubscription, SubscriptionError>>,
    },
    Unsubscribe {
        topic: Sha256Topic,
        subscription_id: usize,
    },
    Publish {
        topic: Sha256Topic,
        message: Vec<u8>,
        result_sender: oneshot::Sender<Result<(), PublishError>>,
    },
    GetClosestPeers {
        key: Multihash,
        result_sender: oneshot::Sender<Vec<PeerId>>,
    },
    PiecesByRangeRequest {
        peer_id: PeerId,
        request: PiecesByRangeRequest,
        result_sender: oneshot::Sender<Result<Vec<u8>, RequestFailure>>,
    },
}

#[derive(Default, Debug)]
pub(crate) struct Handlers {
    pub(crate) new_listener: Bag<Arc<dyn Fn(&Multiaddr) + Send + Sync + 'static>, Multiaddr>,
}

#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) handlers: Handlers,
    pub(crate) id: PeerId,
    /// Addresses on which node is listening for incoming requests.
    pub(crate) listeners: Mutex<Vec<Multiaddr>>,
    pub(crate) connected_peers_count: AtomicUsize,
    /// Sender end of the channel for sending commands to the swarm.
    pub(crate) command_sender: mpsc::Sender<Command>,
}

impl Shared {
    pub(crate) fn new(id: PeerId, command_sender: mpsc::Sender<Command>) -> Self {
        Self {
            handlers: Handlers::default(),
            id,
            listeners: Mutex::default(),
            connected_peers_count: AtomicUsize::new(0),
            command_sender,
        }
    }
}

// Supports exact keys for Kademlia. It uses the Multihash as the canonical type
// and reduces all the conversions to Multihash conversion.
// It supports only Sr25519 key pairs for PeerId and Identity multihashes.
#[derive(Clone, Debug)]
pub struct IdendityHash(GenericArray<u8, U32>);

impl PreimageIntoKeyBytes<Multihash> for IdendityHash {
    fn preimage_into_key_bytes(multihash: &Multihash) -> KeyBytes<Self> {
        const SHA256_HASH_SIZE: usize = 32;

        // Identity hasher for 32-bytes digest
        if multihash.code() == u64::from(Code::Identity)
            && multihash.digest().len() == SHA256_HASH_SIZE
        {
            let array = GenericArray::from_slice(multihash.digest());
            return KeyBytes::from_unchecked(*array);
        }

        // PieceIndex and Piece multihashes are hashed with SHA256
        if multihash.code() == u64::from(multimess::MultihashCode::Piece)
            || multihash.code() == u64::from(multimess::MultihashCode::PieceIndex)
        {
            return KeyBytes::from_unchecked(Sha256::digest(multihash.to_bytes()));
        }

        panic!(
            "Unsupported multihash type. Expected Identity:{:?}",
            multihash
        )
    }
}

impl PreimageIntoKeyBytes<PeerId> for IdendityHash {
    fn preimage_into_key_bytes(peer_id: &PeerId) -> KeyBytes<Self> {
        let multihash: Multihash = if let Ok(public_key) =
            identity::PublicKey::from_protobuf_encoding(peer_id.as_ref().digest())
        {
            if let identity::PublicKey::Sr25519(pk) = public_key {
                Code::Identity.digest(&pk.encode())
            } else {
                panic!("Unsupported public key type. Expected Sr25519")
            }
        } else {
            // No protobuf encoding: it's PeerId::random()
            *peer_id.as_ref()
        };

        PreimageIntoKeyBytes::<Multihash>::preimage_into_key_bytes(&multihash)
    }
}

impl PreimageIntoKeyBytes<record::Key> for IdendityHash {
    fn preimage_into_key_bytes(record_key: &record::Key) -> KeyBytes<Self> {
        let multihash = Multihash::from_bytes(&record_key.to_vec())
            .expect("Not multihash record keys not supported.");

        PreimageIntoKeyBytes::<Multihash>::preimage_into_key_bytes(&multihash)
    }
}

impl PreimageIntoKeyBytes<Vec<u8>> for IdendityHash {
    fn preimage_into_key_bytes(bytes: &Vec<u8>) -> KeyBytes<Self> {
        let multihash = Multihash::from_bytes(bytes).expect("Not multihash arrays not supported");

        PreimageIntoKeyBytes::<Multihash>::preimage_into_key_bytes(&multihash)
    }
}
