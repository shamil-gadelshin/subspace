use crate::{ExecutionReceiptFor, SignedExecutionReceiptFor};
use cirrus_block_builder::{BlockBuilder, BuiltBlock, RecordProof};
use cirrus_primitives::{AccountId, SecondaryApi};
use codec::{Decode, Encode};
use rand::{seq::SliceRandom, SeedableRng};
use rand_chacha::ChaCha8Rng;
use sc_client_api::{AuxStore, BlockBackend};
use sc_consensus::{
	BlockImport, BlockImportParams, ForkChoiceStrategy, ImportResult, StateAction, StorageChanges,
};
use sc_network::NetworkService;
use sc_utils::mpsc::TracingUnboundedSender;
use sp_api::{NumberFor, ProvideRuntimeApi, TransactionFor};
use sp_blockchain::HeaderBackend;
use sp_consensus::BlockOrigin;
use sp_core::ByteArray;
use sp_executor::{
	ExecutionReceipt, ExecutorApi, ExecutorId, ExecutorSignature, OpaqueBundle,
	SignedExecutionReceipt,
};
use sp_keystore::{SyncCryptoStore, SyncCryptoStorePtr};
use sp_runtime::{
	generic::BlockId,
	traits::{Block as BlockT, Header as HeaderT, One},
	RuntimeAppPublic,
};
use std::{
	borrow::Cow,
	collections::{BTreeMap, VecDeque},
	fmt::Debug,
	marker::PhantomData,
	sync::Arc,
};
use subspace_core_primitives::Randomness;

const LOG_TARGET: &str = "bundle-processor";

/// Shuffles the extrinsics in a deterministic way.
///
/// The extrinsics are grouped by the signer. The extrinsics without a signer, i.e., unsigned
/// extrinsics, are considered as a special group. The items in different groups are cross shuffled,
/// while the order of items inside the same group is still maintained.
fn shuffle_extrinsics<Extrinsic: Debug>(
	extrinsics: Vec<(Option<AccountId>, Extrinsic)>,
	shuffling_seed: Randomness,
) -> Vec<Extrinsic> {
	let mut rng = ChaCha8Rng::from_seed(shuffling_seed);

	let mut positions = extrinsics
		.iter()
		.map(|(maybe_signer, _)| maybe_signer)
		.cloned()
		.collect::<Vec<_>>();

	// Shuffles the positions using Fisher–Yates algorithm.
	positions.shuffle(&mut rng);

	let mut grouped_extrinsics: BTreeMap<Option<AccountId>, VecDeque<_>> =
		extrinsics.into_iter().fold(BTreeMap::new(), |mut groups, (maybe_signer, tx)| {
			groups.entry(maybe_signer).or_insert_with(VecDeque::new).push_back(tx);
			groups
		});

	// The relative ordering for the items in the same group does not change.
	let shuffled_extrinsics = positions
		.into_iter()
		.map(|maybe_signer| {
			grouped_extrinsics
				.get_mut(&maybe_signer)
				.expect("Extrinsics are grouped correctly; qed")
				.pop_front()
				.expect("Extrinsic definitely exists as it's correctly grouped above; qed")
		})
		.collect::<Vec<_>>();

	tracing::trace!(target: LOG_TARGET, ?shuffled_extrinsics, "Shuffled extrinsics");

	shuffled_extrinsics
}

pub(crate) struct BundleProcessor<Block, PBlock, Client, PClient, Backend>
where
	Block: BlockT,
	PBlock: BlockT,
{
	primary_chain_client: Arc<PClient>,
	primary_network: Arc<NetworkService<PBlock, PBlock::Hash>>,
	client: Arc<Client>,
	execution_receipt_sender:
		Arc<TracingUnboundedSender<SignedExecutionReceiptFor<PBlock, Block::Hash>>>,
	backend: Arc<Backend>,
	is_authority: bool,
	keystore: SyncCryptoStorePtr,
	_phantom_data: PhantomData<PBlock>,
}

impl<Block, PBlock, Client, PClient, Backend> Clone
	for BundleProcessor<Block, PBlock, Client, PClient, Backend>
where
	Block: BlockT,
	PBlock: BlockT,
{
	fn clone(&self) -> Self {
		Self {
			primary_chain_client: self.primary_chain_client.clone(),
			primary_network: self.primary_network.clone(),
			client: self.client.clone(),
			execution_receipt_sender: self.execution_receipt_sender.clone(),
			backend: self.backend.clone(),
			is_authority: self.is_authority,
			keystore: self.keystore.clone(),
			_phantom_data: self._phantom_data,
		}
	}
}

