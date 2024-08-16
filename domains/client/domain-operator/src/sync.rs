use domain_runtime_primitives::Balance;
use sc_client_api::ProofProvider;
use sc_consensus::ImportedState;
use sc_network::{NetworkRequest, PeerId};
use sc_network_sync::SyncingService;
use sp_blockchain::HeaderBackend;
use sp_domains::{DomainId, ExecutionReceiptFor};
use sp_runtime::traits::{Block as BlockT, Header as HeaderT, Header};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use subspace_service::domains::{get_last_confirmed_domain_block_receipt, LastDomainBlockReceiptProvider};
use subspace_service::sync_from_dsn::snap_sync_engine::SnapSyncingEngine;
use subspace_service::sync_from_dsn::synchronizer::Synchronizer;
use tokio::time::sleep;

//TODO
pub(crate) async fn get_header<Block, Client, NR>(
    client: &Arc<Client>,
    fork_id: Option<&str>,
    network_request: &NR,
    sync_service: &SyncingService<Block>,
) -> Result<Block::Header, sp_blockchain::Error>
where
    Block: BlockT,
    Client: HeaderBackend<Block> + ProofProvider<Block> + Send + Sync + 'static,
    NR: NetworkRequest,
{
    let header = client.header(Default::default()).unwrap().unwrap();

    Ok(header)
}

pub(crate) async fn get_last_confirmed_execution_receipt<Block, Client, NR>(
    client: &Arc<Client>,
    fork_id: Option<&str>,
    network_request: &NR,
    sync_service: &SyncingService<Block>,
) -> Result<ExecutionReceiptFor<Block::Header, Block, Balance>, sp_blockchain::Error>
where
    Block: BlockT,
    Client: HeaderBackend<Block> + ProofProvider<Block> + Send + Sync + 'static,
    NR: NetworkRequest,
{
    let domain_id = DomainId::new(0); // TODO:
    let receipt = get_last_confirmed_domain_block_receipt::<Block, Client, NR, Block::Header>(
        domain_id,
        fork_id.map(|str| str.to_string()),
        client.clone(),
        network_request,
        sync_service,
    )
    .await;

    println!("Execution receipt: {receipt:?}");

    Ok(receipt.unwrap()) // TODO:
}

pub(crate) async fn sync<Block, Client, NR, CBlock>(
    client: &Arc<Client>,
    fork_id: Option<&str>,
    network_request: &NR,
    sync_service: &SyncingService<Block>,
    synchronizer: Arc<Synchronizer>,
    execution_receipt_provider: Box<dyn LastDomainBlockReceiptProvider<CBlock>>
) -> Result<ImportedState<Block>, sp_blockchain::Error>
where
    Block: BlockT,
    Client: HeaderBackend<Block> + ProofProvider<Block> + Send + Sync + 'static,
    NR: NetworkRequest,
    CBlock: BlockT,
{
    //let domain_block_header = get_header(client, fork_id, network_request, sync_service).await?;
    // let last_confirmed_block_receipt = get_last_confirmed_execution_receipt(
    //     &client,
    //     fork_id.clone(),
    //     network_request,
    //     sync_service,
    // )
    // .await
    // .unwrap(); // TODO:

    let execution_receipt_result = execution_receipt_provider.get_execution_receipt().await;

    println!("execution_receipt_resul: {:?}", execution_receipt_result);

    let last_confirmed_block_receipt = execution_receipt_result.unwrap();

    let block_number: u32 = match last_confirmed_block_receipt
        .consensus_block_number
        .try_into()
    {
        Ok(block_number) => block_number,
        Err(_) => {
            panic!("Can't convert block number.")
        }
    };
    synchronizer.allow_snap_sync(block_number);

    let domain_block_header = get_header(client, fork_id, network_request, sync_service).await?;

    let result = download_state(
        &domain_block_header,
        client,
        fork_id,
        network_request,
        sync_service,
    )
    .await;

    println!("State downloaded: {:?}", result);

    result
}

/// Download and return state for specified block
async fn download_state<Block, Client, NR>(
    header: &Block::Header,
    client: &Arc<Client>,
    fork_id: Option<&str>,
    network_request: &NR,
    sync_service: &SyncingService<Block>,
) -> Result<ImportedState<Block>, sp_blockchain::Error>
where
    Block: BlockT,
    Client: HeaderBackend<Block> + ProofProvider<Block> + Send + Sync + 'static,
    NR: NetworkRequest,
{
    let block_number = *header.number();

    const STATE_SYNC_RETRIES: u32 = 5;
    const LOOP_PAUSE: Duration = Duration::from_secs(20);

    for attempt in 1..=STATE_SYNC_RETRIES {
        tracing::debug!(%attempt, "Starting state sync...");

        tracing::debug!("Gathering peers for state sync.");
        let mut tried_peers = HashSet::<PeerId>::new();

        // TODO: add loop timeout
        let current_peer_id = loop {
            let connected_full_peers = sync_service
                .peers_info()
                .await
                .expect("Network service must be available.")
                .iter()
                .filter_map(|(peer_id, info)| {
                    (info.roles.is_full() && info.best_number > block_number).then_some(*peer_id)
                })
                .collect::<Vec<_>>();

            tracing::debug!(?tried_peers, "Sync peers: {}", connected_full_peers.len());

            let active_peers_set = HashSet::from_iter(connected_full_peers.into_iter());

            if let Some(peer_id) = active_peers_set.difference(&tried_peers).next().cloned() {
                break peer_id;
            }

            sleep(LOOP_PAUSE).await;
        };

        tried_peers.insert(current_peer_id);

        let sync_engine = SnapSyncingEngine::<Block, NR>::new(
            client.clone(),
            fork_id,
            header.clone(),
            false,
            (current_peer_id, block_number),
            network_request,
        )?;

        let last_block_from_sync_result = sync_engine.download_state().await;

        match last_block_from_sync_result {
            Ok(block_to_import) => {
                tracing::debug!("Sync worker handle result: {:?}", block_to_import);

                return block_to_import.state.ok_or_else(|| {
                    sp_blockchain::Error::Backend(
                        "Imported state was missing in synced block".into(),
                    )
                });
            }
            Err(error) => {
                tracing::error!(%error, "State sync error");
                continue;
            }
        }
    }

    Err(sp_blockchain::Error::Backend(
        "All snap sync retries failed".into(),
    ))
}

pub struct SyncParams<DomainClient, NR: Send, Block: BlockT> {
    pub domain_client: Arc<DomainClient>,
    pub sync_service: Arc<SyncingService<Block>>,
    pub network_request: NR,
}
