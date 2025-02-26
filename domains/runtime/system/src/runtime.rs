use codec::Decode;
use domain_runtime_primitives::{opaque, RelayerId};
pub use domain_runtime_primitives::{
    AccountId, Address, Balance, BlockNumber, Hash, Index, Signature,
};
use frame_support::dispatch::DispatchClass;
use frame_support::traits::{ConstU16, ConstU32, Everything};
use frame_support::weights::constants::{BlockExecutionWeight, ExtrinsicBaseWeight, RocksDbWeight};
use frame_support::weights::{ConstantMultiplier, IdentityFee, Weight};
use frame_support::{construct_runtime, parameter_types};
use frame_system::limits::{BlockLength, BlockWeights};
use pallet_transporter::EndpointHandler;
use sp_api::impl_runtime_apis;
use sp_core::crypto::KeyTypeId;
use sp_core::{OpaqueMetadata, H256};
use sp_domains::bundle_election::BundleElectionSolverParams;
use sp_domains::fraud_proof::FraudProof;
use sp_domains::transaction::PreValidationObject;
use sp_domains::{DomainId, ExecutorPublicKey, SignedOpaqueBundle};
use sp_messenger::endpoint::{Endpoint, EndpointHandler as EndpointHandlerT, EndpointId};
use sp_messenger::messages::{
    CrossDomainMessage, ExtractedStateRootsFromProof, MessageId, RelayerMessagesWithStorageKey,
};
use sp_runtime::traits::{AccountIdLookup, BlakeTwo256, Block as BlockT, NumberFor};
use sp_runtime::transaction_validity::{TransactionSource, TransactionValidity};
#[cfg(any(feature = "std", test))]
pub use sp_runtime::BuildStorage;
use sp_runtime::{create_runtime_str, generic, impl_opaque_keys, ApplyExtrinsicResult};
pub use sp_runtime::{MultiAddress, Perbill, Permill};
use sp_std::marker::PhantomData;
use sp_std::prelude::*;
#[cfg(feature = "std")]
use sp_version::NativeVersion;
use sp_version::RuntimeVersion;
use subspace_runtime_primitives::{SHANNON, SSC};

// Make core-payments WASM runtime available.
include!(concat!(env!("OUT_DIR"), "/core_payments_wasm_bundle.rs"));
include!(concat!(env!("OUT_DIR"), "/core_eth_relay_wasm_bundle.rs"));

/// Block header type as expected by this runtime.
pub type Header = generic::Header<BlockNumber, BlakeTwo256>;

/// Block type as expected by this runtime.
pub type Block = generic::Block<Header, UncheckedExtrinsic>;

/// A Block signed with a Justification
pub type SignedBlock = generic::SignedBlock<Block>;

/// BlockId type as expected by this runtime.
pub type BlockId = generic::BlockId<Block>;

/// The SignedExtension to the basic transaction logic.
pub type SignedExtra = (
    frame_system::CheckNonZeroSender<Runtime>,
    frame_system::CheckSpecVersion<Runtime>,
    frame_system::CheckTxVersion<Runtime>,
    frame_system::CheckGenesis<Runtime>,
    frame_system::CheckMortality<Runtime>,
    frame_system::CheckNonce<Runtime>,
    frame_system::CheckWeight<Runtime>,
    pallet_transaction_payment::ChargeTransactionPayment<Runtime>,
);

/// Unchecked extrinsic type as expected by this runtime.
pub type UncheckedExtrinsic =
    generic::UncheckedExtrinsic<Address, RuntimeCall, Signature, SignedExtra>;

/// Extrinsic type that has already been checked.
pub type CheckedExtrinsic = generic::CheckedExtrinsic<AccountId, RuntimeCall, SignedExtra>;

/// Executive: handles dispatch to the various modules.
pub type Executive = domain_pallet_executive::Executive<
    Runtime,
    Block,
    frame_system::ChainContext<Runtime>,
    Runtime,
    AllPalletsWithSystem,
    Runtime,
