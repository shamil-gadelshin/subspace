use sc_client_api::{AuxStore, BlockBackend, BlockchainEvents};
use tracing::{debug, info};
use sc_consensus_subspace::archiver::{decode_block, SegmentHeadersStore};
use crate::sync::segment_header_downloader::SegmentHeaderDownloader;
use crate::sync::DsnSyncPieceGetter;
use subspace_archiving::reconstructor::Reconstructor;
use subspace_networking::Node;
use crate::sync::dsn_sync::import_blocks::download_and_reconstruct_blocks;
use core::default::Default;
use std::sync::Arc;
use std::time::Duration;
use sc_consensus::import_queue::ImportQueueService;
use sc_consensus::IncomingBlock;
use sc_network_sync::{SyncingService, SyncStatusProvider};
use sp_api::ProvideRuntimeApi;
use sp_blockchain::HeaderBackend;
use sp_consensus::BlockOrigin;
use sp_runtime::generic::SignedBlock;
use sp_consensus_subspace::FarmerPublicKey;
use sp_consensus_subspace::SubspaceApi;
use sp_runtime::traits::{Block as BlockT, CheckedSub, NumberFor, Header};
use tokio::time::sleep;


pub async fn download_last_segment<PG, AS, Block,Client, IQS>(
    segment_headers_store: &SegmentHeadersStore<AS>,
    node: &Node,
    piece_getter: &PG,
    sync_service: Arc<SyncingService<Block>>,
    client:  &Client,
    import_queue_service: &mut IQS,
) -> Result<(), sc_service::Error>
where
    PG: DsnSyncPieceGetter,
    AS: AuxStore + Send + Sync + 'static,
    Block: BlockT,
    Client: HeaderBackend<Block>
    + BlockBackend<Block>
    + ProvideRuntimeApi<Block>
    + Send
    + Sync
    + 'static,
    Client::Api: SubspaceApi<Block, FarmerPublicKey>,
    IQS: ImportQueueService<Block> + ?Sized,
{
    println!("************* Downloading last segment *************");
    let segment_header_downloader = SegmentHeaderDownloader::new(node);

    download_segment_headers(segment_headers_store, &segment_header_downloader).await.unwrap(); // TODO

    let last_segment_index = segment_headers_store.max_segment_index().unwrap(); // TODO:
    let last_segment_header = segment_headers_store.get_segment_header(last_segment_index);

//    let last_segment_header = segment_header_downloader.get_last_segment_header().await.map_err(|error| error.to_string())?;

    println!("Last segment header: {last_segment_header:?}");

    if let Some(last_segment_header) = last_segment_header {
        let mut reconstructor = Reconstructor::new().map_err(|error| error.to_string())?;


        let blocks =
            download_and_reconstruct_blocks(last_segment_header.segment_index(), piece_getter, &mut reconstructor)
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

        let mut blocks_to_import = Vec::<IncomingBlock<Block>>::new();

        blocks_to_import.push(IncomingBlock {
            hash,
            header: Some(header.clone()),
            body: Some(extrinsics),
            indexed_body: None,
            justifications,
            origin: None,
            allow_missing_state: true,
            import_existing: false,
            state: None,
            skip_execution: true,
        });

        import_queue_service
            .import_blocks(BlockOrigin::NetworkInitialSync, blocks_to_import);
        // // This will notify Substrate's sync mechanism and allow regular Substrate sync to continue gracefully
        // import_queue_service.import_blocks(BlockOrigin::NetworkBroadcast, vec![last_block]);

        println!("Last block #{}. Hash = {:?}, Header = {:?}", last_block.0, hash, header);

        sleep(Duration::from_secs(2)).await;
        let status = sync_service.status().await;
        println!("Sync status: {:?}", status);
        sync_service.on_block_finalized(hash, header);

        sleep(Duration::from_secs(3)).await;

        let status = sync_service.status().await;

        println!("Sync status: {:?}", status);

        // loop {
        //     let status = sync_service.status().await;
        //     println!("Sync status: {:?}", status);
        //
        //     sleep(Duration::from_secs(3)).await;
        //
        //     if let SyncStatus::
        // }

        // Wait and poll status
    } else {
        // TODO:
    }

    Ok(())
}


async fn download_segment_headers<AS,>(    segment_headers_store: &SegmentHeadersStore<AS>,
                                                            segment_header_downloader: &SegmentHeaderDownloader<'_>,)
    -> Result<(), sc_service::Error>
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
