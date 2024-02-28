use crate::sync::dsn_sync::import_blocks::download_and_reconstruct_blocks;
use crate::sync::segment_header_downloader::SegmentHeaderDownloader;
use crate::sync::DsnSyncPieceGetter;
use core::default::Default;
use domain_runtime_primitives::opaque::Block;
use parity_scale_codec::{Decode, Encode};
use sc_client_api::{AuxStore, BlockBackend, BlockchainEvents, ProofProvider};
use sc_consensus::import_queue::ImportQueueService;
use sc_consensus::IncomingBlock;
use sc_consensus_subspace::archiver::{decode_block, SegmentHeadersStore};
use sc_network::{service, NetworkService};
use sc_network_sync::fast_sync_engine::FastSyncingEngine;
use sc_network_sync::service::network::{NetworkServiceHandle, NetworkServiceProvider};
use sc_network_sync::{SyncStatusProvider, SyncingService, SyncStatus};
use sc_service::{ClientExt, RawBlockData};
use sp_api::ProvideRuntimeApi;
use sp_blockchain::HeaderBackend;
use sp_consensus::BlockOrigin;
use sp_consensus_subspace::{FarmerPublicKey, SubspaceApi};
use sp_runtime::generic::SignedBlock;
use sp_runtime::traits::{Block as BlockT, CheckedSub, Header, NumberFor, One};
use sp_runtime::Justifications;
use std::fs::File;
use std::hash::Hash;
use std::io;
use std::io::{Error, Read, Write};
use std::sync::Arc;
use std::time::Duration;
use subspace_archiving::reconstructor::Reconstructor;
use subspace_networking::Node;
use tokio::time::sleep;
use tracing::{debug, error, info};
use subspace_core_primitives::SegmentIndex;

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

// Function to write TempRawBlockData to a file
fn write_to_file<Block: BlockT>(data: &TempRawBlockData<Block>, file_path: &str) -> io::Result<()> {
    let mut file = File::create(file_path)?;
    let encoded = data.encode();
    file.write_all(&encoded)?;
    Ok(())
}

// Function to read TempRawBlockData from a file
fn read_from_file<Block: BlockT>(file_path: &str) -> Result<TempRawBlockData<Block>, Error> {
    let mut file = File::open(file_path)?;
    let mut encoded_data = Vec::new();
    file.read_to_end(&mut encoded_data)?;
    TempRawBlockData::decode(&mut &encoded_data[..]).map_err(|e| Error::other(e))
}

pub(crate) struct FastSyncResult<Block: BlockT>{
    pub(crate) last_imported_block_number: NumberFor<Block>,
    pub(crate) last_imported_segment_index: SegmentIndex,
    pub(crate) reconstructor: Reconstructor,
}

