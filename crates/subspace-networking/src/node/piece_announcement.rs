use super::Node;
use crate::request_handlers::piece_announcement::{
    PieceAnnouncementRequest, PieceAnnouncementResponse,
};
use futures::StreamExt;
use libp2p::core::multihash::Multihash;
use std::collections::HashSet;
use std::error::Error;
use tracing::{debug, trace, warn};

const TARGET_PEERS_TO_ACKNOWLEDGE: usize = 3;

pub async fn announce_key(node: Node, key: Multihash) -> Result<bool, Box<dyn Error>> {
    let get_peers_result = node.get_closest_peers(key).await;

    let mut get_peers_stream = match get_peers_result {
        Ok(get_peers_stream) => get_peers_stream,
        Err(err) => {
            warn!(?err, "get_closest_peers returned an error");

            return Err(err.into());
        }
    };

    let mut contacted_peers = HashSet::new();
    let mut acknowledged_peers = HashSet::new();

    while let Some(peer_id) = get_peers_stream.next().await {
        trace!(?key, %peer_id, "get_closest_peers returned an item");

        if contacted_peers.contains(&peer_id) {
            continue; // skip duplicated PeerId
        }

        contacted_peers.insert(peer_id);

        let request_result = node
            .send_generic_request(
                peer_id,
                PieceAnnouncementRequest {
                    piece_key: key.to_bytes(),
                },
            )
            .await;

        match request_result {
            Ok(PieceAnnouncementResponse) => {
                trace!(
                    %peer_id,
                    ?key,
                    "Piece announcement request succeeded."
                );
            }
            Err(error) => {
                debug!(%peer_id, ?key, ?error, "Last root block request failed.");

                return Err(error.into());
            }
        }

        acknowledged_peers.insert(peer_id);

        // we hit the target peer number
        if acknowledged_peers.len() >= TARGET_PEERS_TO_ACKNOWLEDGE {
            return Ok(true);
        }
    }

    // we publish the key to at least one peer
    Ok(!acknowledged_peers.is_empty())
}
