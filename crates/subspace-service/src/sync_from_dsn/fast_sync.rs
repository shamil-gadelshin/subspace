use crate::sync_from_dsn::import_blocks::download_and_reconstruct_blocks;
use crate::sync_from_dsn::segment_header_downloader::SegmentHeaderDownloader;
use crate::sync_from_dsn::DsnSyncPieceGetter;
use sc_client_api::{AuxStore, BlockBackend, ProofProvider};
use sc_consensus::import_queue::ImportQueueService;
use sc_consensus::IncomingBlock;
use sc_consensus_subspace::archiver::{
    decode_block, SegmentHeadersStore,
};
use sc_consensus_subspace::block_import::{ArchiverInitilizationData};
use sc_consensus_subspace::SubspaceLink;
use sc_network::{NetworkService, PeerId};
use sc_network_sync::fast_sync_engine::FastSyncingEngine;
use sc_network_sync::service::network::NetworkServiceProvider;
use sc_service::{ClientExt, RawBlockData};
use sp_api::ProvideRuntimeApi;
use sp_blockchain::HeaderBackend;
use sp_consensus::BlockOrigin;
use sp_consensus_subspace::{FarmerPublicKey, SubspaceApi};
use sp_objects::ObjectsApi;
use sp_runtime::generic::SignedBlock;
use sp_runtime::traits::{Block as BlockT, Header, NumberFor};
use sp_runtime::Justifications;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use subspace_archiving::reconstructor::Reconstructor;
use subspace_core_primitives::SegmentIndex;
use subspace_networking::Node;
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{error, info};

