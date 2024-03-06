use crate::sync::dsn_sync::import_blocks::download_and_reconstruct_blocks;
use crate::sync::segment_header_downloader::SegmentHeaderDownloader;
use crate::sync::DsnSyncPieceGetter;
use core::default::Default;
use parity_scale_codec::{Decode, Encode};
use sc_client_api::{AuxStore, BlockBackend, ProofProvider};
use sc_consensus::import_queue::ImportQueueService;
use sc_consensus::IncomingBlock;
use sc_consensus_subspace::archiver::{decode_block, SegmentHeadersStore};
use sc_network::NetworkService;
use sc_network_sync::fast_sync_engine::FastSyncingEngine;
use sc_network_sync::service::network::NetworkServiceProvider;
use sc_network_sync::SyncingService;
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
use subspace_archiving::reconstructor::Reconstructor;
use subspace_core_primitives::SegmentIndex;
use subspace_networking::Node;
use tokio::time::sleep;
use tracing::info;

#[derive(Clone, Debug, Encode, Decode)]
pub struct TempRawBlockData<Block: BlockT> {
    pub hash: Block::Hash,
    pub header: Block::Header,
    pub block_body: Option<Vec<Block::Extrinsic>>,
    pub justifications: Option<Justifications>,
    pub number: NumberFor<Block>,
}

impl<Block: BlockT> TempRawBlockData<Block> {
    fn from_raw_block(block_number: NumberFor<Block>, value: RawBlockData<Block>) -> Self {
        Self {
            hash: value.hash,
            header: value.header,
            block_body: value.block_body,
            justifications: value.justifications,
            number: block_number,
        }
    }

    fn to_raw_block(self) -> RawBlockData<Block> {
        RawBlockData {
            hash: self.hash,
            header: self.header,
            block_body: self.block_body,
            justifications: self.justifications,
        }
    }
}

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
    sync_service: Arc<SyncingService<Block>>,
    client: Arc<Client>,
    import_queue_service1: Box<IQS>,
    mut import_queue_service2: Box<IQS>,
    network_service: Arc<NetworkService<Block, <Block as BlockT>::Hash>>,
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
    Client::Api: SubspaceApi<Block, FarmerPublicKey>,
    IQS: ImportQueueService<Block> + ?Sized + 'static,
{
    let mut reconstructor = Reconstructor::new().map_err(|error| error.to_string())?;

    println!("************* Downloading last segment *************");
    let segment_header_downloader = SegmentHeaderDownloader::new(node);

    download_segment_headers(segment_headers_store, &segment_header_downloader)
        .await
        .unwrap(); // TODO

    let last_segment_index = segment_headers_store.max_segment_index().unwrap(); // TODO:
    let last_segment_header = segment_headers_store.get_segment_header(last_segment_index);

    println!("Last segment header: {last_segment_header:?}");

    let last_segment_header = last_segment_header.unwrap();

    let blocks = download_and_reconstruct_blocks(
        last_segment_header.segment_index(),
        piece_getter,
        &mut reconstructor,
    )
    .await?;

    let mut last_block = Default::default(); //TODO:
    let mut prev_last_block = Default::default(); //TODO:

    for block in blocks {
        prev_last_block = last_block;
        last_block = block;
    }

    let active_block = prev_last_block;
    let block_bytes = active_block.1;

    let (header, extrinsics, justifications) = deconstruct_block::<Block>(block_bytes)?;
    let hash = header.hash();
    println!("Reconstructed block #{} {:?}", active_block.0, hash);

    let raw_block_data = RawBlockData {
        hash,
        header,
        block_body: Some(extrinsics),
        justifications,
    };

    let raw_block = TempRawBlockData::from_raw_block(active_block.0.into(), raw_block_data);

    let last_imported_segment_index = last_segment_index;
    let last_imported_block_number = active_block.0.into();

    let original_raw_block = raw_block.clone().to_raw_block();

    let hash1 = raw_block.hash;
    let block_body = raw_block.block_body;
    let justifications = raw_block.justifications;
    let header = raw_block.header;
    let number = raw_block.number;

    let imported_block_header = client.header(hash1);
    println!("Imported block header: {imported_block_header:?}");

    client.import_raw_block(original_raw_block);

    let imported_block_header = client.header(hash);
    println!("Imported block header: {imported_block_header:?}");

    let mut open_peers;
    loop {
        open_peers = network_service.open_peers().await.unwrap();
        //let active_peers = sync_service.num_sync_peers().await.unwrap();
        let active_peers = open_peers.len();
        println!("Sync peers: {active_peers}");
        sleep(Duration::from_secs(20)).await;

        if active_peers > 6 {
            break;
        }
    }

    let (network_service_worker, network_service_handle) = NetworkServiceProvider::new();

    let networking_fut = network_service_worker.run(network_service);

    println!("Peers - #{number} - {:?}", open_peers);
    let initial_peers = open_peers.into_iter().map(|peer_id| (peer_id, number));
    let (sync_worker, sync_engine) = FastSyncingEngine::new(
        client.clone(),
        import_queue_service1,
        network_service_handle,
        None,
        header.clone(),
        block_body.clone(),
        justifications.clone(),
        true,
        initial_peers,
    )
    .unwrap(); // TODO: remove error
    let sync_fut = sync_worker.run();

    let net_fut = tokio::spawn(networking_fut);
    let sync_worker_handle = tokio::spawn(sync_fut); // TODO: join until finish

    // Start the process
    let _ = sync_engine.peers_info().await;

    let result = sync_worker_handle.await;

    println!("Sync worker handle result: {}", result.is_ok(),);

    let info = client.info();

    println!("**** Client info1: {:?}", info);

    client.clear_block_gap();

    // Import delay
    sleep(Duration::from_secs(5)).await;

    net_fut.abort();

    // This will notify Substrate's sync mechanism and allow regular Substrate sync to continue gracefully
    match result {
        Ok(Some(current_block)) => {
            import_queue_service2.import_blocks(BlockOrigin::NetworkBroadcast, vec![current_block]);
            println!("**** Sync worker handle was imported with broadcast",);
        }
        Ok(None) => {
            println!("**** Sync worker handle returned None",);
        }
        Err(err) => {
            println!("**** Sync worker handle returned Err={}", err);
        }
    }

    let info = client.info();

    println!("**** Client info2: {:?}", info);

    sync_service.new_best_number(number);

    // Import delay
    sleep(Duration::from_secs(5)).await;

    let (header, extrinsics, justifications) = deconstruct_block::<Block>(last_block.1)?;
    let hash = header.hash();
    let last_incoming_block = create_incoming_block(header, extrinsics, justifications);
    println!("Reconstructed block #{} {:?}", last_block.0, hash);
    import_queue_service2.import_blocks(BlockOrigin::NetworkBroadcast, vec![last_incoming_block]);
    println!("**** Last block imported.",);

    // Import delay
    sleep(Duration::from_secs(5)).await;
    let info = client.info();

    println!("**** Client info3: {:?}", info);

    //    panic!("Stop");

    Ok(FastSyncResult::<Block> {
        last_imported_block_number,
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