pub async fn download_last_segment<PG, AS, Block, Client, IQS>(
    segment_headers_store: &SegmentHeadersStore<AS>,
    node: &Node,
    piece_getter: &PG,
    sync_service: Arc<SyncingService<Block>>,
    client: Arc<Client>,
    mut import_queue_service1: Box<IQS>,
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
    IQS: ImportQueueService<Block> + ?Sized  + 'static,
{
    let mut last_imported_block_number: NumberFor<Block> = Default::default();
    let mut last_imported_segment_index: SegmentIndex = Default::default();
    let mut reconstructor = Reconstructor::new().map_err(|error| error.to_string())?;
    let raw_block = {
        let file_path = "/Users/shamix/raw_block3.bin";
        if let Ok(raw_block) = read_from_file(file_path) {
            raw_block
        } else {
            println!("************* Downloading last segment *************");
            let segment_header_downloader = SegmentHeaderDownloader::new(node);

            download_segment_headers(segment_headers_store, &segment_header_downloader)
                .await
                .unwrap(); // TODO

            let last_segment_index = segment_headers_store.max_segment_index().unwrap(); // TODO:
            let last_segment_header = segment_headers_store.get_segment_header(last_segment_index);

            // let last_segment_header = segment_header_downloader.get_last_segment_header().await.map_err(|error| error.to_string())?;

            println!("Last segment header: {last_segment_header:?}");

            let last_segment_header = last_segment_header.unwrap();

            let blocks = download_and_reconstruct_blocks(
                last_segment_header.segment_index(),
                piece_getter,
                &mut reconstructor,
            )
            .await?;

            let mut last_block = Default::default(); //TODO:
            for block in blocks {
                println!("Downloaded and reconstructed block #{}", block.0);

                last_block = block;
            }

            let block_bytes = last_block.1;
            let signed_block =
                decode_block::<Block>(&block_bytes).map_err(|error| error.to_string())?;

            let SignedBlock {
                block,
                justifications,
            } = signed_block;
            let (header, extrinsics) = block.deconstruct();
            let hash = header.hash();

            let raw_block_data = RawBlockData {
                hash,
                header,
                block_body: Some(extrinsics),
                justifications,
            };

            let raw_block = TempRawBlockData::from_raw_block(last_block.0.into(), raw_block_data);

      //      write_to_file(&raw_block, file_path).unwrap();

            last_imported_segment_index = last_segment_index;
            last_imported_block_number = last_block.0.into();

            raw_block
        }
    };

    let original_raw_block = raw_block.clone().to_raw_block();

    let hash = raw_block.hash;
    let block_body = raw_block.block_body;
    let justifications = raw_block.justifications;
    let header = raw_block.header;
    let number = raw_block.number;

    let imported_block_header = client.header(hash);
    println!("Imported block header: {imported_block_header:?}");

    client.import_raw_block(original_raw_block);

    let imported_block_header = client.header(hash);
    println!("Imported block header: {imported_block_header:?}");

    let mut open_peers = Vec::new();
    loop {
        open_peers = network_service.open_peers().await.unwrap();
        //let active_peers = sync_service.num_sync_peers().await.unwrap();
        let active_peers = open_peers.len();
        println!("Sync peers: {active_peers}");
        sleep(Duration::from_secs(20)).await;

        if active_peers > 6 {
            break
        }
    }

    let (network_service_worker, network_service_handle) = NetworkServiceProvider::new();

    let networking_fut = network_service_worker.run(network_service);

    println!("Peers - #{number} - {:?}", open_peers);
    let initial_peers = open_peers.into_iter().map(|peer_id| (peer_id, number)).into_iter();
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
       Ok(Some(last_block)) => {
           import_queue_service2.import_blocks(BlockOrigin::NetworkBroadcast, vec![last_block]);
           println!("**** Sync worker handle was imported with broadcast", );
       },
       Ok(None) => {
           println!("**** Sync worker handle returned None", );
       }
       Err(err) => {
           println!("**** Sync worker handle returned Err={}", err );
       }
   }

    let info = client.info();

    println!("**** Client info2: {:?}", info);

    // Import delay
    sleep(Duration::from_secs(5)).await;

    // loop {
    //     let sync_status = sync_service.status().await;
    //
    //     println!("**** Sync status: {:?}", sync_status);
    //
    //     match sync_status{
    //         Ok(sync_status) => {
    //             if let Some(target) = sync_status.best_seen_block {
    //                 let info = client.info();
    //
    //                 if info.best_number != target {
    //                     client.update_block_gap(info.best_number + One::one(), target)
    //                 } else {
    //                     println!("best_number equals target: {target}");
    //                 }
    //
    //                 break;
    //             }
    //         }
    //         Err(err) => {
    //             error!("Sync status error: {err:?}");
    //         }
    //     }
    //
    //     println!("Wait for valid sync status");
    //     sleep(Duration::from_secs(5)).await;
    // }
    //
    // let info = client.info();
    //
    // println!("**** Client info3: {:?}", info);

    // sync_service.on_block_finalized()
    // // Import delay
    // sleep(Duration::from_secs(5)).await;

    // for i in 0..10 {
    //     println!("Pulse {i}");
    //     sleep(Duration::from_secs(60)).await;
    // }
    //
    // panic!("Stop");

    // let mut blocks_to_import = Vec::<IncomingBlock<Block>>::new();
    //
    // blocks_to_import.push(IncomingBlock {
    //     hash,
    //     header: Some(header.clone()),
    //     body: block_body,
    //     indexed_body: None,
    //     justifications,
    //     origin: None,
    //     allow_missing_state: true,
    //     import_existing: false,
    //     state: None,
    //     skip_execution: true,
    // });

    // import_queue_service
    //     .import_blocks(BlockOrigin::NetworkInitialSync, blocks_to_import);
    // // // This will notify Substrate's sync mechanism and allow regular Substrate sync to continue gracefully
    // // import_queue_service.import_blocks(BlockOrigin::NetworkBroadcast, vec![last_block]);
    //
    // sleep(Duration::from_secs(3)).await;
    // let imported_block_header = client.header(hash);
    // println!("Imported block header: {imported_block_header:?}");

    // println!(
    //     "Last block #{}. Hash = {:?}, Header = {:?}",
    //     number, hash, header
    // );
    //
    // sleep(Duration::from_secs(2)).await;
    // let status = sync_service.status().await;
    // println!("Sync status: {:?}", status);
    // sync_service.on_block_finalized(hash, header);
    //
    // loop {
    //     sleep(Duration::from_secs(5)).await;
    //     let status = sync_service.status().await;
    //     println!("Sync status: {:?}", status);
    // }
    // sleep(Duration::from_secs(2)).await;
    // let status = sync_service.status().await;
    // println!("Sync status: {:?}", status);
    // sync_service.on_block_finalized(hash, header);
    //
    // sleep(Duration::from_secs(3)).await;
    //
    // let status = sync_service.status().await;
    //
    // println!("Sync status: {:?}", status);

    // loop {
    //     let status = sync_service.status().await;
    //     println!("Sync status: {:?}", status);
    //
    //     sleep(Duration::from_secs(3)).await;
    //
    //     if let SyncStatus::
    // }

    // Wait and poll status

    Ok(FastSyncResult::<Block> {
        last_imported_block_number,
        last_imported_segment_index,
        reconstructor
    })
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