impl<Block, PBlock, Client, PClient, Backend>
	BundleProcessor<Block, PBlock, Client, PClient, Backend>
where
	Block: BlockT,
	PBlock: BlockT,
	Client: HeaderBackend<Block> + BlockBackend<Block> + AuxStore + ProvideRuntimeApi<Block>,
	Client::Api: SecondaryApi<Block, AccountId>
		+ sp_block_builder::BlockBuilder<Block>
		+ sp_api::ApiExt<
			Block,
			StateBackend = sc_client_api::backend::StateBackendFor<Backend, Block>,
		>,
	for<'b> &'b Client: BlockImport<
		Block,
		Transaction = TransactionFor<Client, Block>,
		Error = sp_consensus::Error,
	>,
	PClient: HeaderBackend<PBlock> + ProvideRuntimeApi<PBlock>,
	PClient::Api: ExecutorApi<PBlock, Block::Hash>,
	Backend: sc_client_api::Backend<Block>,
{
	pub(crate) fn new(
		primary_chain_client: Arc<PClient>,
		primary_network: Arc<NetworkService<PBlock, PBlock::Hash>>,
		client: Arc<Client>,
		execution_receipt_sender: Arc<
			TracingUnboundedSender<SignedExecutionReceiptFor<PBlock, Block::Hash>>,
		>,
		backend: Arc<Backend>,
		is_authority: bool,
		keystore: SyncCryptoStorePtr,
	) -> Self {
		Self {
			primary_chain_client,
			primary_network,
			client,
			execution_receipt_sender,
			backend,
			is_authority,
			keystore,
			_phantom_data: PhantomData::default(),
		}
	}

	pub(crate) async fn process_bundles(
		self,
		(primary_hash, primary_number): (PBlock::Hash, NumberFor<PBlock>),
		bundles: Vec<OpaqueBundle>,
		shuffling_seed: Randomness,
		maybe_new_runtime: Option<Cow<'static, [u8]>>,
	) -> Result<(), sp_blockchain::Error> {
		let parent_hash = self.client.info().best_hash;
		let parent_number = self.client.info().best_number;

		let mut extrinsics = self.bundles_to_extrinsics(parent_hash, bundles, shuffling_seed)?;

		if let Some(new_runtime) = maybe_new_runtime {
			let encoded_set_code = self
				.client
				.runtime_api()
				.construct_set_code_extrinsic(&BlockId::Hash(parent_hash), new_runtime.to_vec())?;
			let set_code_extrinsic =
				Block::Extrinsic::decode(&mut encoded_set_code.as_slice()).unwrap();
			extrinsics.push(set_code_extrinsic);
		}

		let block_builder = BlockBuilder::new(
			&*self.client,
			parent_hash,
			parent_number,
			RecordProof::No,
			Default::default(),
			&*self.backend,
			extrinsics,
		)?;

		let BuiltBlock { block, storage_changes, proof: _ } = block_builder.build()?;

		let (header, body) = block.deconstruct();
		let state_root = *header.state_root();
		let header_hash = header.hash();
		let header_number = *header.number();

		let block_import_params = {
			let mut import_block = BlockImportParams::new(BlockOrigin::Own, header);
			import_block.body = Some(body);
			import_block.state_action =
				StateAction::ApplyChanges(StorageChanges::Changes(storage_changes));
			// TODO: double check the fork choice is correct, see also ParachainBlockImport.
			import_block.fork_choice = Some(ForkChoiceStrategy::LongestChain);
			import_block
		};

		let import_result =
			(&*self.client).import_block(block_import_params, Default::default()).await?;

		// TODO: handle the import result properly.
		match import_result {
			ImportResult::Imported(..) => {},
			ImportResult::AlreadyInChain => {},
			ImportResult::KnownBad => {
				panic!("Bad block {}: {:?}", header_number, header_hash);
			},
			ImportResult::UnknownParent => {
				panic!(
					"Block with unknown parent {}: {:?}, parent: {:?}",
					header_number, header_hash, parent_hash
				);
			},
			ImportResult::MissingState => {
				panic!(
					"Parent state is missing for {}: {:?}, parent: {:?}",
					header_number, header_hash, parent_hash
				);
			},
		}

		let mut roots =
			self.client.runtime_api().intermediate_roots(&BlockId::Hash(header_hash))?;

		let state_root = state_root
			.encode()
			.try_into()
			.expect("State root uses the same Block hash type which must fit into [u8; 32]; qed");

		roots.push(state_root);

		let trace_root = crate::merkle_tree::construct_trace_merkle_tree(roots.clone())?.root();
		let trace = roots
			.into_iter()
			.map(|r| {
				Block::Hash::decode(&mut r.as_slice())
					.expect("Storage root uses the same Block hash type; qed")
			})
			.collect();

		tracing::debug!(
			target: LOG_TARGET,
			?trace,
			?trace_root,
			"Trace root calculated for #{}",
			header_hash
		);

		let execution_receipt = ExecutionReceipt {
			primary_number,
			primary_hash,
			secondary_hash: header_hash,
			trace,
			trace_root,
		};

		let best_execution_chain_number = self
			.primary_chain_client
			.runtime_api()
			.best_execution_chain_number(&BlockId::Hash(primary_hash))?;

		let best_execution_chain_number =
			<NumberFor<Block>>::decode(&mut best_execution_chain_number.encode().as_slice())
				.expect("Primary number and secondary number must use the same type; qed");

		assert!(
			header_number > best_execution_chain_number,
			"Consensus chain number must larger than execution chain number by at least 1"
		);

		crate::aux_schema::write_execution_receipt::<_, Block, PBlock>(
			&*self.client,
			(header_hash, header_number),
			best_execution_chain_number,
			&execution_receipt,
		)?;

		// TODO: The applied txs can be fully removed from the transaction pool

		if self.primary_network.is_major_syncing() {
			tracing::debug!(
				target: LOG_TARGET,
				"Skip generating signed execution receipt as the primary node is still major syncing..."
			);
			return Ok(())
		}

		// Ideally, the receipt of current block will be included in the next block, i.e., no
		// missing receipts.
		if header_number == best_execution_chain_number + One::one() {
			self.try_sign_and_send_receipt(primary_hash, execution_receipt)
		} else {
			// Receipts for some previous blocks are missing.
			let max_drift = self
				.primary_chain_client
				.runtime_api()
				.maximum_receipt_drift(&BlockId::Hash(primary_hash))?;

			let max_drift = <NumberFor<Block>>::decode(&mut max_drift.encode().as_slice())
				.expect("Primary number and secondary number must use the same type; qed");

			let max_allowed = (best_execution_chain_number + max_drift).min(header_number);

			// TODO: parallelize and avoid spamming the missing receipts?
			let mut to_send = best_execution_chain_number + One::one();
			while to_send <= max_allowed {
				let block_hash = self.client.hash(to_send)?.ok_or_else(|| {
					sp_blockchain::Error::Backend(format!("Hash for Block {:?} not found", to_send))
				})?;

				// TODO: will be removed once the TODO below is resolved.
				#[allow(clippy::single_match)]
				match crate::aux_schema::load_execution_receipt(&*self.client, block_hash)? {
					Some(receipt) => {
						self.try_sign_and_send_receipt(primary_hash, receipt)?;
					},
					None => {
						// TODO: In order to solve the problem that the receipt can be pruned by
						// executor, we need to check if every ER in the primary chain is valid beforehand:
						// - If ER is invalid, cache it and expect FraudProof within next X blocks.
						//    - If FraudProof found, remove it from the cache.
						//    - If FraudProof not found and major sync is done:
						//        - Generate a FraudProof for the first incorrect ER.
						//            - FraudProof might need to access the block body, hence all
						//            the blocks have to be kept in the database. TODO: double check.
						//        - Start publishing the correct ERs after the above corrected one.
						// println!("TODO: Cache the invalid receipts without fraud proof");
					},
				}

				to_send += One::one();
			}

			Ok(())
		}
	}

	fn bundles_to_extrinsics(
		&self,
		parent_hash: Block::Hash,
		bundles: Vec<OpaqueBundle>,
		shuffling_seed: Randomness,
	) -> Result<Vec<Block::Extrinsic>, sp_blockchain::Error> {
		let mut extrinsics = bundles
			.into_iter()
			.flat_map(|bundle| {
				bundle.opaque_extrinsics.into_iter().filter_map(|opaque_extrinsic| {
					match <<Block as BlockT>::Extrinsic>::decode(
						&mut opaque_extrinsic.encode().as_slice(),
					) {
						Ok(uxt) => Some(uxt),
						Err(e) => {
							tracing::error!(
								target: LOG_TARGET,
								error = ?e,
								"Failed to decode the opaque extrisic in bundle, this should not happen"
							);
							None
						},
					}
				})
			})
			.collect::<Vec<_>>();

		// TODO: or just Vec::new()?
		// Ideally there should be only a few duplicated transactions.
		let mut seen = Vec::with_capacity(extrinsics.len());
		extrinsics.retain(|uxt| match seen.contains(uxt) {
			true => {
				tracing::trace!(target: LOG_TARGET, extrinsic = ?uxt, "Duplicated extrinsic");
				false
			},
			false => {
				seen.push(uxt.clone());
				true
			},
		});
		drop(seen);

		tracing::trace!(target: LOG_TARGET, ?extrinsics, "Origin deduplicated extrinsics");

		let extrinsics: Vec<_> = match self
			.client
			.runtime_api()
			.extract_signer(&BlockId::Hash(parent_hash), extrinsics)
		{
			Ok(res) => res,
			Err(e) => {
				tracing::error!(
					target: LOG_TARGET,
					error = ?e,
					"Error at calling runtime api: extract_signer"
				);
				return Err(e.into())
			},
		};

		let extrinsics =
			shuffle_extrinsics::<<Block as BlockT>::Extrinsic>(extrinsics, shuffling_seed);

		Ok(extrinsics)
	}

	fn try_sign_and_send_receipt(
		&self,
		primary_hash: PBlock::Hash,
		execution_receipt: ExecutionReceiptFor<PBlock, Block::Hash>,
	) -> Result<(), sp_blockchain::Error> {
		let executor_id = self
			.primary_chain_client
			.runtime_api()
			.executor_id(&BlockId::Hash(primary_hash))?;

		if self.is_authority &&
			SyncCryptoStore::has_keys(
				&*self.keystore,
				&[(ByteArray::to_raw_vec(&executor_id), ExecutorId::ID)],
			) {
			let to_sign = execution_receipt.hash();
			match SyncCryptoStore::sign_with(
				&*self.keystore,
				ExecutorId::ID,
				&executor_id.clone().into(),
				to_sign.as_ref(),
			) {
				Ok(Some(signature)) => {
					let signed_execution_receipt = SignedExecutionReceipt {
						execution_receipt,
						signature: ExecutorSignature::decode(&mut signature.as_slice()).map_err(
							|err| {
								sp_blockchain::Error::Application(Box::from(format!(
									"Failed to decode the signature of execution receipt: {err}"
								)))
							},
						)?,
						signer: executor_id,
					};

					if let Err(e) = self
						.execution_receipt_sender
						.unbounded_send(signed_execution_receipt.clone())
					{
						tracing::error!(target: LOG_TARGET, error = ?e, "Failed to send signed execution receipt");
					}

					let best_hash = self.primary_chain_client.info().best_hash;

					// Broadcast ER to all farmers via unsigned extrinsic.
					self.primary_chain_client.runtime_api().submit_execution_receipt_unsigned(
						&BlockId::Hash(best_hash),
						signed_execution_receipt,
					)?;

					Ok(())
				},
				Ok(None) => Err(sp_blockchain::Error::Application(Box::from(
					"This should not happen as the existence of key was just checked",
				))),
				Err(error) => Err(sp_blockchain::Error::Application(Box::from(format!(
					"Error occurred when signing the execution receipt: {error}"
				)))),
			}
		} else {
			Ok(())
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use sp_keyring::sr25519::Keyring;
	use sp_runtime::traits::{BlakeTwo256, Hash as HashT};

	#[test]
	fn shuffle_extrinsics_should_work() {
		let alice = Keyring::Alice.to_account_id();
		let bob = Keyring::Bob.to_account_id();
		let charlie = Keyring::Charlie.to_account_id();

		let extrinsics = vec![
			(Some(alice.clone()), 10),
			(None, 100),
			(Some(bob.clone()), 1),
			(Some(bob), 2),
			(Some(charlie.clone()), 30),
			(Some(alice.clone()), 11),
			(Some(charlie), 31),
			(None, 101),
			(None, 102),
			(Some(alice), 12),
		];

		let dummy_seed = BlakeTwo256::hash_of(&[1u8; 64]).into();
		let shuffled_extrinsics = shuffle_extrinsics(extrinsics, dummy_seed);

		assert_eq!(shuffled_extrinsics, vec![100, 30, 10, 1, 11, 101, 31, 12, 102, 2]);
	}

	#[test]
	fn construct_trace_merkle_tree_should_work() {
		let root1 = [1u8; 32];
		let root2 = [2u8; 32];
		let root3 = [3u8; 32];

		let roots = vec![root1, root2];
		crate::merkle_tree::construct_trace_merkle_tree(roots).unwrap();

		let roots = vec![root1, root2, root3];
		crate::merkle_tree::construct_trace_merkle_tree(roots).unwrap();
	}
}