>;

impl_opaque_keys! {
    pub struct SessionKeys {
        /// Primarily used for adding the executor authority key into the keystore in the dev mode.
        pub executor: sp_domains::ExecutorKey,
    }
}

#[sp_version::runtime_version]
pub const VERSION: RuntimeVersion = RuntimeVersion {
    spec_name: create_runtime_str!("subspace-system-domain"),
    impl_name: create_runtime_str!("subspace-system-domain"),
    authoring_version: 0,
    spec_version: 2,
    impl_version: 0,
    apis: RUNTIME_API_VERSIONS,
    transaction_version: 0,
    state_version: 0,
};

/// The existential deposit. Same with the one on primary chain.
pub const EXISTENTIAL_DEPOSIT: Balance = 500 * SHANNON;

/// We assume that ~5% of the block weight is consumed by `on_initialize` handlers. This is
/// used to limit the maximal weight of a single extrinsic.
const AVERAGE_ON_INITIALIZE_RATIO: Perbill = Perbill::from_percent(5);

/// We allow `Normal` extrinsics to fill up the block up to 75%, the rest can be used by
/// `Operational` extrinsics.
const NORMAL_DISPATCH_RATIO: Perbill = Perbill::from_percent(75);

/// TODO: Proper max block weight
const MAXIMUM_BLOCK_WEIGHT: Weight = Weight::MAX;

/// The version information used to identify this runtime when compiled natively.
#[cfg(feature = "std")]
pub fn native_version() -> NativeVersion {
    NativeVersion {
        runtime_version: VERSION,
        can_author_with: Default::default(),
    }
}

parameter_types! {
    pub const Version: RuntimeVersion = VERSION;
    pub const BlockHashCount: BlockNumber = 2400;

    // This part is copied from Substrate's `bin/node/runtime/src/lib.rs`.
    //  The `RuntimeBlockLength` and `RuntimeBlockWeights` exist here because the
    // `DeletionWeightLimit` and `DeletionQueueDepth` depend on those to parameterize
    // the lazy contract deletion.
    pub RuntimeBlockLength: BlockLength =
        BlockLength::max_with_normal_ratio(5 * 1024 * 1024, NORMAL_DISPATCH_RATIO);
    pub RuntimeBlockWeights: BlockWeights = BlockWeights::builder()
        .base_block(BlockExecutionWeight::get())
        .for_class(DispatchClass::all(), |weights| {
            weights.base_extrinsic = ExtrinsicBaseWeight::get();
        })
        .for_class(DispatchClass::Normal, |weights| {
            weights.max_total = Some(NORMAL_DISPATCH_RATIO * MAXIMUM_BLOCK_WEIGHT);
        })
        .for_class(DispatchClass::Operational, |weights| {
            weights.max_total = Some(MAXIMUM_BLOCK_WEIGHT);
            // Operational transactions have some extra reserved space, so that they
            // are included even if block reached `MAXIMUM_BLOCK_WEIGHT`.
            weights.reserved = Some(
                MAXIMUM_BLOCK_WEIGHT - NORMAL_DISPATCH_RATIO * MAXIMUM_BLOCK_WEIGHT
            );
        })
        .avg_block_initialization(AVERAGE_ON_INITIALIZE_RATIO)
        .build_or_panic();
}

// Configure FRAME pallets to include in runtime.

