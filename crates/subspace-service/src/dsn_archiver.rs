use futures::StreamExt;
use sc_consensus_subspace::{ArchivedSegmentNotification, SubspaceLink};
use sp_core::traits::SpawnEssentialNamed;
use sp_runtime::traits::Block as BlockT;
//use subspace_archiving::archiver::{ArchivedSegment, Archiver}; //TODO
use subspace_networking::libp2p::gossipsub::Sha256Topic;
use sp_core::Encode;

//TODO: telemetry??

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

                let topic = Sha256Topic::new("PUB-SUB-ARCHIVING");

                println!("DSN: Node ID is {}", node.id());

                tokio::spawn(async move {
                    node_runner.run().await;
                });

                while let Some(ArchivedSegmentNotification {
                    archived_segment, ..
                }) = archived_segment_notification_stream.next().await
                {
                    let msg = subspace_core_primitives::crypto::sha256_hash(&archived_segment.encode());

                    println!("ArchivedSegmentNotification received:  {:?}", msg);
                    if let Err(err) = node.publish(topic.clone(), msg.to_vec()).await{
                        println!("DSN publish error: {:?}", err);
                    }//TODO: expect("publish failed");
                }
            }
        }),
    );
}