pub(crate) struct FastSyncResult<Block: BlockT> {
    pub(crate) last_imported_block_number: NumberFor<Block>,
    pub(crate) last_imported_segment_index: SegmentIndex,
    pub(crate) reconstructor: Reconstructor,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn fast_sync<PG, AS, Block, Client, IQS>(
    segment_headers_store: &SegmentHeadersStore<AS>,
    node: &Node,
    piece_getter: &PG,
    client: Arc<Client>,
    import_queue_service: Box<IQS>,
    network_service: Arc<NetworkService<Block, <Block as BlockT>::Hash>>,
    subspace_link: SubspaceLink<Block>,
) -> Result<FastSyncResult<Block>, sc_service::Error>
where
    PG: DsnSyncPieceGetter,
    AS: AuxStore + Send + Sync + 'static,
    Block: BlockT,
    Client: HeaderBackend<Block>
        + ClientExt<Block>
        + BlockBackend<Block>
        + ProvideRuntimeApi<Block>
        + ProofProvider<Block>
        + Send
        + Sync
        + 'static,
    Client::Api: SubspaceApi<Block, FarmerPublicKey> + ObjectsApi<Block>,
    IQS: ImportQueueService<Block> + ?Sized + 'static,
{
    // TODO: skip fast sync if no segments
    // TODO: retry for fast sync will degrade reputation if we have already announced blocks
    info!("Starting fast sync...");

    let import_queue_service = Arc::new(Mutex::new(import_queue_service));

    let mut reconstructor = Reconstructor::new().map_err(|error| error.to_string())?;
    let segment_header_downloader = SegmentHeaderDownloader::new(node);

    if let Err(error) =
        download_segment_headers(segment_headers_store, &segment_header_downloader).await
    {
        error!(?error, "Failed to download segment headers.");
        return Err(error);
    };

    let Some(last_segment_index) = segment_headers_store.max_segment_index() else {
        return Err(sc_service::Error::Other(
            "Can't get last segment index.".into(),
        ));
    };

    let last_segment_header = segment_headers_store
        .get_segment_header(last_segment_index)
        .expect("We get segment index from the same storage. It should be present.");

    let second_last_segment_index = last_segment_header.segment_index() - 1.into();
    let second_last_segment_header = segment_headers_store
        .get_segment_header(second_last_segment_index)
        .expect("We get segment index from the same storage. It should be present.");

    let prev_blocks = download_and_reconstruct_blocks(
        second_last_segment_index,
        piece_getter,
        &mut reconstructor,
    )
    .await?;

    println!(
        "**** Prev blocks downloaded (SegmentId={}): {}-{}",
        second_last_segment_index,
        prev_blocks[0].0,
        prev_blocks[prev_blocks.len() - 1].0
    );

    let blocks = download_and_reconstruct_blocks(
        last_segment_header.segment_index(),
        piece_getter,
        &mut reconstructor,
    )
    .await?;

    println!(
        "**** Blocks downloaded (SegmentId={}): {}-{}",
        last_segment_index,
        blocks[0].0,
        blocks[blocks.len() - 1].0
    );

    assert_eq!(
        second_last_segment_header.last_archived_block().number,
        blocks[0].0
    );

    let prev_last_block = prev_blocks[prev_blocks.len() - 1].clone();
    let prev_second_last_block = prev_blocks[prev_blocks.len() - 2].clone();

    info!("Started importing prev_blocks 'raw blocks'.");
    for block in prev_blocks.into_iter() {
        let block_bytes = block.1;
        let number = NumberFor::<Block>::from(block.0);

        let (header, extrinsics, justifications) = deconstruct_block::<Block>(block_bytes)?;
        let hash = header.hash();

        info!(?hash, parent_hash=?header.parent_hash(), "Reconstructed block #{} for raw block import", number);

        let raw_block = RawBlockData {
            hash,
            header,
            block_body: Some(extrinsics),
            justifications,
        };

        client.import_raw_block(raw_block.clone());
        info!(?hash, "#{number} raw block imported",);
    }

    if blocks.len() < 2 {
        return Err(sc_service::Error::Other(
            "Unexpected block array length.".into(),
        ));
    }

    let last_block = blocks[blocks.len() - 1].clone();
    let second_last_block = blocks[blocks.len() - 2].clone();

    let last_imported_segment_index = last_segment_index;
    let last_imported_block_number = last_block.0;

    // Prepare gathering the state for the second last block
    // let block_bytes = second_last_block.1;
    // let second_last_block_number = second_last_block.0;

    // TODO: check edge case (full segment)
    let notification_block_bytes = blocks[0].1.clone();
    let notification_block: SignedBlock<Block> =
        decode_block::<Block>(&notification_block_bytes).map_err(|error| error.to_string())?;
    let notification_block_number = blocks[0].0;

    let (header, extrinsics, justifications) =
        deconstruct_block::<Block>(notification_block_bytes.clone())?;
    let hash = header.hash();

    info!(?hash, parent_hash=?header.parent_hash(), "Reconstructed block #{} for raw block import", notification_block_number);

    let raw_block = RawBlockData {
        hash,
        header,
        block_body: Some(extrinsics),
        justifications,
    };

    client.import_raw_block(raw_block.clone());

    info!(
        ?hash,
        "#{notification_block_number} raw Notification block imported",
    );
    info!(?hash, "Notification block = #{notification_block_number}",);

    let last_block_from_sync = download_state(
        client.clone(),
        import_queue_service.clone(),
        network_service,
        notification_block_bytes,
        notification_block_number.into(),
    )
    .await;

    info!("Started importing blocks from last segment.");

    for block in blocks.into_iter() {
        let block_bytes = block.1;
        let number = NumberFor::<Block>::from(block.0);

        let (header, extrinsics, justifications) = deconstruct_block::<Block>(block_bytes.clone())?;
        let hash = header.hash();

        info!(?hash, parent_hash=?header.parent_hash(), "Reconstructed block #{} for block import", number);

        let last_incoming_block = create_incoming_block(header, extrinsics, justifications);

        // Skip the last block import. We'll import it later with execution.
        if number == NumberFor::<Block>::from(last_imported_block_number) {
            break;
        }

        if number == notification_block_number.into() {
            let notification_block = notification_block.clone();
            subspace_link
                .archiver_notification_sender()
                .notify(move || ArchiverInitilizationData {
                    last_archived_block: (
                        second_last_segment_header,
                        notification_block.clone(),
                        Default::default(),
                    ),
                });

            sleep(Duration::from_secs(3)).await; // TODO

            continue;
        }

        {
            import_queue_service
                .lock()
                .await
                .import_blocks(BlockOrigin::NetworkInitialSync, vec![last_incoming_block]);
        }
    }

    info!("Clearing block gap...");
    client.clear_block_gap();

    // Block import delay
    sleep(Duration::from_secs(20)).await; // TODO

    info!(
        "Importing the last block from the segment #{}",
        last_block.0
    );
    let (header, extrinsics, justifications) = deconstruct_block::<Block>(last_block.1)?;
    let hash = header.hash();
    let last_incoming_block = create_incoming_block(header, extrinsics, justifications);

    info!(
        ?hash,
        %last_imported_block_number,
        %last_segment_index,
        "Importing reconstructed last block from the segment.",
    );

    // Final import delay
    loop {
        let info = client.info();
        info!(%last_imported_block_number, "Waiting client info: {:?}", info);
        tokio::time::sleep(Duration::from_secs(5)).await;

        let block_number: NumberFor<Block> = last_imported_block_number.into();
        if block_number >= info.best_number {
            break;
        }
    }

    // Import and execute the last block from the segment and setup the substrate sync
    {
        import_queue_service
            .lock()
            .await
            .import_blocks(BlockOrigin::NetworkBroadcast, vec![last_incoming_block]);
    }

    // Final import delay
    loop {
        let info = client.info();
        info!(%last_imported_block_number, "Waiting client info: {:?}", info);
        tokio::time::sleep(Duration::from_secs(5)).await;

        let block_number: NumberFor<Block> = last_imported_block_number.into();
        if block_number >= info.best_number {
            break;
        }
    }

    // Final import delay
    sleep(Duration::from_secs(5)).await; // TODO

    let info = client.info();
    info!("Current client info: {:?}", info);

    Ok(FastSyncResult::<Block> {
        last_imported_block_number: last_imported_block_number.into(),
        last_imported_segment_index,
        reconstructor,
    })
}

async fn download_state<Client, IQS, Block>(
    client: Arc<Client>,
    import_queue_service: Arc<Mutex<Box<IQS>>>,
    network_service: Arc<NetworkService<Block, <Block as BlockT>::Hash>>,
    state_block_bytes: Vec<u8>,
    state_block_number: NumberFor<Block>,
) -> Result<Option<IncomingBlock<Block>>, sc_service::Error>
where
    Block: BlockT,
    Client: HeaderBackend<Block>
        + ClientExt<Block>
        + BlockBackend<Block>
        + ProvideRuntimeApi<Block>
        + ProofProvider<Block>
        + Send
        + Sync
        + 'static,
    Client::Api: SubspaceApi<Block, FarmerPublicKey> + ObjectsApi<Block>,
    IQS: ImportQueueService<Block> + ?Sized + 'static,
{
    let (header, extrinsics, justifications) = deconstruct_block::<Block>(state_block_bytes)?;

    const STATE_SYNC_RETRIES: u32 = 5;

    for attempt in 1..=STATE_SYNC_RETRIES {
        info!(%attempt, "Starting state sync...");

        info!("Gathering peers for state sync.");
        let network_service = network_service.clone();
        let mut tried_peers = HashSet::<PeerId>::new();

        // TODO: add loop timeout
        let peer_candidates = loop {
            let open_peers = network_service
                .open_peers()
                .await
                .expect("Network service must be available.");

            info!(?tried_peers, "Sync peers: {}", open_peers.len()); // TODO: debug comment

            let active_peers_set = HashSet::from_iter(open_peers.into_iter());

            let diff = active_peers_set
                .difference(&tried_peers)
                .cloned()
                .collect::<HashSet<_>>();

            if !diff.is_empty() {
                break diff;
            }

            sleep(Duration::from_secs(20)).await; // TODO: customize period
        };

        let current_peer_id = peer_candidates
            .into_iter()
            .next()
            .expect("Length is checked within the loop.");
        tried_peers.insert(current_peer_id);

        let (network_service_worker, network_service_handle) = NetworkServiceProvider::new();

        let networking_fut = network_service_worker.run(network_service);

        let (sync_worker, sync_engine) = FastSyncingEngine::<Block, IQS>::new(
            client.clone(),
            import_queue_service.clone(),
            network_service_handle,
            None,
            header.clone(),
            Some(extrinsics.clone()),
            justifications.clone(),
            true,
            (current_peer_id, state_block_number),
        )
        .map_err(sc_service::Error::Client)?;

        let sync_fut = sync_worker.run();

        let net_fut = tokio::spawn(networking_fut);
        let sync_worker_handle = tokio::spawn(sync_fut); // TODO: join until finish

        // Start syncing..
        let _ = sync_engine.start().await;
        let last_block_from_sync_result = sync_worker_handle.await;

        net_fut.abort();

        // .map_err(|e| sc_service::Error::Other("Fast sync task error.".into()))?
        // .map_err(sc_service::Error::Client)?;

        match last_block_from_sync_result {
            Ok(Ok(last_block_from_sync)) => {
                info!("Sync worker handle result: {:?}", last_block_from_sync,);

                // Block import delay
                sleep(Duration::from_secs(5)).await; // TODO

                info!("Clearing block gap...");
                client.clear_block_gap();

                return Ok(last_block_from_sync);
            }
            Ok(Err(error)) => {
                error!(?error, "State sync future error.");
                continue;
            }
            Err(error) => {
                error!(?error, "State sync future error.");
                continue;
            }
        }
    }

    Err(sc_service::Error::Other(
        "All fast sync retries failed.".into(),
    ))
}

fn deconstruct_block<Block: BlockT>(
    block_data: Vec<u8>,
) -> Result<(Block::Header, Vec<Block::Extrinsic>, Option<Justifications>), sc_service::Error> {
    let signed_block = decode_block::<Block>(&block_data).map_err(|error| error.to_string())?;

    let SignedBlock {
        block,
        justifications,
    } = signed_block;
    let (header, extrinsics) = block.deconstruct();

    Ok((header, extrinsics, justifications))
}

fn create_incoming_block<Block: BlockT>(
    header: Block::Header,
    extrinsics: Vec<Block::Extrinsic>,
    justifications: Option<Justifications>,
) -> IncomingBlock<Block> {
    IncomingBlock {
        hash: header.hash(),
        header: Some(header),
        body: Some(extrinsics),
        indexed_body: None,
        justifications,
        origin: None,
        allow_missing_state: false,
        import_existing: false,
        skip_execution: false,
        state: None,
    }
}

async fn download_segment_headers<AS>(
    segment_headers_store: &SegmentHeadersStore<AS>,
    segment_header_downloader: &SegmentHeaderDownloader<'_>,
) -> Result<(), sc_service::Error>
where
    AS: AuxStore + Send + Sync + 'static,
{
    let max_segment_index = segment_headers_store.max_segment_index().ok_or_else(|| {
        sc_service::Error::Other(
            "Archiver needs to be initialized before syncing from DSN to populate the very \
                    first segment"
                .to_string(),
        )
    })?;
    let new_segment_headers = segment_header_downloader
        .get_segment_headers(max_segment_index)
        .await
        .map_err(|error| error.to_string())?;

    info!("Found {} new segment headers", new_segment_headers.len()); // TODO: debug

    if !new_segment_headers.is_empty() {
        segment_headers_store.add_segment_headers(&new_segment_headers)?;
    }

    Ok(())
}
