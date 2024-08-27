use domain_runtime_primitives::{Balance, BlockNumber};
use sc_client_api::ProofProvider;
use sc_consensus::{BlockImport, BlockImportParams, ForkChoiceStrategy, ImportedState, StateAction, StorageChanges};
use sc_network::{NetworkRequest, PeerId, ProtocolName, RequestFailure};
use sc_network_sync::SyncingService;
use sp_blockchain::HeaderBackend;
use sp_domains::{DomainId, ExecutionReceiptFor};
use sp_runtime::traits::{Block as BlockT, Header as HeaderT, Header, NumberFor};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use sc_consensus::import_queue::ImportQueueService;
use sc_network_sync::block_relay_protocol::BlockDownloader;
use sp_consensus::BlockOrigin;
use subspace_service::domains::{ LastDomainBlockReceiptProvider};
use subspace_service::sync_from_dsn::snap_sync_engine::SnapSyncingEngine;
use subspace_service::sync_from_dsn::synchronizer::Synchronizer;
use tokio::time::sleep;
use tracing::{debug, error, info};
use subspace_service::sync_from_dsn::wait_for_block_import;
use sc_network_common::sync::message::{BlockRequest, FromBlock, Direction, BlockAttributes, BlockData};

//TODO
pub(crate) async fn get_header<Block, Client, NR>(
    client: &Arc<Client>,
    fork_id: Option<&str>,
    network_request: &NR,
    sync_service: &SyncingService<Block>,
) -> Result<Block::Header, sp_blockchain::Error>
where
    Block: BlockT,
    Client: HeaderBackend<Block> +  Send + Sync + 'static,
    NR: NetworkRequest,
{
    let header = client.header(Default::default()).unwrap().unwrap();

    Ok(header)
}

// pub(crate) async fn get_last_confirmed_execution_receipt<Block, Client, NR>(
//     client: &Arc<Client>,
//     fork_id: Option<&str>,
//     network_request: &NR,
//     sync_service: &SyncingService<Block>,
// ) -> Result<ExecutionReceiptFor<Block::Header, Block, Balance>, sp_blockchain::Error>
// where
//     Block: BlockT,
//     Client: HeaderBackend<Block> + ProofProvider<Block> + Send + Sync + 'static,
//     NR: NetworkRequest,
// {
//     let domain_id = DomainId::new(0); // TODO:
//     let receipt = get_last_confirmed_domain_block_receipt::<Block, Client, NR, Block::Header>(
//         domain_id,
//         fork_id.map(|str| str.to_string()),
//         client.clone(),
//         network_request,
//         sync_service,
//     )
//     .await;
//
//     println!("Execution receipt: {receipt:?}");
//
//     Ok(receipt.unwrap()) // TODO:
// }

async fn get_last_confirmed_block<Block: BlockT>(block_downloader: Arc<dyn BlockDownloader<Block>>, sync_service: &SyncingService<Block>, block_number: BlockNumber)
    -> Result<BlockData<Block>, sp_blockchain::Error>

{
    const LAST_CONFIRMED_BLOCK_RETRIES: u32 = 5;
    const LOOP_PAUSE: Duration = Duration::from_secs(20);

    for attempt in 1..=LAST_CONFIRMED_BLOCK_RETRIES {
        info!(%attempt, "Starting last confirmed block request..."); // TODO:

        info!("Gathering peers for last confirmed block request.");  // TODO:
        let mut tried_peers = HashSet::<PeerId>::new();

        // TODO: add loop timeout
        let current_peer_id = loop {
            let connected_full_peers = sync_service
                .peers_info()
                .await
                .expect("Network service must be available.")
                .iter()
                .map(|(peer_id, info)| *peer_id)
                // TODO:
                // .filter_map(|(peer_id, info)| {
                //     (info.roles.is_full() && info.best_number > block_number.into()).then_some(*peer_id)
                // })
                .collect::<Vec<_>>();

            info!(?tried_peers, "Sync peers: {}", connected_full_peers.len()); // TODO

            let active_peers_set = HashSet::from_iter(connected_full_peers.into_iter());

            if let Some(peer_id) = active_peers_set.difference(&tried_peers).next().cloned() {
                break peer_id;
            }

            sleep(LOOP_PAUSE).await;
        };

        tried_peers.insert(current_peer_id);

        // TODO:
        let block_request = BlockRequest::<Block>{
            id: 0, // TODO:
            direction: Direction::Ascending,
            from: FromBlock::Number(block_number.into()),
            max: Some(1),
            fields: BlockAttributes::HEADER | BlockAttributes::JUSTIFICATION | BlockAttributes::BODY |
                BlockAttributes::RECEIPT | BlockAttributes::MESSAGE_QUEUE | BlockAttributes::INDEXED_BODY
        };
        let block_response_result = block_downloader.download_blocks(current_peer_id, block_request.clone()).await;

        match block_response_result {
            Ok(block_response_inner_result) => {
                info!("Sync worker handle result: {:?}", block_response_inner_result); // TODO:

                match block_response_inner_result{
                    Ok(data) => {
                        match block_downloader.block_response_into_blocks(&block_request, data.0){
                            Ok(mut blocks) => {
                                info!("Domain block parsing result: {:?}", blocks); // TODO:

                                return Ok(blocks.pop().unwrap()) // TODO:
                            }
                            Err(error) => {
                                error!(?error, "Domain block parsing error");
                                continue;
                            }
                        }
                    }
                    Err(error) => {
                        error!(?error, "Domain block sync error (inner)");
                        continue;
                    }
                }
            }
            Err(error) => {
                error!(?error, "Domain block sync error");
                continue;
            }
        }
    }

    Err(sp_blockchain::Error::IncompletePipeline) // TODO
}

