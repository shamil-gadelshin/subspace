use prometheus_endpoint::Registry;
use sc_consensus::import_queue::Origin;
use sc_consensus::{
    BasicQueue, BoxJustificationImport, DefaultImportQueue, ImportQueue, IncomingBlock, Link,
};
use sp_api::{BlockT, ProvideRuntimeApi, TransactionFor};
use sp_consensus::BlockOrigin;
use sp_core::traits::SpawnEssentialNamed;
use sp_runtime::traits::NumberFor;
use sp_runtime::Justifications;
use std::task::Context;

/// Subspace import queue
pub struct SubspaceImportQueue<Block, Client>
where
    Block: BlockT,
    Client: ProvideRuntimeApi<Block> + Send + Sync + 'static,
{
    inner: DefaultImportQueue<Block, Client>,
}

impl<Block, Client> SubspaceImportQueue<Block, Client>
where
    Block: BlockT,
    Client: ProvideRuntimeApi<Block> + Send + Sync + 'static,
{
    /// Instantiate a new basic queue, with given verifier.
    ///
    /// This creates a background task, and calls `on_start` on the justification importer.
    pub fn new<Verifier, Spawner, BlockImport>(
        verifier: Verifier,
        block_import: BlockImport,
        justification_import: Option<BoxJustificationImport<Block>>,
        spawner: &Spawner,
        prometheus_registry: Option<&Registry>,
    ) -> Self
    where
        Verifier: sc_consensus::Verifier<Block> + 'static,
        Spawner: SpawnEssentialNamed,
        BlockImport: sc_consensus::BlockImport<
                Block,
                Error = sp_consensus::Error,
                Transaction = TransactionFor<Client, Block>,
            > + Send
            + Sync
            + 'static,
    {
        Self {
            inner: BasicQueue::new(
                verifier,
                Box::new(block_import),
                justification_import,
                spawner,
                prometheus_registry,
            ),
        }
    }
}

impl<Block, Client> ImportQueue<Block> for SubspaceImportQueue<Block, Client>
where
    Block: BlockT,
    Client: ProvideRuntimeApi<Block> + Send + Sync + 'static,
{
    fn import_blocks(&mut self, origin: BlockOrigin, blocks: Vec<IncomingBlock<Block>>) {
        self.inner.import_blocks(origin, blocks)
    }

    fn import_justifications(
        &mut self,
        who: Origin,
        hash: Block::Hash,
        number: NumberFor<Block>,
        justifications: Justifications,
    ) {
        self.inner
            .import_justifications(who, hash, number, justifications)
    }

    fn poll_actions(&mut self, cx: &mut Context, link: &mut dyn Link<Block>) {
        self.inner.poll_actions(cx, link)
    }
}
