use crate::providers::{BlockImportProvider, RpcProvider};
use crate::transaction_pool::FullChainApiWrapper;
use crate::{FullBackend, FullClient};
use cross_domain_message_gossip::ChainMsg;
use domain_client_block_preprocessor::inherents::CreateInherentDataProvider;
use domain_client_message_relayer::GossipMessageSink;
use domain_client_operator::{Operator, OperatorParams, OperatorStreams};
use domain_runtime_primitives::opaque::{Block, Header};
use domain_runtime_primitives::{Balance, Hash};
use futures::channel::mpsc;
use futures::Stream;
use pallet_transaction_payment_rpc::TransactionPaymentRuntimeApi;
use sc_client_api::{
    AuxStore, BlockBackend, BlockImportNotification, BlockchainEvents, ExecutorProvider,
    ProofProvider,
};
use sc_consensus::SharedBlockImport;
use sc_domains::{ExtensionsFactory, RuntimeExecutor};
use sc_network::{NetworkPeers, NotificationMetrics};
use sc_rpc_api::DenyUnsafe;
use sc_service::{
    BuildNetworkParams, Configuration as ServiceConfiguration, NetworkStarter, PartialComponents,
    PruningMode, SpawnTasksParams, TFullBackend, TaskManager,
};
use sc_telemetry::{Telemetry, TelemetryWorker, TelemetryWorkerHandle};
use sc_transaction_pool_api::OffchainTransactionPoolFactory;
use sc_utils::mpsc::{tracing_unbounded, TracingUnboundedReceiver};
use serde::de::DeserializeOwned;
use sp_api::{ApiExt, ConstructRuntimeApi, Metadata, ProvideRuntimeApi};
use sp_block_builder::BlockBuilder;
use sp_blockchain::{HeaderBackend, HeaderMetadata};
use sp_consensus::SyncOracle;
use sp_consensus_slots::Slot;
use sp_core::traits::SpawnEssentialNamed;
use sp_core::{Decode, Encode, H256};
use sp_domains::core_api::DomainCoreApi;
use sp_domains::{BundleProducerElectionApi, DomainId, DomainsApi, OperatorId};
use sp_domains_fraud_proof::FraudProofApi;
use sp_messenger::messages::ChainId;
use sp_messenger::{MessengerApi, RelayerApi};
use sp_mmr_primitives::MmrApi;
use sp_offchain::OffchainWorkerApi;
use sp_runtime::traits::{Block as BlockT, NumberFor};
use sp_session::SessionKeys;
use sp_transaction_pool::runtime_api::TaggedTransactionQueue;
use std::fmt::{Debug, Display};
use std::marker::PhantomData;
use std::str::FromStr;
use std::sync::Arc;
use sc_network_sync::block_relay_protocol::BlockDownloader;
use subspace_core_primitives::PotOutput;
use subspace_runtime_primitives::Nonce;
use subspace_service::sync_from_dsn::synchronizer::Synchronizer;
use substrate_frame_rpc_system::AccountNonceApi;
use subspace_service::domains::LastDomainBlockReceiptProvider;
use sc_service::ImportQueue;

pub type DomainOperator<Block, CBlock, CClient, RuntimeApi> = Operator<
    Block,
    CBlock,
    FullClient<Block, RuntimeApi>,
    CClient,
    FullPool<RuntimeApi>,
    FullBackend<Block>,
    RuntimeExecutor,
>;

