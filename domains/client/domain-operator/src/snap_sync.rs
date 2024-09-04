use domain_runtime_primitives::BlockNumber;
use sc_client_api::{AuxStore, ProofProvider};
use sc_consensus::{
    BlockImport, BlockImportParams, ForkChoiceStrategy, ImportedState, StateAction, StorageChanges,
};
use sc_network::{NetworkRequest, PeerId};
use sc_network_common::sync::message::{
    BlockAttributes, BlockData, BlockRequest, Direction, FromBlock,
};
use sc_network_sync::block_relay_protocol::BlockDownloader;
use sc_network_sync::SyncingService;
use sc_service::ClientExt;
use sp_blockchain::HeaderBackend;
use sp_consensus::BlockOrigin;
use sp_runtime::traits::{Block as BlockT, Header as HeaderT, NumberFor};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use subspace_service::domains::ConsensusChainSyncParams;
use subspace_service::sync_from_dsn::snap_sync_engine::SnapSyncingEngine;
use subspace_service::sync_from_dsn::{wait_for_block_import, wait_for_block_import_ext};
use tokio::time::sleep;
use tracing::{debug, error, info, trace};

pub struct SyncParams<DomainClient, CClient, NR, Block, CBlock>
where
    NR: Send,
    Block: BlockT,
    CBlock: BlockT,
{
    pub domain_client: Arc<DomainClient>,
    pub sync_service: Arc<SyncingService<Block>>,
    pub fork_id: Option<&'static str>,
    pub network_request: NR,
    pub consensus_client: Arc<CClient>,
    pub block_downloader: Arc<dyn BlockDownloader<Block>>,
    pub consensus_chain_sync_params: ConsensusChainSyncParams<Block, CBlock>,
}

