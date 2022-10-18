use crate::single_disk_plot::{FarmingError, SectorMetadata};
use bitvec::prelude::*;
use parity_scale_codec::{Decode, IoReader};
use schnorrkel::Keypair;
use std::io;
use subspace_core_primitives::crypto::blake2b_256_254_hash;
use subspace_core_primitives::crypto::kzg::Witness;
use subspace_core_primitives::{
    Blake2b256Hash, Chunk, Piece, PublicKey, SectorId, Solution, SolutionRange, PIECE_SIZE,
};
use subspace_rpc_primitives::FarmerProtocolInfo;
use subspace_solving::{create_chunk_signature, derive_chunk_otp};
use subspace_verification::is_within_solution_range;
use tracing::error;

/// Sector that can be used to create a solution that is within desired solution range
#[derive(Debug, Clone)]
pub struct EligibleSector {
    /// Sector ID
    pub sector_id: SectorId,
    /// Sector index
    pub sector_index: u64,
    /// Derived local challenge
    pub local_challenge: SolutionRange,
    /// Audit index corresponding to the challenge used
    pub audit_index: u64,
    /// Chunk at audit index
    pub chunk: Chunk,
    /// Expanded version of the above chunk
    pub expanded_chunk: SolutionRange,
    /// Encoded piece where chunk is located
    pub encoded_piece: Piece,
    /// Offset of the piece in sector
    pub audit_piece_offset: u64,
}

/// Audit a single sector
pub fn audit_sector<S>(
    public_key: &PublicKey,
    sector_index: u64,
    farmer_protocol_info: &FarmerProtocolInfo,
    global_challenge: &Blake2b256Hash,
    solution_range: SolutionRange,
    mut sector: S,
) -> Result<Option<EligibleSector>, FarmingError>
where
    S: io::Read,
{
    let sector_id = SectorId::new(public_key, sector_index);
    let chunks_in_sector = u64::from(farmer_protocol_info.record_size.get()) * u64::from(u8::BITS)
        / u64::from(farmer_protocol_info.space_l.get());

    let local_challenge = sector_id.derive_local_challenge(global_challenge);
    let audit_index: u64 = local_challenge % chunks_in_sector;
    let audit_piece_offset = (audit_index / u64::from(u8::BITS)) / PIECE_SIZE as u64;
    // Offset of the piece in sector (in bytes)
    let audit_piece_bytes_offset = audit_piece_offset * PIECE_SIZE as u64;
    // Audit index (chunk) within corresponding piece
    let audit_index_within_piece = audit_index - audit_piece_bytes_offset * u64::from(u8::BITS);
    let mut piece = Piece::default();
    sector.read_exact(&mut piece)?;

    // TODO: We are skipping witness part of the piece or else it is not
    //  decodable
    let maybe_chunk = piece[..farmer_protocol_info.record_size.get() as usize]
        .view_bits()
        .chunks_exact(farmer_protocol_info.space_l.get() as usize)
        .nth(audit_index_within_piece as usize);

    let chunk = match maybe_chunk {
        Some(chunk) => Chunk::from(chunk),
        None => {
            // TODO: Record size is not multiple of `space_l`, last bits
            //  were not encoded and should not be used for solving
            return Ok(None);
        }
    };

    // TODO: This just have 20 bits of entropy as input, should we add
    //  something else?
    let expanded_chunk = chunk.expand(local_challenge);

    Ok(
        is_within_solution_range(local_challenge, expanded_chunk, solution_range).then_some(
            EligibleSector {
                sector_id,
                sector_index,
                local_challenge,
                audit_index,
                chunk,
                expanded_chunk,
                encoded_piece: piece,
                audit_piece_offset,
            },
        ),
    )
}

/// Create solution for eligible sector
pub fn create_solution<SM>(
    keypair: &Keypair,
    mut eligible_sector: EligibleSector,
    reward_address: PublicKey,
    farmer_protocol_info: &FarmerProtocolInfo,
    sector_metadata: SM,
) -> Result<Option<Solution<PublicKey, PublicKey>>, FarmingError>
where
    SM: io::Read,
{
    let sector_metadata = SectorMetadata::decode(&mut IoReader(sector_metadata))
        .map_err(|error| FarmingError::FailedToDecodeMetadata { error })?;

    // Decode piece
    let (record, witness_bytes) = eligible_sector
        .encoded_piece
        .split_at_mut(farmer_protocol_info.record_size.get() as usize);
    let piece_witness = match Witness::try_from_bytes(
        (&*witness_bytes).try_into().expect(
            "Witness must have correct size unless implementation is broken in a big way; qed",
        ),
    ) {
        Ok(piece_witness) => piece_witness,
        Err(error) => {
            let piece_index = eligible_sector.sector_id.derive_piece_index(
                eligible_sector.audit_piece_offset,
                sector_metadata.total_pieces,
            );
            let audit_piece_bytes_offset = eligible_sector.audit_piece_offset * PIECE_SIZE as u64;
            error!(
                ?error,
                sector_id = ?eligible_sector.sector_id,
                %audit_piece_bytes_offset,
                %piece_index,
                "Failed to decode witness for piece, likely caused by on-disk data corruption"
            );
            return Ok(None);
        }
    };
    // TODO: Extract encoding into separate function reusable in
    //  farmer and otherwise
    record
        .view_bits_mut::<Lsb0>()
        .chunks_mut(farmer_protocol_info.space_l.get() as usize)
        .enumerate()
        .for_each(|(chunk_index, bits)| {
            // Derive one-time pad
            let mut otp = derive_chunk_otp(
                &eligible_sector.sector_id,
                witness_bytes,
                chunk_index as u32,
            );
            // XOR chunk bit by bit with one-time pad
            bits.iter_mut()
                .zip(otp.view_bits_mut::<Lsb0>().iter())
                .for_each(|(mut a, b)| {
                    *a ^= *b;
                });
        });

    Ok(Some(Solution {
        public_key: PublicKey::from(keypair.public.to_bytes()),
        reward_address,
        sector_index: eligible_sector.sector_index,
        total_pieces: sector_metadata.total_pieces,
        piece_offset: eligible_sector.audit_piece_offset,
        piece_record_hash: blake2b_256_254_hash(record),
        piece_witness,
        chunk: eligible_sector.chunk,
        chunk_signature: create_chunk_signature(keypair, &eligible_sector.chunk),
    }))
}