/// Domain full node along with some other components.
pub struct NewFull<C, CodeExecutor, CBlock, CClient, RuntimeApi, AccountId>
where
    Block: BlockT,
    CBlock: BlockT,
    NumberFor<CBlock>: From<NumberFor<Block>>,
    CBlock::Hash: From<Hash>,
    CClient: HeaderBackend<CBlock>
        + BlockBackend<CBlock>
        + ProvideRuntimeApi<CBlock>
        + Send
        + Sync
        + 'static,
    CClient::Api:
        DomainsApi<CBlock, Header> + MessengerApi<CBlock, NumberFor<CBlock>, CBlock::Hash>,
    RuntimeApi: ConstructRuntimeApi<Block, FullClient<Block, RuntimeApi>> + Send + Sync + 'static,
    RuntimeApi::RuntimeApi: ApiExt<Block>
        + Metadata<Block>
        + AccountNonceApi<Block, AccountId, Nonce>
        + BlockBuilder<Block>
        + OffchainWorkerApi<Block>
        + SessionKeys<Block>
        + TaggedTransactionQueue<Block>
        + TransactionPaymentRuntimeApi<Block, Balance>
        + DomainCoreApi<Block>
        + MessengerApi<Block, NumberFor<CBlock>, CBlock::Hash>
        + RelayerApi<Block, NumberFor<Block>, NumberFor<CBlock>, CBlock::Hash>,
    AccountId: Encode + Decode,
{
    /// Task manager.
    pub task_manager: TaskManager,
    /// Full client.
    pub client: C,
    /// Backend.
    pub backend: Arc<FullBackend<Block>>,
    /// Code executor.
    pub code_executor: Arc<CodeExecutor>,
    /// Network service.
    pub network_service: Arc<sc_network::NetworkService<Block, <Block as BlockT>::Hash>>,
    /// Sync service.
    pub sync_service: Arc<sc_network_sync::SyncingService<Block>>,
    /// RPCHandlers to make RPC queries.
    pub rpc_handlers: sc_service::RpcHandlers,
    /// Network starter.
    pub network_starter: NetworkStarter,
    /// Operator.
    pub operator: DomainOperator<Block, CBlock, CClient, RuntimeApi>,
    /// Transaction pool
    pub transaction_pool: Arc<FullPool<RuntimeApi>>,
    _phantom_data: PhantomData<AccountId>,
}

pub type FullPool<RuntimeApi> =
    crate::transaction_pool::FullPool<Block, FullClient<Block, RuntimeApi>>;

/// Constructs a partial domain node.
#[allow(clippy::type_complexity)]
fn new_partial<RuntimeApi, CBlock, CClient, BIMP>(
    config: &ServiceConfiguration,
    consensus_client: Arc<CClient>,
    block_import_provider: &BIMP,
    confirmation_depth_k: NumberFor<CBlock>,
) -> Result<
    PartialComponents<
        FullClient<Block, RuntimeApi>,
        FullBackend<Block>,
        (),
        sc_consensus::DefaultImportQueue<Block>,
        FullPool<RuntimeApi>,
        (
            Option<Telemetry>,
            Option<TelemetryWorkerHandle>,
            Arc<RuntimeExecutor>,
            SharedBlockImport<Block>,
        ),
    >,
    sc_service::Error,
>
where
    CBlock: BlockT,
    NumberFor<CBlock>: From<NumberFor<Block>> + Into<u32>,
    CBlock::Hash: From<Hash> + Into<Hash>,
    CClient: HeaderBackend<CBlock>
        + BlockBackend<CBlock>
        + ProvideRuntimeApi<CBlock>
        + Send
        + Sync
        + 'static,
    CClient::Api: DomainsApi<CBlock, Header>
        + MessengerApi<CBlock, NumberFor<CBlock>, CBlock::Hash>
        + MmrApi<CBlock, H256, NumberFor<CBlock>>,
    RuntimeApi: ConstructRuntimeApi<Block, FullClient<Block, RuntimeApi>> + Send + Sync + 'static,
    RuntimeApi::RuntimeApi: TaggedTransactionQueue<Block>
        + MessengerApi<Block, NumberFor<CBlock>, CBlock::Hash>
        + ApiExt<Block>,
    BIMP: BlockImportProvider<Block, FullClient<Block, RuntimeApi>>,
{
    let telemetry = config
        .telemetry_endpoints
        .clone()
        .filter(|x| !x.is_empty())
        .map(|endpoints| -> Result<_, sc_telemetry::Error> {
            let worker = TelemetryWorker::new(16)?;
            let telemetry = worker.handle().new_telemetry(endpoints);
            Ok((worker, telemetry))
        })
        .transpose()?;

    let executor = sc_service::new_wasm_executor(config);

    let (client, backend, keystore_container, task_manager) = sc_service::new_full_parts(
        config,
        telemetry.as_ref().map(|(_, telemetry)| telemetry.handle()),
        executor.clone(),
    )?;
    let client = Arc::new(client);

    let executor = Arc::new(executor);
    client.execution_extensions().set_extensions_factory(
        ExtensionsFactory::<_, CBlock, Block, _>::new(
            consensus_client.clone(),
            executor.clone(),
            confirmation_depth_k.into(),
        ),
    );

    let telemetry_worker_handle = telemetry.as_ref().map(|(worker, _)| worker.handle());

    let telemetry = telemetry.map(|(worker, telemetry)| {
        task_manager
            .spawn_handle()
            .spawn("telemetry", None, worker.run());
        telemetry
    });

    let transaction_pool = crate::transaction_pool::new_full(config, &task_manager, client.clone());

    let block_import = SharedBlockImport::new(BlockImportProvider::block_import(
        block_import_provider,
        client.clone(),
    ));
    let import_queue = domain_client_consensus_relay_chain::import_queue(
        block_import.clone(),
        &task_manager.spawn_essential_handle(),
        config.prometheus_registry(),
    )?;

    let params = PartialComponents {
        backend,
        client,
        import_queue,
        keystore_container,
        task_manager,
        transaction_pool,
        select_chain: (),
        other: (telemetry, telemetry_worker_handle, executor, block_import),
    };

    Ok(params)
}

