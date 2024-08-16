use parking_lot::Mutex;
//use sp_runtime::traits::{Block as BlockT, NumberFor};
use subspace_core_primitives::BlockNumber;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::Notify;

pub struct Synchronizer {
    notify_snap_sync: Notify,
    snap_sync_block_number: Mutex<Option<BlockNumber>>,
}

impl Synchronizer {
    pub fn new() -> Self {
        Self {
            notify_snap_sync: Notify::new(),
            snap_sync_block_number: Mutex::new(None),
        }
    }

    pub async fn snap_sync_allowed(&self) -> Option<BlockNumber> {
        self.notify_snap_sync.notified().await;

        *self.snap_sync_block_number.lock()
    }

    pub fn allow_snap_sync(&self, block_number: BlockNumber) {
        self.snap_sync_block_number.lock().replace(block_number);

        self.notify_snap_sync.notify_one();
    }
}

// pub struct Synchronizer{
//     snap_sync_rx: UnboundedReceiver<BlockNumber>,
//     snap_sync_tx: UnboundedSender<BlockNumber>,
// }
//
// impl Synchronizer{
//     pub fn new() -> Self<>{
//         let (tx, rx) = mpsc::unbounded_channel();
//         Self{
//             snap_sync_tx: tx,
//             snap_sync_rx: rx,
//         }
//     }
//
//     pub async fn snap_sync_allowed(&mut self) -> BlockNumber{
//         self.snap_sync_rx.recv().await.expect("We don't close the channel.")
//     }
//
//     pub fn allow_snap_sync(&mut self, block_number: BlockNumber) {
//         // We don't close the channel and the error is not expected.
//         let _ = self.snap_sync_tx.send(block_number);
//     }
// }
