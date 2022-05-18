use sp_executor::ExecutionReceipt;

/// Compare if two execution receipts are the same, return a tuple of the first mismatched
/// trace index and trace root if any.
pub fn compare_receipt<Number, Hash, SecondaryHash: Copy + Eq>(
	local: &ExecutionReceipt<Number, Hash, SecondaryHash>,
	other: &ExecutionReceipt<Number, Hash, SecondaryHash>,
) -> Option<(usize, SecondaryHash)> {
	local.trace.iter().enumerate().zip(other.trace.iter().enumerate()).find_map(
		|((local_idx, local_root), (_, external_root))| {
			if local_root != external_root {
				Some((local_idx, *local_root))
			} else {
				None
			}
		},
	)
}