impl frame_system::Config for Runtime {
    /// The identifier used to distinguish between accounts.
    type AccountId = AccountId;
    /// The aggregated dispatch type that is available for extrinsics.
    type RuntimeCall = RuntimeCall;
    /// The lookup mechanism to get account ID from whatever is passed in dispatchers.
    type Lookup = AccountIdLookup<AccountId, ()>;
    /// The index type for storing how many extrinsics an account has signed.
    type Index = Index;
    /// The index type for blocks.
    type BlockNumber = BlockNumber;
    /// The type for hashing blocks and tries.
    type Hash = Hash;
    /// The hashing algorithm used.
    type Hashing = BlakeTwo256;
    /// The header type.
    type Header = generic::Header<BlockNumber, BlakeTwo256>;
    /// The ubiquitous event type.
    type RuntimeEvent = RuntimeEvent;
    /// The ubiquitous origin type.
    type RuntimeOrigin = RuntimeOrigin;
    /// Maximum number of block number to block hash mappings to keep (oldest pruned first).
    type BlockHashCount = BlockHashCount;
    /// Runtime version.
    type Version = Version;
    /// Converts a module to an index of this module in the runtime.
    type PalletInfo = PalletInfo;
    /// The data to be stored in an account.
    type AccountData = pallet_balances::AccountData<Balance>;
    /// What to do if a new account is created.
    type OnNewAccount = ();
    /// What to do if an account is fully reaped from the system.
    type OnKilledAccount = ();
    /// The weight of database operations that the runtime can invoke.
    type DbWeight = RocksDbWeight;
    /// The basic call filter to use in dispatchable.
    type BaseCallFilter = Everything;
    /// Weight information for the extrinsics of this pallet.
    type SystemWeightInfo = ();
    /// Block & extrinsics weights: base values and limits.
    type BlockWeights = RuntimeBlockWeights;
    /// The maximum length of a block (in bytes).
    type BlockLength = RuntimeBlockLength;
    /// This is used as an identifier of the chain. 42 is the generic substrate prefix.
    type SS58Prefix = ConstU16<42>;
    /// The action to take on a Runtime Upgrade
    type OnSetCode = ();
    type MaxConsumers = ConstU32<16>;
}

parameter_types! {
    pub const ExistentialDeposit: Balance = EXISTENTIAL_DEPOSIT;
    pub const MaxLocks: u32 = 50;
    pub const MaxReserves: u32 = 50;
}

impl pallet_balances::Config for Runtime {
    type MaxLocks = MaxLocks;
    /// The type for recording an account's balance.
    type Balance = Balance;
    /// The ubiquitous event type.
    type RuntimeEvent = RuntimeEvent;
    type DustRemoval = ();
    type ExistentialDeposit = ExistentialDeposit;
    type AccountStore = System;
    type WeightInfo = pallet_balances::weights::SubstrateWeight<Runtime>;
    type MaxReserves = MaxReserves;
    type ReserveIdentifier = [u8; 8];
    type FreezeIdentifier = ();
    type MaxFreezes = ();
    type HoldIdentifier = ();
    type MaxHolds = ();
}

parameter_types! {
    pub const TransactionByteFee: Balance = 1;
    pub const OperationalFeeMultiplier: u8 = 5;
}

impl pallet_transaction_payment::Config for Runtime {
    type RuntimeEvent = RuntimeEvent;
    type OnChargeTransaction = pallet_transaction_payment::CurrencyAdapter<Balances, ()>;
    type WeightToFee = IdentityFee<Balance>;
    type LengthToFee = ConstantMultiplier<Balance, TransactionByteFee>;
    type FeeMultiplierUpdate = ();
    type OperationalFeeMultiplier = OperationalFeeMultiplier;
}

impl domain_pallet_executive::Config for Runtime {
    type RuntimeEvent = RuntimeEvent;
    type RuntimeCall = RuntimeCall;
}

parameter_types! {
    // TODO: proper parameters
    pub const MinExecutorStake: Balance = SSC;
    pub const MaxExecutorStake: Balance = 1000 * SSC;
    pub const MinExecutors: u32 = 1;
    pub const MaxExecutors: u32 = 10;
    // One hour in blocks.
    pub const EpochDuration: BlockNumber = 3600 / 6;
    pub const MaxWithdrawals: u32 = 1;
    // One day in blocks.
    pub const WithdrawalDuration: BlockNumber = 3600 * 24 / 6;
}

