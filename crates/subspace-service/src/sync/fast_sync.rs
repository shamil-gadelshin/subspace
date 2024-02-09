use crate::sync::dsn_sync::import_blocks::download_and_reconstruct_blocks;
use crate::sync::segment_header_downloader::SegmentHeaderDownloader;
use crate::sync::DsnSyncPieceGetter;
use sc_client_api::{AuxStore, BlockBackend, ProofProvider};
use sc_consensus::import_queue::ImportQueueService;
use sc_consensus::IncomingBlock;
use sc_consensus_subspace::archiver::{decode_block, find_last_archived_block, SegmentHeadersStore};
use sc_network::NetworkService;
use sc_network_sync::fast_sync_engine::FastSyncingEngine;
use sc_network_sync::service::network::NetworkServiceProvider;
use sc_service::{ClientExt, RawBlockData};
use sp_api::ProvideRuntimeApi;
use sp_blockchain::HeaderBackend;
use sp_consensus::BlockOrigin;
use sp_consensus_subspace::{FarmerPublicKey, SubspaceApi};
use sp_runtime::generic::SignedBlock;
use sp_runtime::traits::{Block as BlockT, Header, NumberFor};
use sp_runtime::Justifications;
use std::sync::Arc;
use std::time::Duration;
use futures::channel::mpsc;
use subspace_archiving::reconstructor::Reconstructor;
use subspace_core_primitives::SegmentIndex;
use subspace_networking::Node;
use tokio::time::sleep;
use tracing::{error, info};
use sc_consensus_subspace::block_import::BlockImportingNotification;
use sc_consensus_subspace::SubspaceLink;
use sp_objects::ObjectsApi;

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
    import_queue_service1: Box<IQS>,
    mut import_queue_service2: Box<IQS>,
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
    let second_last_segment_header =  segment_headers_store
        .get_segment_header(second_last_segment_index)
        .expect("We get segment index from the same storage. It should be present.");

    let prev_blocks = download_and_reconstruct_blocks(
        second_last_segment_index,
        piece_getter,
        &mut reconstructor,
    )
    .await?;

    println!("**** Prev blocks downloaded (SegmentId={}): {}-{}", second_last_segment_index, prev_blocks[0].0, prev_blocks[prev_blocks.len() - 1].0);

    let blocks = download_and_reconstruct_blocks(
        last_segment_header.segment_index(),
        piece_getter,
        &mut reconstructor,
    )
    .await?;

    println!("**** Blocks downloaded (SegmentId={}): {}-{}", last_segment_index, blocks[0].0, blocks[blocks.len() - 1].0);

    assert_eq!(second_last_segment_header.last_archived_block().number, blocks[0].0);

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

        // // Skip the last block import. We'll import it later with execution.
        // if number == NumberFor::<Block>::from(last_imported_block_number){
        //     break;
        // }

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

    info!("Gathering peers for state sync.");