pub struct DomainParams<CBlock, CClient, IBNS, CIBNS, NSNS, ASS, Provider>
where
    CBlock: BlockT,
{
    pub domain_id: DomainId,
    pub domain_config: ServiceConfiguration,
    pub domain_created_at: NumberFor<CBlock>,
    pub maybe_operator_id: Option<OperatorId>,
    pub consensus_client: Arc<CClient>,
    pub consensus_network: Arc<dyn NetworkPeers + Send + Sync>,
    pub consensus_offchain_tx_pool_factory: OffchainTransactionPoolFactory<CBlock>,
    pub consensus_network_sync_oracle: Arc<dyn SyncOracle + Send + Sync>,
    pub operator_streams: OperatorStreams<CBlock, IBNS, CIBNS, NSNS, ASS>,
    pub gossip_message_sink: GossipMessageSink,
    pub domain_message_receiver: TracingUnboundedReceiver<ChainMsg>,
    pub provider: Provider,
    pub skip_empty_bundle_production: bool,
    pub consensus_state_pruning: PruningMode,
    pub skip_out_of_order_slot: bool,
    pub confirmation_depth_k: NumberFor<CBlock>,
}

/// Builds service for a domain full node.
pub async fn new_full<CBlock, CClient, IBNS, CIBNS, NSNS, ASS, RuntimeApi, AccountId, Provider>(
    domain_params: DomainParams<CBlock, CClient, IBNS, CIBNS, NSNS, ASS, Provider>,
    synchronizer: Option<Arc<Synchronizer>>,
    execution_receipt_provider: Box<dyn LastDomainBlockReceiptProvider<Block, CBlock>>
) -> sc_service::error::Result<
    NewFull<
        Arc<FullClient<Block, RuntimeApi>>,
        RuntimeExecutor,
        CBlock,
        CClient,
        RuntimeApi,
        AccountId,
    >,