impl pallet_executor_registry::Config for Runtime {
    type RuntimeEvent = RuntimeEvent;
    type Currency = Balances;
    type StakeWeight = sp_domains::StakeWeight;
    type MinExecutorStake = MinExecutorStake;
    type MaxExecutorStake = MaxExecutorStake;
    type MinExecutors = MinExecutors;
    type MaxExecutors = MaxExecutors;
    type MaxWithdrawals = MaxWithdrawals;
    type WithdrawalDuration = WithdrawalDuration;
    type EpochDuration = EpochDuration;
    type OnNewEpoch = DomainRegistry;
}

parameter_types! {
    pub const MinDomainDeposit: Balance = 10 * SSC;
    pub const MaxDomainDeposit: Balance = 1000 * SSC;
    pub const MinDomainOperatorStake: Balance = 10 * SSC;
    pub const ReceiptsPruningDepth: BlockNumber = 256;
    pub const MaximumReceiptDrift: BlockNumber = 128;
}

impl pallet_domain_registry::Config for Runtime {
    type RuntimeEvent = RuntimeEvent;
    type Currency = Balances;
    type StakeWeight = sp_domains::StakeWeight;
    type ExecutorRegistry = ExecutorRegistry;
    type MinDomainDeposit = MinDomainDeposit;
    type MaxDomainDeposit = MaxDomainDeposit;
    type MinDomainOperatorStake = MinDomainOperatorStake;
}

impl pallet_receipts::Config for Runtime {
    type RuntimeEvent = RuntimeEvent;
    type DomainHash = domain_runtime_primitives::Hash;
    type MaximumReceiptDrift = MaximumReceiptDrift;
    type ReceiptsPruningDepth = ReceiptsPruningDepth;
}

parameter_types! {
    pub const StateRootsBound: u32 = 50;
    pub const RelayConfirmationDepth: BlockNumber = 7;
}

pub struct DomainInfo;

impl sp_messenger::endpoint::DomainInfo<BlockNumber, Hash, Hash> for DomainInfo {
    fn domain_best_number(domain_id: DomainId) -> Option<BlockNumber> {
        Some(Receipts::head_receipt_number(domain_id))
    }

    fn domain_state_root(domain_id: DomainId, number: BlockNumber, hash: Hash) -> Option<Hash> {
        Receipts::domain_state_root_at(domain_id, number, hash)
    }
}

parameter_types! {
    pub const MaximumRelayers: u32 = 100;
    pub const RelayerDeposit: Balance = 100 * SSC;
    pub const SystemDomainId: DomainId = DomainId::SYSTEM;
}

impl pallet_messenger::Config for Runtime {
    type RuntimeEvent = RuntimeEvent;
    type SelfDomainId = SystemDomainId;

    fn get_endpoint_response_handler(
        endpoint: &Endpoint,
    ) -> Option<Box<dyn EndpointHandlerT<MessageId>>> {
        if endpoint == &Endpoint::Id(TransporterEndpointId::get()) {
            Some(Box::new(EndpointHandler(PhantomData::<Runtime>::default())))
        } else {
            None
        }
    }

    type Currency = Balances;
    type MaximumRelayers = MaximumRelayers;
    type RelayerDeposit = RelayerDeposit;
    type DomainInfo = DomainInfo;
    type ConfirmationDepth = RelayConfirmationDepth;
}

impl<C> frame_system::offchain::SendTransactionTypes<C> for Runtime
where
    RuntimeCall: From<C>,
{
    type Extrinsic = UncheckedExtrinsic;
    type OverarchingCall = RuntimeCall;
}

parameter_types! {
    pub const TransporterEndpointId: EndpointId = 1;
}

impl pallet_transporter::Config for Runtime {
    type RuntimeEvent = RuntimeEvent;
    type SelfDomainId = SystemDomainId;
    type SelfEndpointId = TransporterEndpointId;
    type Currency = Balances;
    type Sender = Messenger;
}

