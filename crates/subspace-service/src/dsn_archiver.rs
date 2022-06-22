use futures::StreamExt;
use sc_consensus_subspace::{ArchivedSegmentNotification, SubspaceLink};
use sp_core::traits::SpawnEssentialNamed;
use sp_core::Encode;
use sp_runtime::traits::Block as BlockT;
use subspace_networking::PUB_SUB_ARCHIVING_TOPIC;
use tracing::{error, info, trace};

//TODO: telemetry??

//TODO
/// Start an archiver that will listen for imported blocks and archive blocks at `K` depth,
/// producing pieces and root blocks (root blocks are then added back to the blockchain as
/// `store_root_block` extrinsic).
pub fn start_subspace_dsn_archiver<Block>(
    subspace_link: &SubspaceLink<Block>,
    node_config: subspace_networking::Config, //TODO
    spawner: &impl SpawnEssentialNamed,
) where
    Block: BlockT,
{
    spawner.spawn_essential_blocking(
        "subspace-archiver-DSN",
        None,
        Box::pin({
            let mut archived_segment_notification_stream = subspace_link
                .archived_segment_notification_stream()
                .subscribe();

            async move {
                let (node, node_runner) = subspace_networking::create(node_config).await.unwrap();

                info!("DSN initialized: DSN Node ID is {}", node.id());

                tokio::spawn(async move {
                    node_runner.run().await;
                });

                while let Some(ArchivedSegmentNotification {
                    archived_segment, ..
                }) = archived_segment_notification_stream.next().await
                {
                    trace!("ArchivedSegmentNotification received");
                    let data = archived_segment.encode().to_vec();

                    // if let Err(err) = node.publish(PUB_SUB_ARCHIVING_TOPIC.clone(), data.to_vec()).await{
                    //     println!("DSN publish error: {:?}", err);
                    // }//TODO: expect("publish failed");

                    match node.publish(PUB_SUB_ARCHIVING_TOPIC.clone(), data).await {
                        Ok(_) => {
                            trace!("Archived segment published.");
                        }
                        Err(err) => {
                            error!("Failed to publish archived segment: {:?}", err);
                        }
                    }
                }
            }
        }),
    );
}