async fn get_last_confirmed_block<Block: BlockT>(
    block_downloader: Arc<dyn BlockDownloader<Block>>,
    sync_service: &SyncingService<Block>,
    block_number: BlockNumber,
) -> Result<BlockData<Block>, sp_blockchain::Error> {
    const LAST_CONFIRMED_BLOCK_RETRIES: u32 = 5;
    const LOOP_PAUSE: Duration = Duration::from_secs(20);

    for attempt in 1..=LAST_CONFIRMED_BLOCK_RETRIES {
        debug!(%attempt, "Starting last confirmed block request...");

        debug!("Gathering peers for last confirmed block request.");
        let mut tried_peers = HashSet::<PeerId>::new();

        // TODO: add loop timeout
        let current_peer_id = loop {
            let connected_full_peers = sync_service
                .peers_info()
                .await
                .expect("Network service must be available.")
                .iter()
                .map(|(peer_id, _)| *peer_id)
                // TODO:
                // .filter_map(|(peer_id, info)| {
                //     (info.roles.is_full() && info.best_number > block_number.into()).then_some(*peer_id)
                // })
                .collect::<Vec<_>>();

            debug!(?tried_peers, "Sync peers: {}", connected_full_peers.len()); // TODO

            let active_peers_set = HashSet::from_iter(connected_full_peers.into_iter());

            if let Some(peer_id) = active_peers_set.difference(&tried_peers).next().cloned() {
                break peer_id;
            }

            sleep(LOOP_PAUSE).await;
        };

        tried_peers.insert(current_peer_id);

        // TODO:
        let block_request = BlockRequest::<Block> {
            id: 0, // TODO:
            direction: Direction::Ascending,
            from: FromBlock::Number(block_number.into()),
            max: Some(1),
            fields: BlockAttributes::HEADER
                | BlockAttributes::JUSTIFICATION
                | BlockAttributes::BODY
                | BlockAttributes::RECEIPT
                | BlockAttributes::MESSAGE_QUEUE
                | BlockAttributes::INDEXED_BODY,
        };
        let block_response_result = block_downloader
            .download_blocks(current_peer_id, block_request.clone())
            .await;

        match block_response_result {
            Ok(block_response_inner_result) => {
                trace!(
                    "Sync worker handle result: {:?}",
                    block_response_inner_result
                );

                match block_response_inner_result {
                    Ok(data) => {
                        match block_downloader.block_response_into_blocks(&block_request, data.0) {
                            Ok(mut blocks) => {
                                trace!("Domain block parsing result: {:?}", blocks); // TODO:

                                return Ok(blocks.pop().unwrap()); // TODO:
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

fn convert_block_number<Block: BlockT>(block_number: NumberFor<Block>) -> u32 {
    let block_number: u32 = match block_number.try_into() {
        Ok(block_number) => block_number,
        Err(_) => {
            panic!("Can't convert block number.")
        }
    };

    block_number
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn snap_sync<Block, Client, NR, CBlock, CClient, Backend>(
    sync_params: SyncParams<Client, CClient, NR, Block, CBlock>,
) -> Result<(), sp_blockchain::Error>
where
    Block: BlockT,
    Backend: sc_client_api::Backend<Block>,
    Client: HeaderBackend<Block>
        + BlockImport<Block>
        + AuxStore
        + ProofProvider<Block>
        + ClientExt<Block, Backend>
        + Send
        + Sync
        + 'static,
    for<'a> &'a Client: BlockImport<Block>,
    NR: NetworkRequest + Send,
    CBlock: BlockT,
    CClient: HeaderBackend<CBlock> + ProofProvider<CBlock> + Send + Sync + 'static,
{
    let execution_receipt_result = sync_params
        .consensus_chain_sync_params
        .execution_receipt_provider
        .get_execution_receipt(None)
        .await;
    debug!(
        "Snap-sync: execution receipt result - {:?}",
        execution_receipt_result
    );

    let Some(last_confirmed_block_receipt) = execution_receipt_result else {
        return Err(sp_blockchain::Error::RemoteFetchFailed);
    };

    let consensus_block_number =
        convert_block_number::<CBlock>(last_confirmed_block_receipt.consensus_block_number);

    let consensus_block_hash = last_confirmed_block_receipt.consensus_block_hash;
    sync_params
        .consensus_chain_sync_params
        .synchronizer
        .allow_consensus_snap_sync(consensus_block_number); // TODO: combine workflow
    sync_params
        .consensus_chain_sync_params
        .synchronizer
        .allow_resuming_consensus_sync(); // TODO: remove

    sync_params
        .consensus_chain_sync_params
        .synchronizer
        .domain_snap_sync_allowed()
        .await;

    wait_for_block_import_ext(
        sync_params.consensus_client.as_ref(),
        consensus_block_number.into(),
        Duration::from_secs(30),
        100,
    )
    .await; // TODO:

    let domain_block_number =
        convert_block_number::<Block>(last_confirmed_block_receipt.domain_block_number);
    let domain_block_hash = last_confirmed_block_receipt.domain_block_hash;
    let domain_block = get_last_confirmed_block(
        sync_params.block_downloader,
        &sync_params.sync_service,
        domain_block_number,
    )
    .await?;

    let Some(domain_block_header) = domain_block.header.clone() else {
        return Err(sp_blockchain::Error::MissingHeader(
            "Can't obtain domain block header for snap sync".to_string(),
        ));
    };

    let state_result = download_state(
        &domain_block_header,
        &sync_params.domain_client,
        sync_params.fork_id,
        &sync_params.network_request,
        &sync_params.sync_service,
    )
    .await;

    trace!("State downloaded: {:?}", state_result);

    {
        let client = sync_params.domain_client.clone();
        // Import first block as finalized
        let mut block =
            BlockImportParams::new(BlockOrigin::NetworkInitialSync, domain_block_header);
        block.body = domain_block.body;
        block.justifications = domain_block.justifications;
        block.state_action =
            StateAction::ApplyChanges(StorageChanges::Import(state_result.unwrap())); // TODO:
        block.finalized = true;
        block.fork_choice = Some(ForkChoiceStrategy::Custom(true));
        // TODO: Simplify when https://github.com/paritytech/polkadot-sdk/pull/5339 is in our fork
        (&mut client.as_ref())
            .import_block(block)
            .await
            .map_err(|error| {
                sp_blockchain::Error::Backend(format!("Failed to import state block: {error}"))
            })?;
    }

    crate::aux_schema::track_domain_hash_and_consensus_hash(
        sync_params.domain_client.as_ref(),
        domain_block_hash,
        consensus_block_hash,
    )?;

    crate::aux_schema::write_execution_receipt::<_, Block, CBlock>(
        sync_params.domain_client.as_ref(),
        None,
        &last_confirmed_block_receipt,
    )?;

    wait_for_block_import(
        sync_params.domain_client.as_ref(),
        domain_block_number.into(),
    )
    .await;
    trace!(
        "Domain client info after waiting: {:?}",
        sync_params.domain_client.info()
    );

    sync_params
        .consensus_chain_sync_params
        .synchronizer
        .mark_initial_blocks_imported(); // TODO:

    // Clear the block gap that arises from first block import with a much higher number than
    // previously (resulting in a gap)
    // TODO: This is a hack and better solution is needed: https://github.com/paritytech/polkadot-sdk/issues/4407
    sync_params.domain_client.clear_block_gap()?;

    debug!(info = ?sync_params.domain_client.info(), "Client info after successful domain snap sync.");

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

        info!("Gathering peers for state sync."); // TODO:
        let mut tried_peers = HashSet::<PeerId>::new();

        // TODO: add loop timeout
        let current_peer_id = loop {
            let connected_full_peers = sync_service
                .peers_info()
                .await
                .expect("Network service must be available.")
                .iter()
                .map(|(peer_id, _)| {
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