>
where
    CBlock: BlockT,
    NumberFor<CBlock>: From<NumberFor<Block>> + Into<u32>,
    CBlock::Hash: From<Hash> + Into<Hash>,
    CClient: HeaderBackend<CBlock>
        + HeaderMetadata<CBlock, Error = sp_blockchain::Error>
        + BlockBackend<CBlock>
        + ProofProvider<CBlock>
        + ProvideRuntimeApi<CBlock>
        + BlockchainEvents<CBlock>
        + AuxStore
        + Send
        + Sync
        + 'static,
    CClient::Api: DomainsApi<CBlock, Header>
        + RelayerApi<CBlock, NumberFor<CBlock>, NumberFor<CBlock>, CBlock::Hash>
        + MessengerApi<CBlock, NumberFor<CBlock>, CBlock::Hash>
        + BundleProducerElectionApi<CBlock, subspace_runtime_primitives::Balance>
        + FraudProofApi<CBlock, Header>
        + MmrApi<CBlock, H256, NumberFor<CBlock>>,
    IBNS: Stream<Item = (NumberFor<CBlock>, mpsc::Sender<()>)> + Send + 'static,
    CIBNS: Stream<Item = BlockImportNotification<CBlock>> + Send + 'static,
    NSNS: Stream<Item = (Slot, PotOutput)> + Send + 'static,
    ASS: Stream<Item = mpsc::Sender<()>> + Send + 'static,
    RuntimeApi: ConstructRuntimeApi<Block, FullClient<Block, RuntimeApi>> + Send + Sync + 'static,
    RuntimeApi::RuntimeApi: ApiExt<Block>
        + Metadata<Block>
        + BlockBuilder<Block>
        + OffchainWorkerApi<Block>
        + SessionKeys<Block>
        + DomainCoreApi<Block>
        + MessengerApi<Block, NumberFor<CBlock>, CBlock::Hash>
        + TaggedTransactionQueue<Block>
        + AccountNonceApi<Block, AccountId, Nonce>
        + TransactionPaymentRuntimeApi<Block, Balance>
        + RelayerApi<Block, NumberFor<Block>, NumberFor<CBlock>, CBlock::Hash>,
    AccountId: DeserializeOwned
        + Encode
        + Decode
        + Clone
        + Debug
        + Display
        + FromStr
        + Sync
        + Send
        + 'static,
    Provider: RpcProvider<
            Block,
            FullClient<Block, RuntimeApi>,
            FullPool<RuntimeApi>,
            FullChainApiWrapper<Block, FullClient<Block, RuntimeApi>>,
            TFullBackend<Block>,
            AccountId,
            CreateInherentDataProvider<CClient, CBlock>,
        > + BlockImportProvider<Block, FullClient<Block, RuntimeApi>>
        + 'static,
{
    println!("domain new_full");
    let DomainParams {
        domain_id,
        maybe_operator_id,
        mut domain_config,
        domain_created_at,
        consensus_client,
        consensus_offchain_tx_pool_factory,
        consensus_network_sync_oracle,
        consensus_network,
        operator_streams,
        gossip_message_sink,
        domain_message_receiver,
        provider,
        skip_empty_bundle_production,
        consensus_state_pruning,
        skip_out_of_order_slot,
        confirmation_depth_k,
    } = domain_params;

    // TODO: Do we even need block announcement on domain node?
    // domain_config.announce_block = false;

    let params = new_partial(
        &domain_config,
        consensus_client.clone(),
        &provider,
        confirmation_depth_k,
    )?;

    let (mut telemetry, _telemetry_worker_handle, code_executor, block_import) = params.other;

    let client = params.client.clone();
    let backend = params.backend.clone();

    let transaction_pool = params.transaction_pool.clone();
    let mut task_manager = params.task_manager;
    let net_config = sc_network::config::FullNetworkConfiguration::new(&domain_config.network);
    let import_queue_service = params.import_queue.service();

    let (network_service, system_rpc_tx, tx_handler_controller, network_starter, sync_service, block_downloader) =
        crate::build_network(BuildNetworkParams {
            config: &domain_config,
            net_config,
            client: client.clone(),
            transaction_pool: transaction_pool.clone(),
            spawn_handle: task_manager.spawn_handle(),
            import_queue: params.import_queue,
            // TODO: we might want to re-enable this some day.
            block_announce_validator_builder: None,
            warp_sync_params: None,
            block_relay: None,
            metrics: NotificationMetrics::new(
                domain_config
                    .prometheus_config
                    .as_ref()
                    .map(|cfg| &cfg.registry),
            ),
        })?;

    let is_authority = domain_config.role.is_authority();
    let domain_state_pruning = domain_config.state_pruning.clone().unwrap_or_default();
    domain_config.rpc_id_provider = provider.rpc_id();
    let rpc_builder = {
        let deps = crate::rpc::FullDeps {
            client: client.clone(),
            pool: transaction_pool.clone(),
            graph: transaction_pool.pool().clone(),
            chain_spec: domain_config.chain_spec.cloned_box(),
            deny_unsafe: DenyUnsafe::Yes,
            network: network_service.clone(),
            sync: sync_service.clone(),
            is_authority,
            prometheus_registry: domain_config.prometheus_registry().cloned(),
            database_source: domain_config.database.clone(),
            task_spawner: task_manager.spawn_handle(),
            backend: backend.clone(),
            // This is required by the eth rpc to create pending state using the underlying
            // consensus provider. In our case, the consensus provider is empty and
            // as a result this is not used at all. Providing this just to make the api
            // compatible
            create_inherent_data_provider: CreateInherentDataProvider::new(
                consensus_client.clone(),
                // It is safe to pass empty consensus hash here as explained above
                None,
                domain_id,
            ),
        };

        let spawn_essential = task_manager.spawn_essential_handle();
        let rpc_deps = provider.deps(deps)?;
        Box::new(move |_, subscription_task_executor| {
            let spawn_essential = spawn_essential.clone();
            provider
                .rpc_builder(
                    rpc_deps.clone(),
                    subscription_task_executor,
                    spawn_essential,
                )
                .map_err(Into::into)
        })
    };

    let rpc_handlers = sc_service::spawn_tasks(SpawnTasksParams {
        rpc_builder,
        client: client.clone(),
        transaction_pool: transaction_pool.clone(),
        task_manager: &mut task_manager,
        config: domain_config,
        keystore: params.keystore_container.keystore(),
        backend: backend.clone(),
        network: network_service.clone(),
        system_rpc_tx,
        tx_handler_controller,
        sync_service: sync_service.clone(),
        telemetry: telemetry.as_mut(),
    })?;

    let spawn_essential = task_manager.spawn_essential_handle();
    let (bundle_sender, _bundle_receiver) = tracing_unbounded("domain_bundle_stream", 100);

    // let domain_confirmation_depth = consensus_client
    // .runtime_api()
    // .receipts_pruning_depth(consensus_client.info().best_hash)
    // .map_err(|err| sc_service::error::Error::Application(Box::new(err)))?
    // .into();
    // TODO: Implement when block tree is ready.
    let domain_confirmation_depth = 256u32;

    let operator = Operator::new(
        Box::new(spawn_essential.clone()),
        OperatorParams {
            domain_id,
            domain_created_at,
            consensus_client: consensus_client.clone(),
            consensus_offchain_tx_pool_factory,
            consensus_network_sync_oracle: consensus_network_sync_oracle.clone(),
            client: client.clone(),
            transaction_pool: transaction_pool.clone(),
            backend: backend.clone(),
            code_executor: code_executor.clone(),
            maybe_operator_id,
            keystore: params.keystore_container.keystore(),
            bundle_sender: Arc::new(bundle_sender),
            operator_streams,
            domain_confirmation_depth,
            block_import,
            skip_empty_bundle_production,
            skip_out_of_order_slot,
            sync_service: sync_service.clone(),
            network_request: Arc::clone(&network_service),
        },
        synchronizer,
        execution_receipt_provider,
        block_downloader,
        import_queue_service,
    )
    .await?;

    if is_authority {
        let relayer_worker = domain_client_message_relayer::worker::relay_domain_messages(
            domain_id,
            consensus_client.clone(),
            confirmation_depth_k,
            consensus_state_pruning,
            client.clone(),
            domain_state_pruning,
            // domain relayer will use consensus chain sync oracle instead of domain sync oracle
            // since domain sync oracle will always return `synced` due to force sync being set.
            consensus_network_sync_oracle.clone(),
            gossip_message_sink.clone(),
        );

        spawn_essential.spawn_essential_blocking("domain-relayer", None, Box::pin(relayer_worker));

        let channel_update_worker =
            domain_client_message_relayer::worker::gossip_channel_updates::<_, _, CBlock, _>(
                ChainId::Domain(domain_id),
                client.clone(),
                // domain will use consensus chain sync oracle instead of domain sync oracle
                // since domain sync oracle will always return `synced` due to force sync being set.
                consensus_network_sync_oracle,
                gossip_message_sink,
            );

        spawn_essential.spawn_essential_blocking(
            "domain-channel-update-worker",
            None,
            Box::pin(channel_update_worker),
        );
    }

    // Start cross domain message listener for domain
    let domain_listener =
        cross_domain_message_gossip::start_cross_chain_message_listener::<_, _, _, _, _, Block, _>(
            ChainId::Domain(domain_id),
            consensus_client.clone(),
            client.clone(),
            params.transaction_pool.clone(),
            consensus_network,
            domain_message_receiver,
            code_executor.clone(),
        );

    spawn_essential.spawn_essential_blocking(
        "domain-message-listener",
        None,
        Box::pin(domain_listener),
    );

    let new_full = NewFull {
        task_manager,
        client,
        backend,
        code_executor,
        network_service,
        sync_service,
        rpc_handlers,
        network_starter,
        operator,
        transaction_pool: params.transaction_pool,
        _phantom_data: Default::default(),
    };

    Ok(new_full)
}