fn convert_block_number<Block: BlockT>(block_number: NumberFor<Block>) -> u32{
    let block_number: u32 = match block_number
        .try_into()
    {
        Ok(block_number) => block_number,
        Err(_) => {
            panic!("Can't convert block number.")
        }
    };

    block_number
}

pub(crate) async fn sync<Block, Client, NR, CBlock, CClient>(
    client: &Arc<Client>,
    fork_id: Option<&str>,
    network_request: &NR,
    sync_service: &SyncingService<Block>,
    synchronizer: Arc<Synchronizer>,
    execution_receipt_provider: Box<dyn LastDomainBlockReceiptProvider<CBlock>>,
    consensus_client: Arc<CClient>,
    block_downloader: Arc<dyn BlockDownloader<Block>>,
    mut import_queue_service: Box<dyn ImportQueueService<Block>>,
) -> Result<(), sp_blockchain::Error>
where
    Block: BlockT,
    Client: HeaderBackend<Block>  + BlockImport<Block> + ProofProvider<Block> + Send + Sync + 'static,
    for<'a> &'a Client: BlockImport<Block>,
    NR: NetworkRequest,
    CBlock: BlockT,
    CClient: HeaderBackend<CBlock> + ProofProvider<CBlock>  + Send + Sync + 'static,
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

    let execution_receipt_result = execution_receipt_provider.get_execution_receipt(None).await;

    println!("execution_receipt_resul: {:?}", execution_receipt_result);

    let last_confirmed_block_receipt = execution_receipt_result.unwrap();

    let block_number = convert_block_number::<CBlock>(last_confirmed_block_receipt
        .consensus_block_number);
    synchronizer.allow_consensus_snap_sync(block_number); // TODO: combine workflow


//    return Err(sp_blockchain::Error::IncompletePipeline);

    synchronizer.domain_snap_sync_allowed().await;

    wait_for_block_import(consensus_client.as_ref(), block_number.into()).await;

    let domain_block_number = convert_block_number::<CBlock>(last_confirmed_block_receipt.domain_block_number);
    let domain_block = get_last_confirmed_block(block_downloader, sync_service, domain_block_number).await.unwrap();  // TODO:

    let domain_block_header = domain_block.header.clone().unwrap(); // TODO

    let state_result = download_state(
        &domain_block_header,
        &client,
        fork_id,
        network_request,
        sync_service,
    )
    .await;

    println!("State downloaded: {:?}", state_result);

    {
        let mut client = client.clone();
        // Import first block as finalized
        let mut block = BlockImportParams::new(BlockOrigin::NetworkInitialSync, domain_block_header);
        block.body = domain_block.body;
        block.justifications = domain_block.justifications;
        block.state_action = StateAction::ApplyChanges(StorageChanges::Import(state_result.unwrap())); // TODO:
        block.finalized = true;
        block.fork_choice = Some(ForkChoiceStrategy::Custom(true));
        // TODO: Simplify when https://github.com/paritytech/polkadot-sdk/pull/5339 is in our fork
        (&mut client.as_ref())
            .import_block(block)
            .await
            .map_err(|error| format!("Failed to import state block: {error}")).unwrap(); // TODO:
    }


    //    synchronizer.resuming_consensus_sync_allowed(); // TODO:

    panic!("Full stop!!!!!!!!");

    Ok(())
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
        info!(%attempt, "Starting state sync..."); // TODO:

        info!("Gathering peers for state sync.");// TODO:
        let mut tried_peers = HashSet::<PeerId>::new();

        // TODO: add loop timeout
        let current_peer_id = loop {
            let connected_full_peers = sync_service
                .peers_info()
                .await
                .expect("Network service must be available.")
                .iter()
                .map(|(peer_id, info)| {
                   *peer_id // TODO
                })
                // .filter_map(|(peer_id, info)| {
                //     (info.roles.is_full() && info.best_number > block_number).then_some(*peer_id)
                // })
                .collect::<Vec<_>>();

            info!(?tried_peers, "Sync peers: {}", connected_full_peers.len()); // TODO:

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
                info!("Sync worker handle result: {:?}", block_to_import); // TODO:

                return block_to_import.state.ok_or_else(|| {
                    sp_blockchain::Error::Backend(
                        "Imported state was missing in synced block".into(),
                    )
                });
            }
            Err(error) => {
                error!(%error, "State sync error");
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
