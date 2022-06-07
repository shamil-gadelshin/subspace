// Copyright (C) 2021 Subspace Labs, Inc.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use crate::{ArchivedSegmentNotification, SubspaceLink, SubspaceNotificationSender};
use codec::Encode;
use futures::{future, SinkExt, StreamExt};
use log::{debug, error, info};
use sc_client_api::BlockBackend;
use sc_utils::mpsc::tracing_unbounded;
use sp_api::ProvideRuntimeApi;
use sp_blockchain::HeaderBackend;
use sp_consensus_subspace::{FarmerPublicKey, SubspaceApi};
use sp_objects::ObjectsApi;
use sp_runtime::generic::BlockId;
use sp_runtime::traits::{Block as BlockT, CheckedSub, Header, One, Saturating, Zero};
use std::sync::Arc;
use std::time::Duration;
use subspace_archiving::archiver::{ArchivedSegment, Archiver};
use subspace_core_primitives::{BlockNumber, RootBlock};

const ARCHIVED_SEGMENT_NOTIFICATION_INTERVAL: Duration = Duration::from_secs(5);

fn find_last_root_block<Block, Client>(client: &Client) -> Option<RootBlock>
where
    Block: BlockT,
    Client: ProvideRuntimeApi<Block> + BlockBackend<Block> + HeaderBackend<Block>,
    Client::Api: SubspaceApi<Block, FarmerPublicKey>,
{
    let mut block_to_check = BlockId::Hash(client.info().best_hash);
    loop {
        let block = client
            .block(&block_to_check)
            .expect("Older blocks should always exist")
            .expect("Older blocks should always exist");

        for extrinsic in block.block.extrinsics() {
            match client
                .runtime_api()
                .extract_root_blocks(&block_to_check, extrinsic)
            {
                Ok(Some(root_blocks)) => {
                    return root_blocks.into_iter().last();
                }
                Ok(None) => {
                    // Some other extrinsic, ignore
                }
                Err(error) => {
                    // TODO: Probably light client, can this even happen?
                    error!(target: "subspace", "Failed to make runtime API call: {:?}", error);
                }
            }
        }

        let parent_block_hash = *block.block.header().parent_hash();

        if parent_block_hash == Block::Hash::default() {
            // Genesis block, nothing else to check
            break None;
        }

        block_to_check = BlockId::Hash(parent_block_hash);
    }
}

