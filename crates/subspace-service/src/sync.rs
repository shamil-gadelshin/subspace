pub mod dsn_sync;
pub mod fast_sync;
pub mod segment_header_downloader;

use async_trait::async_trait;
use std::error::Error;
use std::fmt;
use std::num::NonZeroU16;
use std::sync::Arc;
use subspace_core_primitives::{Piece, PieceIndex};
use subspace_networking::utils::piece_provider::{PieceProvider, PieceValidator};

/// Trait representing a way to get pieces for DSN sync purposes
#[async_trait]
pub trait DsnSyncPieceGetter: fmt::Debug {
    async fn get_piece(
        &self,
        piece_index: PieceIndex,
    ) -> Result<Option<Piece>, Box<dyn Error + Send + Sync + 'static>>;
}

#[async_trait]
impl<T> DsnSyncPieceGetter for Arc<T>
where
    T: DsnSyncPieceGetter + Send + Sync + ?Sized,
{
    async fn get_piece(
        &self,
        piece_index: PieceIndex,
    ) -> Result<Option<Piece>, Box<dyn Error + Send + Sync + 'static>> {
        self.as_ref().get_piece(piece_index).await
    }
}

#[async_trait]
impl<PV> DsnSyncPieceGetter for PieceProvider<PV>
where
    PV: PieceValidator,
{
    async fn get_piece(
        &self,
        piece_index: PieceIndex,
    ) -> Result<Option<Piece>, Box<dyn Error + Send + Sync + 'static>> {
        self.get_piece(
            piece_index,
        )
        .await
    }
}

/// Get piece retry attempts number.
const PIECE_GETTER_RETRY_NUMBER: NonZeroU16 = NonZeroU16::new(7).expect("Not zero; qed");
