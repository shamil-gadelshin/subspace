use async_trait::async_trait;
pub use db::DbNetworkingParametersProvider;
use futures::future::Fuse;
use futures::FutureExt;
pub use json::JsonNetworkingParametersProvider;
use libp2p::multiaddr::Protocol;
use libp2p::{Multiaddr, PeerId};
use lru::LruCache;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::time::{sleep, Sleep};
use tracing::trace;

// Size of the LRU cache for peers.
const PEER_CACHE_SIZE: usize = 100;
// Pause duration between network parameters save.
const DATA_FLUSH_DURATION_SECS: u64 = 5;

/// An alias for the reference to a `NetworkingParametersProvider` implementation.
pub type NetworkingParametersHandler =
    Arc<dyn NetworkingParametersProvider + Send + Sync + 'static>;

/// Networking parameters container.
#[derive(Debug)]
pub struct NetworkingParameters {
    /// LRU cache for the known peers and their addresses
    pub known_peers: LruCache<PeerId, HashSet<Multiaddr>>,
}

impl Clone for NetworkingParameters {
    fn clone(&self) -> Self {
        let mut known_peers = LruCache::new(self.known_peers.cap());

        for (peer_id, addresses) in self.known_peers.iter() {
            known_peers.push(*peer_id, addresses.clone());
        }

        Self { known_peers }
    }
}

impl NetworkingParameters {
    /// A type constructor. Cache cap defines LRU cache size for known peers.
    fn new(cache_cap: usize) -> Self {
        Self {
            known_peers: LruCache::new(cache_cap),
        }
    }

    /// Add peer with its addresses to the cache.
    fn add_known_peer(&mut self, peer_id: PeerId, addr_set: HashSet<Multiaddr>) {
        if let Some(addresses) = self.known_peers.get_mut(&peer_id) {
            *addresses = addresses.union(&addr_set).cloned().collect()
        } else {
            self.known_peers.push(peer_id, addr_set);
        }
    }

    /// Returns a collection of peer ID and address from the cache.
    fn get_known_peer_addresses(&self, peer_number: usize) -> Vec<(PeerId, Multiaddr)> {
        self.known_peers
            .iter()
            .take(peer_number)
            .flat_map(|(peer_id, addresses)| addresses.iter().map(|addr| (*peer_id, addr.clone())))
            .collect()
    }
}

/// Defines operations with the networking parameters.
#[async_trait]
pub trait NetworkingParametersRegistry: Send {
    /// Registers a peer ID and associated addresses
    async fn add_known_peer(&mut self, peer_id: PeerId, addresses: Vec<Multiaddr>);

    /// Returns known addresses from networking parameters DB. It removes p2p-protocol suffix.
    /// Peer number parameter limits peers to retrieve.
    async fn known_addresses(&self, peer_number: usize) -> Vec<(PeerId, Multiaddr)>;

    /// Drive async work in the persistence provider
    async fn run(&mut self);

    //TODO:
    fn clone_box(&self) -> Box<dyn NetworkingParametersRegistry>;
}

/// Defines networking parameters persistence operations
pub trait NetworkingParametersProvider: Send {
    /// Loads networking parameters from the underlying DB implementation.
    fn load(&self) -> Result<NetworkingParameters, NetworkParametersPersistenceError>;

    /// Saves networking to the underlying DB implementation.
    fn save(&self, params: &NetworkingParameters) -> Result<(), NetworkParametersPersistenceError>;
}

/// Handles networking parameters. It manages network parameters set and its persistence.
pub struct NetworkingParametersManager {
    // Persistence provider for the networking parameters.
    network_parameters_persistence_handler: NetworkingParametersHandler,
    // Networking parameters working cache.
    networking_params: NetworkingParameters,
    // Period between networking parameters saves.
    networking_parameters_save_delay: Pin<Box<Fuse<Sleep>>>,
}

impl NetworkingParametersManager {
    /// Object constructor. It accepts `NetworkingParametersProvider` implementation as a parameter.
    /// On object creation it starts a job for networking parameters cache handling.
    pub fn new(
        network_parameters_persistence_handler: NetworkingParametersHandler,
    ) -> NetworkingParametersManager {
        let networking_params = network_parameters_persistence_handler
            .load()
            .unwrap_or_else(|_| NetworkingParameters::new(PEER_CACHE_SIZE));

        NetworkingParametersManager {
            network_parameters_persistence_handler,
            networking_params,
            networking_parameters_save_delay: Self::default_delay(),
        }
    }

