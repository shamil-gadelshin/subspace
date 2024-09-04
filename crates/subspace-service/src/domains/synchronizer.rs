//! Provides syncrhronization primitives for consensus and domain chains snap sync.

use parking_lot::Mutex;
//use sp_runtime::traits::{Block as BlockT, NumberFor};
use subspace_core_primitives::BlockNumber;
use tokio::sync::Notify;

/// Syncrhonizes consensus and domain chain snap sync.
pub struct Synchronizer {
    notify_consensus_snap_sync: Notify,
    consensus_snap_sync_block_number: Mutex<Option<BlockNumber>>,
    notify_domain_snap_sync: Notify,
    notify_resuming_consensus_sync: Notify,
    notify_resuming_consensus_block_process: Notify,
    initial_blocks_imported: Mutex<bool>,
}

impl Default for Synchronizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Synchronizer {
    /// Constructor
    pub fn new() -> Self {
        Self {
            notify_consensus_snap_sync: Notify::new(),
            consensus_snap_sync_block_number: Mutex::new(None),
            notify_domain_snap_sync: Notify::new(),
            notify_resuming_consensus_sync: Notify::new(),
            notify_resuming_consensus_block_process: Notify::new(),
            initial_blocks_imported: Mutex::new(false),
        }
    }

    pub fn target_consensus_snap_sync_block_number(&self) -> Option<BlockNumber> {
        *self.consensus_snap_sync_block_number.lock()
    }

    pub async fn consensus_snap_sync_allowed(&self) {
        println!("Waiting for notify_consensus_snap_sync");
        self.notify_consensus_snap_sync.notified().await;
        println!("Finished waiting for notify_consensus_snap_sync");
    }

    pub fn allow_consensus_snap_sync(&self, block_number: BlockNumber) {
        println!("Allowed notify_consensus_snap_sync: {block_number}");
        self.consensus_snap_sync_block_number
            .lock()
            .replace(block_number);

        self.notify_consensus_snap_sync.notify_waiters();
    }

    pub async fn domain_snap_sync_allowed(&self) {
        println!("Waiting for notify_domain_snap_sync");

        self.notify_domain_snap_sync.notified().await;
        println!("Finished waiting for notify_domain_snap_sync");
    }

    pub fn initial_blocks_imported(&self) -> bool {
        *self.initial_blocks_imported.lock()
    }

    pub fn allow_domain_snap_sync(&self) {
        println!("Allowed notify_domain_snap_sync");
        self.notify_domain_snap_sync.notify_waiters();
    }

    pub async fn resuming_consensus_sync_allowed(&self) {
        println!("Waiting for notify_resuming_consensus_sync");
        self.notify_resuming_consensus_sync.notified().await;
        println!("Finished waiting for notify_resuming_consensus_sync");
    }

    pub fn allow_resuming_consensus_sync(&self) {
        println!("Allowed notify_resuming_consensus_sync");
        self.notify_resuming_consensus_sync.notify_waiters();
    }

    pub fn mark_initial_blocks_imported(&self) {
        println!("mark_initial_blocks_imported");
        *self.initial_blocks_imported.lock() = true;
    }

    pub async fn resuming_consensus_block_process_allowed(&self) {
        println!("Waiting for notify_resuming_consensus_block_process");
        self.notify_resuming_consensus_block_process
            .notified()
            .await;
        println!("Finished waiting for notify_resuming_consensus_block_process");
    }

    pub fn allow_resuming_consensus_block_process(&self) {
        println!("Allowed resuming_consensus_block_process");
        self.notify_resuming_consensus_block_process
            .notify_waiters();
    }
}