impl pallet_sudo::Config for Runtime {
    type RuntimeEvent = RuntimeEvent;
    type RuntimeCall = RuntimeCall;
}

// Create the runtime by composing the FRAME pallets that were previously configured.
//
// NOTE: Currently domain runtime does not naturally support the pallets with inherent extrinsics.
construct_runtime!(
    pub struct Runtime where
        Block = Block,
        NodeBlock = opaque::Block,
        UncheckedExtrinsic = UncheckedExtrinsic,
    {
        // System support stuff.
        System: frame_system = 0,
        ExecutivePallet: domain_pallet_executive = 1,

        // Monetary stuff.
        Balances: pallet_balances = 2,
        TransactionPayment: pallet_transaction_payment = 3,

        // System domain.
        //
        // Must be after Balances pallet so that its genesis is built after the Balances genesis is
        // built.
        ExecutorRegistry: pallet_executor_registry = 4,
        Receipts: pallet_receipts = 9,
        DomainRegistry: pallet_domain_registry = 5,
        // Note: Indexes should be used by all other core domain for proper xdm decode.
        Messenger: pallet_messenger = 6,
        Transporter: pallet_transporter = 7,

        // Sudo account
        Sudo: pallet_sudo = 100,
    }
);

#[cfg(feature = "runtime-benchmarks")]
frame_benchmarking::define_benchmarks!(
    [frame_benchmarking, BaselineBench::<Runtime>]
    [frame_system, SystemBench::<Runtime>]
    [pallet_balances, Balances]
);