    //TODO: add comment
    pub fn boxed(self) -> Box<dyn NetworkingParametersRegistry> {
        Box::new(self)
    }

    // Create default delay for networking parameters.
    fn default_delay() -> Pin<Box<Fuse<Sleep>>> {
        Box::pin(sleep(Duration::from_secs(DATA_FLUSH_DURATION_SECS)).fuse())
    }
}

impl Clone for Box<dyn NetworkingParametersRegistry> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

#[async_trait]
impl NetworkingParametersRegistry for NetworkingParametersManager {
    async fn add_known_peer(&mut self, peer_id: PeerId, addresses: Vec<Multiaddr>) {
        let addr_set = addresses.iter().cloned().collect::<HashSet<_>>();

        self.networking_params.add_known_peer(peer_id, addr_set);
    }

    async fn known_addresses(&self, peer_number: usize) -> Vec<(PeerId, Multiaddr)> {
        self.networking_params
            .get_known_peer_addresses(peer_number)
            .into_iter()
            .map(|(peer_id, addr)| {
                // remove p2p-protocol suffix if any
                let mut modified_address = addr.clone();

                if let Some(Protocol::P2p(_)) = modified_address.pop() {
                    (peer_id, modified_address)
                } else {
                    (peer_id, addr)
                }
            })
            .collect()
    }

    async fn run(&mut self) {
        (&mut self.networking_parameters_save_delay).await;

        if let Err(err) = self
            .network_parameters_persistence_handler
            .save(&self.networking_params.clone())
        {
            trace!(error=%err, "Error on saving network parameters");
        }
        self.networking_parameters_save_delay = NetworkingParametersManager::default_delay();
    }

    fn clone_box(&self) -> Box<dyn NetworkingParametersRegistry> {
        NetworkingParametersManager {
            network_parameters_persistence_handler: self
                .network_parameters_persistence_handler
                .clone(),
            networking_params: self.networking_params.clone(),
            networking_parameters_save_delay: Self::default_delay(),
        }
        .boxed()
    }
}

// Helper struct for NetworkingPersistence implementations (data transfer object).
#[derive(Default, Debug, Serialize, Deserialize)]
struct NetworkingParametersDto {
    pub known_peers: HashMap<PeerId, HashSet<Multiaddr>>,
}

impl From<&NetworkingParameters> for NetworkingParametersDto {
    fn from(cache: &NetworkingParameters) -> Self {
        Self {
            known_peers: cache
                .known_peers
                .iter()
                .map(|(peer_id, addresses)| (*peer_id, addresses.clone()))
                .collect(),
        }
    }
}

impl From<NetworkingParametersDto> for NetworkingParameters {
    fn from(params: NetworkingParametersDto) -> Self {
        let mut known_peers = LruCache::<PeerId, HashSet<Multiaddr>>::new(PEER_CACHE_SIZE);

        for (peer_id, addresses) in params.known_peers.iter() {
            known_peers.push(*peer_id, addresses.clone());
        }

        Self { known_peers }
    }
}

mod json {
    use super::{NetworkingParameters, NetworkingParametersDto, NetworkingParametersProvider};
    use crate::behavior::persistent_parameters::NetworkParametersPersistenceError;
    use std::fs;
    use std::path::PathBuf;
    use tracing::trace;

    /// JSON implementation for the networking parameters provider.
    #[derive(Clone)]
    pub struct JsonNetworkingParametersProvider {
        /// JSON file path
        path: PathBuf,
    }

    impl JsonNetworkingParametersProvider {
        /// Constructor
        pub fn new(path: PathBuf) -> Self {
            trace!(?path, "JSON networking parameters created.");

            JsonNetworkingParametersProvider { path }
        }
    }

    impl NetworkingParametersProvider for JsonNetworkingParametersProvider {
        fn load(&self) -> Result<NetworkingParameters, NetworkParametersPersistenceError> {
            let data = fs::read(&self.path)?;

            let result: NetworkingParametersDto = serde_json::from_slice(&data)?;

            trace!("Networking parameters loaded from JSON file");

            Ok(result.into())
        }

