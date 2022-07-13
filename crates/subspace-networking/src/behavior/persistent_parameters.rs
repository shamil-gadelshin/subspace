use async_trait::async_trait;
use futures::channel::mpsc::{self, UnboundedSender};
use futures::{select, FutureExt, SinkExt, StreamExt};
use libp2p::{Multiaddr, PeerId};
use lru::LruCache;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::marker::PhantomData;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::sleep;

//TODO:
//pub type ValueGetter = Arc<dyn (Fn(&Multihash) -> Option<Vec<u8>>) + Send + Sync + 'static>;
#[derive(Default, Debug, Serialize, Deserialize)]
pub struct NetworkingParameters {
    pub known_peers: HashMap<PeerId, HashSet<Multiaddr>>,
}

//TODO:
//pub type ValueGetter = Arc<dyn (Fn(&Multihash) -> Option<Vec<u8>>) + Send + Sync + 'static>;
#[derive(Debug)]
pub struct NetworkingParametersCache {
    pub known_peers: LruCache<PeerId, HashSet<Multiaddr>>,
}

impl Clone for NetworkingParametersCache {
    fn clone(&self) -> Self {
        let mut known_peers = LruCache::new(self.known_peers.cap());

        for (peer_id, addresses) in self.known_peers.iter() {
            known_peers.push(*peer_id, addresses.clone());
        }

        Self { known_peers }
    }
}

impl NetworkingParametersCache {
    fn add_known_peer(&mut self, peer_id: PeerId, addr_set: HashSet<Multiaddr>) {
        if let Some(addresses) = self.known_peers.get_mut(&peer_id) {
            *addresses = addresses.union(&addr_set).cloned().collect()
        } else {
            self.known_peers.push(peer_id, addr_set);
        }
    }

    fn get_known_peer_addresses(&self, peer_number: usize) -> Vec<(PeerId, Multiaddr)> {
        self.known_peers
            .iter()
            .take(peer_number)
            .map(|(peer_id, addresses)| addresses.iter().map(|addr| (*peer_id, addr.clone())))
            .flatten()
            .collect()
    }
}

//TODO: change to impl methods to avoid duplication
impl From<NetworkingParametersCache> for NetworkingParameters {
    fn from(cache: NetworkingParametersCache) -> Self {
        Self {
            known_peers: cache
                .known_peers
                .iter()
                .map(|(peer_id, addresses)| (*peer_id, addresses.clone()))
                .collect(),
        }
    }
}

//TODO: change to impl methods to avoid duplication
impl From<NetworkingParameters> for NetworkingParametersCache {
    fn from(params: NetworkingParameters) -> Self {
        let mut known_peers = LruCache::<PeerId, HashSet<Multiaddr>>::new(1000); // TODO

        for (peer_id, addresses) in params.known_peers.iter() {
            known_peers.push(*peer_id, addresses.clone());
        }

        Self {
            known_peers, //TODO: iter?
        }
    }
}

#[async_trait]
pub trait NetworkingParametersManager {
    async fn add_known_peer(&mut self, peer_id: PeerId, addresses: Vec<Multiaddr>);
}

// TODO: Result?, save-load params?
pub trait PersistentNetworkingParametersManager {
    fn load(path: &String) -> NetworkingParameters;
    fn save(path: &String, params: &NetworkingParameters);
}

pub struct NetworkingDataManager<
    P: PersistentNetworkingParametersManager = JsonNetworkingPersistence
> {
    stop_handle: JoinHandle<()>,
    tx: UnboundedSender<(PeerId, HashSet<Multiaddr>)>, //TODO
    initial_bootstrap_addresses: Vec<(PeerId, Multiaddr)>,
    _persistence_marker: PhantomData<P>,
}

impl<P: PersistentNetworkingParametersManager> Drop for NetworkingDataManager<P> {
    fn drop(&mut self) {
        self.stop_handle.abort();
    }
}

impl<P: PersistentNetworkingParametersManager> NetworkingDataManager<P> {
    pub fn new(networking_data_path: Option<String>) -> NetworkingDataManager<P> {
        let (tx, mut rx) = mpsc::unbounded();

        let networking_params = networking_data_path.as_ref().map(|ref path| P::load(path)).unwrap_or_default();
        let initial_cache: NetworkingParametersCache = networking_params.into();

        const INITIAL_BOOTSTRAP_ADDRESS_NUMBER: usize = 100; //TODO
        let delay_duration = Duration::from_secs(5); //TODO

        let initial_bootstrap_addresses =
            initial_cache.get_known_peer_addresses(INITIAL_BOOTSTRAP_ADDRESS_NUMBER);

        let stop_handle = tokio::spawn(async move {
            let mut params_cache = initial_cache;
            let mut delay = sleep(delay_duration).boxed().fuse();
            loop {
                select! {
                    _ = delay => {
                        if let Some(ref path) = networking_data_path.clone(){
                            P::save(path, &params_cache.clone().into());
                        }
                        // restart the delay future
                        delay = sleep(delay_duration).boxed().fuse();
                    },
                    data = rx.next() => {
                        if let Some((peer_id, addr_set)) = data {
                            println!("New data: {:?}", (peer_id, &addr_set));

                            params_cache.add_known_peer(peer_id, addr_set); //TODO

                            println!("Known peers so far: {:?}", params_cache);
                        }
                    }
                }
            }
        });

        NetworkingDataManager {
            tx,
            stop_handle,
            initial_bootstrap_addresses,
            _persistence_marker: PhantomData,
        }
    }

    //TODO: comment, convert from P2p-address ??
    pub fn initial_bootstrap_addresses(&self) -> Vec<(PeerId, Multiaddr)> {
        self.initial_bootstrap_addresses.clone()
    }
}

#[async_trait]
impl<P: PersistentNetworkingParametersManager + Send> NetworkingParametersManager
    for NetworkingDataManager<P>
{
    async fn add_known_peer(&mut self, peer_id: PeerId, addresses: Vec<Multiaddr>) {
        let addr_set = addresses.iter().cloned().collect::<HashSet<_>>();

        let _ = self.tx.send((peer_id, addr_set)).await; // TODO
    }
}

//TODO: empty saver, result errors, parameters?
pub struct JsonNetworkingPersistence;
impl PersistentNetworkingParametersManager for JsonNetworkingPersistence {
    fn load(path: &String) -> NetworkingParameters {
        let data = fs::read(path).expect("Unable to read file"); //TODO

        let result =
            serde_json::from_slice(&data).expect("Cannot serialize networking parameters to JSON"); //TODO

        println!("Networking parameters loaded");

        result
    }

    fn save(path: &String, params: &NetworkingParameters) {
        //TODO
        //let addresses = params.known_peers.iter().map(|(_, addr)|addr).cloned().flatten().collect();

        //let params = NetworkingParameters{bootstrap_nodes: addresses};
        let data =
            serde_json::to_string(&params).expect("Cannot serialize networking parameters to JSON"); //TODO

        fs::write(path, data).expect("Unable to write file"); //TODO
        println!("Networking parameters saved");
    }
}