/// Start an archiver that will listen for imported blocks and archive blocks at `K` depth,
/// producing pieces and root blocks (root blocks are then added back to the blockchain as
/// `store_root_block` extrinsic).
pub fn start_subspace_archiver<Block, Client>(
    subspace_link: &SubspaceLink<Block>,
    client: Arc<Client>,
    spawner: &impl sp_core::traits::SpawnEssentialNamed,
    is_authoring_blocks: bool,
) where
    Block: BlockT,
    Client: ProvideRuntimeApi<Block>
        + BlockBackend<Block>
        + HeaderBackend<Block>
        + Send
        + Sync
        + 'static,
    Client::Api: SubspaceApi<Block, FarmerPublicKey> + ObjectsApi<Block>,
{
    let best_block_id = BlockId::Hash(client.info().best_hash);

    let confirmation_depth_k = TryInto::<BlockNumber>::try_into(
        client
            .runtime_api()
            .confirmation_depth_k(&best_block_id)
            .expect("Failed to get `confirmation_depth_k` from runtime API"),
    )
    .unwrap_or_else(|_| {
        panic!("Confirmation depth K can't be converted into BlockNumber");
    });
    let record_size = client
        .runtime_api()
        .record_size(&best_block_id)
        .expect("Failed to get `record_size` from runtime API");
    let recorded_history_segment_size = client
        .runtime_api()
        .recorded_history_segment_size(&best_block_id)
        .expect("Failed to get `recorded_history_segment_size` from runtime API");

    let maybe_last_root_block = find_last_root_block(client.as_ref());

    let mut archiver = if let Some(last_root_block) = maybe_last_root_block {
        // Continuing from existing initial state
        let last_archived_block_number = last_root_block.last_archived_block().number;
        info!(
            target: "subspace",
            "Last archived block {}",
            last_archived_block_number,
        );
        let last_archived_block = client
            .block(&BlockId::Number(last_archived_block_number.into()))
            .expect("Older blocks must always exist")
            .expect("Older blocks must always exist");

        let block_object_mappings = client
            .runtime_api()
            .validated_object_call_hashes(&BlockId::Number(last_archived_block_number.into()))
            .and_then(|calls| {
                client.runtime_api().extract_block_object_mapping(
                    &BlockId::Number(last_archived_block_number.saturating_sub(1).into()),
                    last_archived_block.block.clone(),
                    calls,
                )
            })
            .unwrap_or_default();

        Archiver::with_initial_state(
            record_size as usize,
            recorded_history_segment_size as usize,
            last_root_block,
            &last_archived_block.encode(),
            block_object_mappings,
        )
        .expect("Incorrect parameters for archiver")
    } else {
        info!(target: "subspace", "Starting archiving from genesis");

        Archiver::new(record_size as usize, recorded_history_segment_size as usize)
            .expect("Incorrect parameters for archiver")
    };

    let mut older_archived_segments = Vec::new();

    // Process blocks since last fully archived block (or genesis) up to the current head minus K
    {
        let blocks_to_archive_from = archiver
            .last_archived_block_number()
            .map(|n| n + 1)
            .unwrap_or_default();
        let best_number = client.info().best_number;
        let blocks_to_archive_to = TryInto::<BlockNumber>::try_into(best_number)
            .unwrap_or_else(|_| {
                panic!(
                    "Best block number {} can't be converted into BlockNumber",
                    best_number,
                );
            })
            .checked_sub(confirmation_depth_k)
            .or_else(|| {
                if maybe_last_root_block.is_none() {
                    // If not continuation, archive genesis block
                    Some(0)
                } else {
                    None
                }
            });

        if let Some(blocks_to_archive_to) = blocks_to_archive_to {
            info!(
                target: "subspace",
                "Archiving already produced blocks {}..={}",
                blocks_to_archive_from,
                blocks_to_archive_to,
            );
            for block_to_archive in blocks_to_archive_from..=blocks_to_archive_to {
                let block = client
                    .block(&BlockId::Number(block_to_archive.into()))
                    .expect("Older block by number must always exist")
                    .expect("Older block by number must always exist");

                let block_object_mappings = client
                    .runtime_api()
                    .validated_object_call_hashes(&BlockId::Number(block_to_archive.into()))
                    .and_then(|calls| {
                        client.runtime_api().extract_block_object_mapping(
                            &BlockId::Number(block_to_archive.saturating_sub(1).into()),
                            block.block.clone(),
                            calls,
                        )
                    })
                    .unwrap_or_default();

                let encoded_block = block.encode();
                debug!(
                    target: "subspace",
                    "Encoded block {} has size of {:.2} kiB",
                    block_to_archive,
                    encoded_block.len() as f32 / 1024.0
                );

                let archived_segments = archiver.add_block(encoded_block, block_object_mappings);
                let new_root_blocks: Vec<RootBlock> = archived_segments
                    .iter()
                    .map(|archived_segment| archived_segment.root_block)
                    .collect();

                older_archived_segments.extend(archived_segments);

                if !new_root_blocks.is_empty() {
                    // Set list of expected root blocks for the block where we expect root block
                    // extrinsic to be included
                    subspace_link.root_blocks.lock().put(
                        if block_to_archive.is_zero() {
                            // Special case for genesis block whose root block should be included in
                            // the first block in order for further validation to work properly.
                            One::one()
                        } else {
                            (block_to_archive + confirmation_depth_k + 1).into()
                        },
                        new_root_blocks,
                    );
                }
            }
        }
    }

    spawner.spawn_essential_blocking(
        "subspace-archiver",
        None,
        Box::pin({
            let mut imported_block_notification_stream =
                subspace_link.imported_block_notification_stream.subscribe();
            let archived_segment_notification_sender =
                subspace_link.archived_segment_notification_sender.clone();

            async move {
                // Farmers may have not received all previous segments, send them now.
                if is_authoring_blocks {
                    for archived_segment in older_archived_segments {
                        send_archived_segment_notification(
                            &archived_segment_notification_sender,
                            archived_segment,
                        )
                        .await;
                    }
                } else {
                    drop(older_archived_segments);
                }

                let mut last_archived_block_number =
                    archiver.last_archived_block_number().map(Into::into);

                while let Some((block_number, mut root_block_sender)) =
                    imported_block_notification_stream.next().await
                {
                    let block_to_archive =
                        match block_number.checked_sub(&confirmation_depth_k.into()) {
                            Some(block_to_archive) => block_to_archive,
                            None => {
                                continue;
                            }
                        };

                    if let Some(last_archived_block) = &mut last_archived_block_number {
                        if *last_archived_block >= block_to_archive {
                            // This block was already archived, skip
                            continue;
                        }

                        *last_archived_block = block_to_archive;
                    } else {
                        last_archived_block_number.replace(block_to_archive);
                    }

                    debug!(target: "subspace", "Archiving block {:?}", block_to_archive);

                    let block = client
                        .block(&BlockId::Number(block_to_archive))
                        .expect("Older block by number must always exist")
                        .expect("Older block by number must always exist");

                    let block_object_mappings = client
                        .runtime_api()
                        .validated_object_call_hashes(&BlockId::Number(block_to_archive))
                        .and_then(|calls| {
                            client.runtime_api().extract_block_object_mapping(
                                &BlockId::Number(block_to_archive.saturating_sub(One::one())),
                                block.block.clone(),
                                calls,
                            )
                        })
                        .unwrap_or_default();

                    let encoded_block = block.encode();
                    debug!(
                        target: "subspace",
                        "Encoded block {} has size of {:.2} kiB",
                        block_to_archive,
                        encoded_block.len() as f32 / 1024.0
                    );
                    for archived_segment in archiver.add_block(encoded_block, block_object_mappings)
                    {
                        let root_block = archived_segment.root_block;

                        if is_authoring_blocks {
                            send_archived_segment_notification(
                                &archived_segment_notification_sender,
                                archived_segment,
                            )
                            .await;
                        }

                        let _ = root_block_sender.send(root_block).await;
                    }
                }
            }
        }),
    );
}

async fn send_archived_segment_notification(
    archived_segment_notification_sender: &SubspaceNotificationSender<ArchivedSegmentNotification>,
    archived_segment: ArchivedSegment,
) {
    let (acknowledgement_sender, mut acknowledgement_receiver) =
        tracing_unbounded("subspace_acknowledgement");
    let archived_segment_notification = ArchivedSegmentNotification {
        archived_segment: Arc::new(archived_segment),
        acknowledgement_sender,
    };

    // This could have been done in a nicer way (reactive), but that is a
    // lot of code, so we have this for now with periodic attempts.
    future::select(
        Box::pin(async {
            let get_value = move || archived_segment_notification;

            // Try in a loop until receiver below gets notification back
            loop {
                archived_segment_notification_sender.notify(get_value.clone());

                futures_timer::Delay::new(ARCHIVED_SEGMENT_NOTIFICATION_INTERVAL).await;

                info!(
                    target: "subspace",
                    "Waiting for farmer to receive and acknowledge \
                    archived segment",
                );
            }
        }),
        Box::pin(acknowledgement_receiver.next()),
    )
    .await;
}