        fn save(
            &self,
            params: &NetworkingParameters,
        ) -> Result<(), NetworkParametersPersistenceError> {
            let params: NetworkingParametersDto = params.into();
            let data = serde_json::to_string(&params)?;

            fs::write(&self.path, data)?;

            trace!("Networking parameters saved to JSON file");

            Ok(())
        }
    }
}

mod db {
    use super::{NetworkingParameters, NetworkingParametersProvider};
    use crate::behavior::persistent_parameters::{
        NetworkParametersPersistenceError, NetworkingParametersDto, PEER_CACHE_SIZE,
    };
    use parity_db::{Db, Options};
    use std::path::Path;
    use std::sync::Arc;
    use tracing::trace;

    /// Parity DB implementation for the networking parameters provider.
    #[derive(Clone)]
    pub struct DbNetworkingParametersProvider {
        // Parity DB instance
        db: Arc<Db>,
        // Column ID to persist parameters
        column_id: u8,
        // Key to persistent parameters
        object_id: &'static [u8],
    }

    impl DbNetworkingParametersProvider {
        /// Opens or creates a new object mappings database
        pub fn new(path: &Path) -> Result<Self, NetworkParametersPersistenceError> {
            trace!(?path, "Networking parameters DB created.");

            let mut options = Options::with_columns(path, 1);
            // We don't use stats
            options.stats = false;

            let db = Db::open_or_create(&options)?;

            Ok(Self {
                db: Arc::new(db),
                column_id: 0u8,
                object_id: b"global_networking_parameters_key",
            })
        }
    }

    impl NetworkingParametersProvider for DbNetworkingParametersProvider {
        fn load(&self) -> Result<NetworkingParameters, NetworkParametersPersistenceError> {
            let result = self
                .db
                .get(self.column_id, self.object_id)?
                .map(|data| {
                    let result = serde_json::from_slice::<NetworkingParametersDto>(&data)
                        .map(NetworkingParameters::from);

                    if result.is_ok() {
                        trace!("Networking parameters loaded from DB");
                    }

                    result
                })
                .unwrap_or_else(|| Ok(NetworkingParameters::new(PEER_CACHE_SIZE)))?;

            Ok(result)
        }

        fn save(
            &self,
            params: &NetworkingParameters,
        ) -> Result<(), NetworkParametersPersistenceError> {
            let dto: NetworkingParametersDto = params.into();
            let data = serde_json::to_vec(&dto)?;

            let tx = vec![(self.column_id, self.object_id, Some(data))];
            self.db.commit(tx)?;

            trace!("Networking parameters saved to DB");

            Ok(())
        }
    }
}

/// The default implementation for networking provider stub. It doesn't save or load anything and
/// only logs attempts.
#[derive(Clone)]
pub struct NetworkingParametersProviderStub;

impl NetworkingParametersProvider for NetworkingParametersProviderStub {
    fn load(&self) -> Result<NetworkingParameters, NetworkParametersPersistenceError> {
        trace!("Default network parameters used");

        Ok(NetworkingParameters::new(PEER_CACHE_SIZE))
    }

    fn save(&self, _: &NetworkingParameters) -> Result<(), NetworkParametersPersistenceError> {
        trace!("Network parameters saving skipped.");

        Ok(())
    }
}

/// The default implementation for networking manager stub. All operations are muted.
#[derive(Clone)]
pub struct NetworkingParametersRegistryStub;

#[async_trait]
impl NetworkingParametersRegistry for NetworkingParametersRegistryStub {
    async fn add_known_peer(&mut self, _: PeerId, _: Vec<Multiaddr>) {}

    async fn known_addresses(&self, _: usize) -> Vec<(PeerId, Multiaddr)> {
        Vec::new()
    }

    async fn run(&mut self) {}

    fn clone_box(&self) -> Box<dyn NetworkingParametersRegistry> {
        Box::new(self.clone())
    }
}

#[derive(Debug, Error)]
pub enum NetworkParametersPersistenceError {
    #[error("DB error: {0}")]
    Db(#[from] parity_db::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialization error: {0}")]
    JsonSerialization(#[from] serde_json::Error),
}