impl_runtime_apis! {
    impl sp_api::Core<Block> for Runtime {
        fn version() -> RuntimeVersion {
            VERSION
        }

        fn execute_block(block: Block) {
            Executive::execute_block(block)
        }

        fn initialize_block(header: &<Block as BlockT>::Header) {
            Executive::initialize_block(header)
        }
    }

    impl sp_api::Metadata<Block> for Runtime {
        fn metadata() -> OpaqueMetadata {
            OpaqueMetadata::new(Runtime::metadata().into())
        }

        fn metadata_at_version(version: u32) -> Option<OpaqueMetadata> {
            Runtime::metadata_at_version(version)
        }

        fn metadata_versions() -> sp_std::vec::Vec<u32> {
            Runtime::metadata_versions()
        }
    }

    impl sp_block_builder::BlockBuilder<Block> for Runtime {
        fn apply_extrinsic(extrinsic: <Block as BlockT>::Extrinsic) -> ApplyExtrinsicResult {
            Executive::apply_extrinsic(extrinsic)
        }

        fn finalize_block() -> <Block as BlockT>::Header {
            Executive::finalize_block()
        }

        fn inherent_extrinsics(data: sp_inherents::InherentData) -> Vec<<Block as BlockT>::Extrinsic> {
            data.create_extrinsics()
        }

        fn check_inherents(
            block: Block,
            data: sp_inherents::InherentData,
        ) -> sp_inherents::CheckInherentsResult {
            data.check_extrinsics(&block)
        }
    }

    impl sp_transaction_pool::runtime_api::TaggedTransactionQueue<Block> for Runtime {
        fn validate_transaction(
            source: TransactionSource,
            tx: <Block as BlockT>::Extrinsic,
            block_hash: <Block as BlockT>::Hash,
        ) -> TransactionValidity {
            Executive::validate_transaction(source, tx, block_hash)
        }
    }

    impl sp_offchain::OffchainWorkerApi<Block> for Runtime {
        fn offchain_worker(header: &<Block as BlockT>::Header) {
            Executive::offchain_worker(header)
        }
    }

    impl sp_session::SessionKeys<Block> for Runtime {
        fn generate_session_keys(seed: Option<Vec<u8>>) -> Vec<u8> {
            SessionKeys::generate(seed)
        }

        fn decode_session_keys(
            encoded: Vec<u8>,
        ) -> Option<Vec<(Vec<u8>, KeyTypeId)>> {
            SessionKeys::decode_into_raw_public_keys(&encoded)
        }
    }

    impl frame_system_rpc_runtime_api::AccountNonceApi<Block, AccountId, Index> for Runtime {
        fn account_nonce(account: AccountId) -> Index {
            System::account_nonce(account)
        }
    }

    impl pallet_transaction_payment_rpc_runtime_api::TransactionPaymentApi<Block, Balance> for Runtime {
        fn query_info(
            uxt: <Block as BlockT>::Extrinsic,
            len: u32,
        ) -> pallet_transaction_payment_rpc_runtime_api::RuntimeDispatchInfo<Balance> {
            TransactionPayment::query_info(uxt, len)
        }
        fn query_fee_details(
            uxt: <Block as BlockT>::Extrinsic,
            len: u32,
        ) -> pallet_transaction_payment::FeeDetails<Balance> {
            TransactionPayment::query_fee_details(uxt, len)
        }
        fn query_weight_to_fee(weight: Weight) -> Balance {
            TransactionPayment::weight_to_fee(weight)
        }
        fn query_length_to_fee(length: u32) -> Balance {
            TransactionPayment::length_to_fee(length)
        }
    }

    impl domain_runtime_primitives::DomainCoreApi<Block, AccountId> for Runtime {
        fn extract_signer(
            extrinsics: Vec<<Block as BlockT>::Extrinsic>,
        ) -> Vec<(Option<AccountId>, <Block as BlockT>::Extrinsic)> {
            use domain_runtime_primitives::Signer;
            let lookup = frame_system::ChainContext::<Runtime>::default();
            extrinsics.into_iter().map(|xt| (xt.signer(&lookup), xt)).collect()
        }

        fn intermediate_roots() -> Vec<[u8; 32]> {
            ExecutivePallet::intermediate_roots()
        }

        fn initialize_block_with_post_state_root(header: &<Block as BlockT>::Header) -> Vec<u8> {
            Executive::initialize_block(header);
            Executive::storage_root()
        }

        fn apply_extrinsic_with_post_state_root(extrinsic: <Block as BlockT>::Extrinsic) -> Vec<u8> {
            let _ = Executive::apply_extrinsic(extrinsic);
            Executive::storage_root()
        }

        fn construct_set_code_extrinsic(code: Vec<u8>) -> Vec<u8> {
            use codec::Encode;
            let set_code_call = frame_system::Call::set_code { code };
            UncheckedExtrinsic::new_unsigned(
                domain_pallet_executive::Call::sudo_unchecked_weight_unsigned {
                    call: Box::new(set_code_call.into()),
                    weight: Weight::zero(),
                }.into()
            ).encode()
        }
    }

    impl sp_receipts::ReceiptsApi<Block, domain_runtime_primitives::Hash> for Runtime {
        fn execution_trace(domain_id: DomainId, receipt_hash: H256) -> Vec<domain_runtime_primitives::Hash> {
            Receipts::receipts(domain_id, receipt_hash).map(|receipt| receipt.trace).unwrap_or_default()
        }

        fn state_root(
            domain_id: DomainId,
            domain_block_number: BlockNumber,
            domain_block_hash: Hash,
        ) -> Option<Hash> {
            Receipts::state_root((domain_id, domain_block_number, domain_block_hash))
        }

        fn primary_hash(domain_id: DomainId, domain_block_number: BlockNumber) -> Option<Hash> {
            Receipts::primary_hash(domain_id, domain_block_number)
        }
    }

    impl system_runtime_primitives::SystemDomainApi<Block, BlockNumber, Hash> for Runtime {
        fn construct_submit_core_bundle_extrinsics(
            signed_opaque_bundles: Vec<SignedOpaqueBundle<BlockNumber, Hash, <Block as BlockT>::Hash>>,
        ) -> Vec<Vec<u8>> {
            use codec::Encode;
            signed_opaque_bundles
                .into_iter()
                .map(|signed_opaque_bundle| {
                    UncheckedExtrinsic::new_unsigned(
                        pallet_domain_registry::Call::submit_core_bundle {
                            signed_opaque_bundle
                        }.into()
                    ).encode()
                })
                .collect()
        }

        fn bundle_election_solver_params(domain_id: DomainId) -> BundleElectionSolverParams {
            if domain_id.is_system() {
                BundleElectionSolverParams {
                    authorities: ExecutorRegistry::authorities().into(),
                    total_stake_weight: ExecutorRegistry::total_stake_weight(),
                    slot_probability: ExecutorRegistry::slot_probability(),
                }
            } else {
                match (
                    DomainRegistry::domain_authorities(domain_id),
                    DomainRegistry::domain_total_stake_weight(domain_id),
                    DomainRegistry::domain_slot_probability(domain_id),
                ) {
                    (authorities, Some(total_stake_weight), Some(slot_probability)) => {
                        BundleElectionSolverParams {
                            authorities,
                            total_stake_weight,
                            slot_probability,
                        }
                    }
                    _ => BundleElectionSolverParams::empty(),
                }
            }
        }

        fn core_bundle_election_storage_keys(
            domain_id: DomainId,
            executor_public_key: ExecutorPublicKey,
        ) -> Option<Vec<Vec<u8>>> {
            let executor = ExecutorRegistry::key_owner(&executor_public_key)?;
            let mut storage_keys = DomainRegistry::core_bundle_election_storage_keys(domain_id, executor);
            storage_keys.push(ExecutorRegistry::key_owner_hashed_key_for(&executor_public_key));
            Some(storage_keys)
        }

        fn head_receipt_number(domain_id: DomainId) -> NumberFor<Block> {
            DomainRegistry::head_receipt_number(domain_id)
        }

        fn oldest_receipt_number(domain_id: DomainId) -> NumberFor<Block> {
            DomainRegistry::oldest_receipt_number(domain_id)
        }

        fn maximum_receipt_drift() -> NumberFor<Block> {
            MaximumReceiptDrift::get()
        }

        fn submit_fraud_proof_unsigned(fraud_proof: FraudProof<NumberFor<Block>, Hash>) {
            DomainRegistry::submit_fraud_proof_unsigned(fraud_proof)
        }

        fn core_domain_state_root_at(domain_id: DomainId, number: BlockNumber, domain_hash: Hash) -> Option<Hash> {
            Receipts::domain_state_root_at(domain_id, number, domain_hash)
        }
    }

    impl sp_messenger::RelayerApi<Block, RelayerId, BlockNumber> for Runtime {
        fn domain_id() -> DomainId {
            SystemDomainId::get()
        }

        fn relay_confirmation_depth() -> BlockNumber {
            RelayConfirmationDepth::get()
        }

        fn domain_best_number(domain_id: DomainId) -> Option<BlockNumber> {
            Some(Receipts::head_receipt_number(domain_id))
        }

        fn domain_state_root(domain_id: DomainId, number: BlockNumber, hash: Hash) -> Option<Hash>{
            Receipts::domain_state_root_at(domain_id, number, hash)
        }

        fn relayer_assigned_messages(relayer_id: RelayerId) -> RelayerMessagesWithStorageKey {
            Messenger::relayer_assigned_messages(relayer_id)
        }

        fn outbox_message_unsigned(msg: CrossDomainMessage<BlockNumber, <Block as BlockT>::Hash, <Block as BlockT>::Hash>) -> Option<<Block as BlockT>::Extrinsic> {
            Messenger::outbox_message_unsigned(msg)
        }

        fn inbox_response_message_unsigned(msg: CrossDomainMessage<BlockNumber, <Block as BlockT>::Hash, <Block as BlockT>::Hash>) -> Option<<Block as BlockT>::Extrinsic> {
            Messenger::inbox_response_message_unsigned(msg)
        }

        fn should_relay_outbox_message(dst_domain_id: DomainId, msg_id: MessageId) -> bool {
            Messenger::should_relay_outbox_message(dst_domain_id, msg_id)
        }

        fn should_relay_inbox_message_response(dst_domain_id: DomainId, msg_id: MessageId) -> bool {
            Messenger::should_relay_inbox_message_response(dst_domain_id, msg_id)
        }
    }

    impl sp_messenger::MessengerApi<Block, BlockNumber> for Runtime {
        fn extract_xdm_proof_state_roots(
            extrinsic: Vec<u8>,
        ) -> Option<ExtractedStateRootsFromProof<BlockNumber, <Block as BlockT>::Hash, <Block as BlockT>::Hash>> {
            extract_xdm_proof_state_roots(extrinsic)
        }

        fn confirmation_depth() -> BlockNumber {
            RelayConfirmationDepth::get()
        }
    }

    impl sp_domains::transaction::PreValidationObjectApi<Block, domain_runtime_primitives::Hash> for Runtime {
        fn extract_pre_validation_object(
            extrinsic: <Block as BlockT>::Extrinsic,
        ) -> PreValidationObject<Block, domain_runtime_primitives::Hash> {
                match extrinsic.function {
                    RuntimeCall::DomainRegistry(pallet_domain_registry::Call::submit_fraud_proof { fraud_proof }) => {
                        PreValidationObject::FraudProof(fraud_proof)
                    }
                    _ => PreValidationObject::Null,
                }
        }
    }


    #[cfg(feature = "runtime-benchmarks")]
    impl frame_benchmarking::Benchmark<Block> for Runtime {
        fn benchmark_metadata(extra: bool) -> (
            Vec<frame_benchmarking::BenchmarkList>,
            Vec<frame_support::traits::StorageInfo>,
        ) {
            use frame_benchmarking::{baseline, Benchmarking, BenchmarkList};
            use frame_support::traits::StorageInfoTrait;
            use frame_system_benchmarking::Pallet as SystemBench;
            use baseline::Pallet as BaselineBench;

            let mut list = Vec::<BenchmarkList>::new();
            list_benchmarks!(list, extra);

            let storage_info = AllPalletsWithSystem::storage_info();

            (list, storage_info)
        }

        fn dispatch_benchmark(
            config: frame_benchmarking::BenchmarkConfig
        ) -> Result<Vec<frame_benchmarking::BenchmarkBatch>, sp_runtime::RuntimeString> {
            use frame_benchmarking::{baseline, Benchmarking, BenchmarkBatch, TrackedStorageKey};

            use frame_system_benchmarking::Pallet as SystemBench;
            use baseline::Pallet as BaselineBench;

            impl frame_system_benchmarking::Config for Runtime {}
            impl baseline::Config for Runtime {}

            use frame_support::traits::WhitelistedStorageKeys;
            let whitelist: Vec<TrackedStorageKey> = AllPalletsWithSystem::whitelisted_storage_keys();

            let mut batches = Vec::<BenchmarkBatch>::new();
            let params = (&config, &whitelist);
            add_benchmarks!(params, batches);

            Ok(batches)
        }
    }
}

fn extract_xdm_proof_state_roots(
    encoded_ext: Vec<u8>,
) -> Option<ExtractedStateRootsFromProof<BlockNumber, Hash, Hash>> {
    if let Ok(ext) = UncheckedExtrinsic::decode(&mut encoded_ext.as_slice()) {
        match &ext.function {
            RuntimeCall::Messenger(pallet_messenger::Call::relay_message { msg }) => {
                msg.extract_state_roots_from_proof::<BlakeTwo256>()
            }
            RuntimeCall::Messenger(pallet_messenger::Call::relay_message_response { msg }) => {
                msg.extract_state_roots_from_proof::<BlakeTwo256>()
            }
            _ => None,
        }
    } else {
        None
    }
}