//    panic!("Stop");

    let mut open_peers;
    loop {
        open_peers = network_service.open_peers().await.unwrap();
        //let active_peers = sync_service.num_sync_peers().await.unwrap();
        let active_peers = open_peers.len();
        println!("Sync peers: {active_peers}"); // TODO:
        sleep(Duration::from_secs(20)).await;

        if active_peers > 6 {
            break;
        }
    }

    info!("Starting state sync...");

    let (network_service_worker, network_service_handle) = NetworkServiceProvider::new();

    let networking_fut = network_service_worker.run(network_service);

    // Prepare gathering the state for the second last block
    // let block_bytes = second_last_block.1;
    // let second_last_block_number = second_last_block.0;

    // TODO: check edge case (full segment)
    let notification_block_bytes = blocks[0].1.clone();
    let notification_block: SignedBlock<Block> = decode_block::<Block>(&notification_block_bytes).map_err(|error| error.to_string())?;
    let notification_block_number = blocks[0].0;

    let (header, extrinsics, justifications) = deconstruct_block::<Block>(notification_block_bytes.clone())?;
    let hash = header.hash();

    info!(?hash, parent_hash=?header.parent_hash(), "Reconstructed block #{} for raw block import", notification_block_number);

    let raw_block = RawBlockData {
        hash,
        header,
        block_body: Some(extrinsics),
        justifications,
    };

    client.import_raw_block(raw_block.clone());

    info!(?hash, "#{notification_block_number} raw Notification block imported",);
    info!(?hash, "Notification block = #{notification_block_number}",);

    let state_block_bytes = notification_block_bytes;
    let state_block_number = notification_block_number;

    let (header, extrinsics, justifications) = deconstruct_block::<Block>(state_block_bytes)?;
    let initial_peers = open_peers.into_iter().map(|peer_id| (peer_id, state_block_number.into()));

    let (sync_worker, sync_engine) = FastSyncingEngine::new(
        client.clone(),
        import_queue_service1,
        network_service_handle,
        None,
        header.clone(),
        Some(extrinsics),
        justifications.clone(),
        true,
        initial_peers,
    )
    .map_err(sc_service::Error::Client)?;

    let sync_fut = sync_worker.run();

    let net_fut = tokio::spawn(networking_fut);
    let sync_worker_handle = tokio::spawn(sync_fut); // TODO: join until finish

    // Start syncing..
    let _ = sync_engine.start().await;

    let last_block_from_sync = sync_worker_handle.await
        .map_err(|e| sc_service::Error::Other("Fast sync task error.".into()))?
        .map_err(sc_service::Error::Client)?;

    info!("Sync worker handle result: {:?}", last_block_from_sync,);

    info!("Clearing block gap...");
    client.clear_block_gap();

    // Block import delay
    sleep(Duration::from_secs(5)).await; // TODO

    net_fut.abort();

    info!("Started importing blocks from last segment.");

    for block in blocks.into_iter() {
        let block_bytes = block.1;
        let number = NumberFor::<Block>::from(block.0);

        let (header, extrinsics, justifications) = deconstruct_block::<Block>(block_bytes.clone())?;
        let hash = header.hash();

        info!(?hash, parent_hash=?header.parent_hash(), "Reconstructed block #{} for block import", number);

        let last_incoming_block = create_incoming_block(header, extrinsics, justifications);

        // Skip the last block import. We'll import it later with execution.
        if number == NumberFor::<Block>::from(last_imported_block_number){
            break;
        }

        if number == notification_block_number.into(){
            let (acknowledgement_sender, _) = mpsc::channel(0);
            let notification_block = notification_block.clone();
            subspace_link
                .block_importing_notification_sender()
                .notify(move || BlockImportingNotification {
                    block_number: notification_block_number.into(),
                    origin: BlockOrigin::FastSync,
                    acknowledgement_sender,
                    //    last_archived_block: None,
                    last_archived_block: Some((
                        second_last_segment_header,
                        notification_block.clone(),
                        Default::default(),
                    ))
                });

            sleep(Duration::from_secs(3)).await; // TODO

            continue
        }

        info!("Clearing block gap...");
        client.clear_block_gap();

        import_queue_service2.import_blocks(BlockOrigin::NetworkInitialSync, vec![last_incoming_block]);
//        info!(?hash, "#{number} block imported",);
//        sleep(Duration::from_millis(20)).await; // TODO

    }
    // Block import delay
    sleep(Duration::from_secs(20)).await; // TODO

    info!(
        "Importing the last block from the segment #{}",
        last_block.0
    );
    let (header, extrinsics, justifications) = deconstruct_block::<Block>(last_block.1)?;
    let hash = header.hash();
    let last_incoming_block = create_incoming_block(header, extrinsics, justifications);
 //   let last_block_number = last_block.0;

    info!(
        ?hash,
        %last_imported_block_number,
        %last_segment_index,
        "Importing reconstructed last block from the segment.",
    );

    // let last_segment_header = segment_headers_store
    //     .get_segment_header(last_segment_index)
    //     .expect("We get segment index from the same storage. It should be present.");
    // assert_eq!(last_segment_header.segment_index(), last_segment_index);
    // let last_archived_block_number = last_segment_header.last_archived_block().number;

    // let last_archived_block = client
    //     .block(last_archived_block_hash)?
    //     .expect("Last archived block must always be retrievable; qed");

    // let block_object_mappings = client
    //     .runtime_api()
    //     .validated_object_call_hashes(last_archived_block_hash)
    //     .and_then(|calls| {
    //         client.runtime_api().extract_block_object_mapping(
    //             *last_archived_block.block.header().parent_hash(),
    //             last_archived_block.block.clone(),
    //             calls,
    //         )
    //     })
    //     .unwrap_or_default();

   // info!(?last_segment_header, ?last_archived_block_number, "{:?}", last_archived_block);

    // TODO:
    // let (acknowledgement_sender, _) = mpsc::channel(0);
    // subspace_link
    //     .block_importing_notification_sender()
    //     .notify(move || BlockImportingNotification {
    //         block_number: block_number.into(),
    //         origin: BlockOrigin::FastSync,
    //         acknowledgement_sender,
    //         last_archived_block: None,
    //         // last_archived_block: Some((
    //         // last_segment_header,
    //         // last_archived_block,
    //         // block_object_mappings,
    //         // ))
    //     });

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
    import_queue_service2.import_blocks(BlockOrigin::NetworkBroadcast, vec![last_incoming_block]);


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

//    panic!("Stop");

    Ok(FastSyncResult::<Block> {
        last_imported_block_number: last_imported_block_number.into(),
        last_imported_segment_index,
        reconstructor,
    })
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
