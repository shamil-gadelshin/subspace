pub mod farming;
pub mod plotting;

use crate::file_ext::FileExt;
use crate::identity::Identity;
use crate::reward_signing::reward_signing;
use crate::rpc_client;
use crate::rpc_client::RpcClient;
use crate::single_disk_plot::farming::{audit_sector, create_solution};
use crate::single_disk_plot::plotting::{plot_sector, PlottingStatus};
use crate::utils::JoinOnDrop;
use bytesize::ByteSize;
use derive_more::{Display, From};
use futures::channel::oneshot;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use memmap2::{MmapMut, MmapOptions};
use parity_db::const_assert;
use parity_scale_codec::{Decode, Encode};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::future::Future;
use std::io::{Seek, SeekFrom};
use std::num::{NonZeroU16, NonZeroU64};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use std::{fmt, fs, io, thread};
use std_semaphore::{Semaphore, SemaphoreGuard};
use subspace_core_primitives::{
    plot_sector_size, PieceIndex, PublicKey, SectorIndex, SegmentIndex, Solution, PIECE_SIZE,
};
use subspace_rpc_primitives::SolutionResponse;
use thiserror::Error;
use tokio::runtime::Handle;
use tracing::{debug, error, info, info_span, trace, Instrument, Span};
use ulid::Ulid;

// Refuse to compile on non-64-bit platforms, offsets may fail on those when converting from u64 to
// usize depending on chain parameters
const_assert!(std::mem::size_of::<usize>() >= std::mem::size_of::<u64>());

/// Reserve 1M of space for plot metadata (for potential future expansion)
const RESERVED_PLOT_METADATA: u64 = 1024 * 1024;

/// Semaphore that limits disk access concurrency in strategic places to the number specified during
/// initialization
#[derive(Clone)]
pub struct SingleDiskSemaphore {
    inner: Arc<Semaphore>,
}

impl fmt::Debug for SingleDiskSemaphore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SingleDiskSemaphore").finish()
    }
}

impl SingleDiskSemaphore {
    /// Create new semaphore for limiting concurrency of the major processes working with the same
    /// disk
    pub fn new(concurrency: NonZeroU16) -> Self {
        Self {
            inner: Arc::new(Semaphore::new(concurrency.get() as isize)),
        }
    }

    /// Acquire access, will block current thread until previously acquired guards are dropped and
    /// access is released
    pub fn acquire(&self) -> SemaphoreGuard<'_> {
        self.inner.access()
    }
}

/// An identifier for single disk plot, can be used for in logs, thread names, etc.
#[derive(
    Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Serialize, Deserialize, Display, From,
)]
#[serde(untagged)]
pub enum SingleDiskPlotId {
    /// Plot ID
    Ulid(Ulid),
}

#[allow(clippy::new_without_default)]
impl SingleDiskPlotId {
    /// Creates new ID
    pub fn new() -> Self {
        Self::Ulid(Ulid::new())
    }
}

/// Important information about the contents of the `SingleDiskPlot`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SingleDiskPlotInfo {
    /// V0 of the info
    #[serde(rename_all = "camelCase")]
    V0 {
        /// ID of the plot
        id: SingleDiskPlotId,
        /// Genesis hash of the chain used for plot creation
        #[serde(with = "hex::serde")]
        genesis_hash: [u8; 32],
        /// Public key of identity used for plot creation
        public_key: PublicKey,
        /// First sector index in this plot
        ///
        /// Multiple plots can reuse the same identity, but they have to use different ranges for
        /// sector indexes or else they'll essentially plot the same data and will not result in
        /// increased probability of winning the reward.
        first_sector_index: SectorIndex,
        /// How much space in bytes is allocated for this plot
        allocated_space: u64,
    },
}

impl SingleDiskPlotInfo {
    const FILE_NAME: &'static str = "single_disk_plot.json";

    pub fn new(
        id: SingleDiskPlotId,
        genesis_hash: [u8; 32],
        public_key: PublicKey,
        first_sector_index: SectorIndex,
        allocated_space: u64,
    ) -> Self {
        Self::V0 {
            id,
            genesis_hash,
            public_key,
            first_sector_index,
            allocated_space,
        }
    }

    /// Load `SingleDiskPlot` from path is supposed to be stored, `None` means no info file was
    /// found, happens during first start.
    pub fn load_from(path: &Path) -> io::Result<Option<Self>> {
        let bytes = match fs::read(path.join(Self::FILE_NAME)) {
            Ok(bytes) => bytes,
            Err(error) => {
                return if error.kind() == io::ErrorKind::NotFound {
                    Ok(None)
                } else {
                    Err(error)
                };
            }
        };

        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    /// Store `SingleDiskPlot` info to path so it can be loaded again upon restart.
    pub fn store_to(&self, directory: &Path) -> io::Result<()> {
        fs::write(
            directory.join(Self::FILE_NAME),
            serde_json::to_vec(self).expect("Info serialization never fails; qed"),
        )
    }

    // ID of the plot
    pub fn id(&self) -> &SingleDiskPlotId {
        let Self::V0 { id, .. } = self;
        id
    }

    // Genesis hash of the chain used for plot creation
    pub fn genesis_hash(&self) -> &[u8; 32] {
        let Self::V0 { genesis_hash, .. } = self;
        genesis_hash
    }

    // Public key of identity used for plot creation
    pub fn public_key(&self) -> &PublicKey {
        let Self::V0 { public_key, .. } = self;
        public_key
    }

    /// First sector index in this plot
    ///
    /// Multiple plots can reuse the same identity, but they have to use different ranges for
    /// sector indexes or else they'll essentially plot the same data and will not result in
    /// increased probability of winning the reward.
    pub fn first_sector_index(&self) -> SectorIndex {
        let Self::V0 {
            first_sector_index, ..
        } = self;
        *first_sector_index
    }

    /// How much space in bytes is allocated for this plot
    pub fn allocated_space(&self) -> u64 {
        let Self::V0 {
            allocated_space, ..
        } = self;
        *allocated_space
    }
}

/// Summary of single disk plot for presentational purposes
pub enum SingleDiskPlotSummary {
    /// Plot was found and read successfully
    Found {
        /// Plot info
        info: SingleDiskPlotInfo,
        /// Path to directory where plot is stored.
        directory: PathBuf,
    },
    /// Plot was not found
    NotFound {
        /// Path to directory where plot is stored.
        directory: PathBuf,
    },
    /// Failed to open plot
    Error {
        /// Path to directory where plot is stored.
        directory: PathBuf,
        /// Error itself
        error: io::Error,
    },
}

#[derive(Debug, Encode, Decode)]
struct PlotMetadataHeader {
    version: u8,
    sector_count: u64,
}

impl PlotMetadataHeader {
    fn encoded_size() -> usize {
        let default = PlotMetadataHeader {
            version: 0,
            sector_count: 0,
        };

        default.encoded_size()
    }
}

/// Metadata of the plotted sector
#[doc(hidden)]
#[derive(Debug, Encode, Decode)]
pub struct SectorMetadata {
    /// Total number of pieces in archived history of the blockchain as of sector creation
    pub total_pieces: NonZeroU64,
    /// Sector expiration, defined as sector of the archived history of the blockchain
    pub expires_at: SegmentIndex,
}

impl SectorMetadata {
    /// Size of encoded sector metadata
    pub fn encoded_size() -> usize {
        let default = SectorMetadata {
            total_pieces: NonZeroU64::new(1).expect("1 is not 0; qed"),
            expires_at: 0,
        };

        default.encoded_size()
    }
}

/// Options used to open single dis plot
pub struct SingleDiskPlotOptions<RC> {
    /// Path to directory where plot are stored.
    pub directory: PathBuf,
    /// How much space in bytes can plot use for plot
    pub allocated_space: u64,
    /// RPC client connected to Subspace node
    pub rpc_client: RC,
    /// Address where farming rewards should go
    pub reward_address: PublicKey,
}

/// Errors happening when trying to create/open single disk plot
#[derive(Debug, Error)]
pub enum SingleDiskPlotError {
    // TODO: Make more variants out of this generic one
    /// I/O error occurred
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// Can't resize plot after creation
    #[error(
        "Usable plotting space of plot {id} {new_space} is different from {old_space} when plot was \
        created, resizing isn't supported yet"
    )]
    CantResize {
        /// Plot ID
        id: SingleDiskPlotId,
        /// Space allocated during plot creation
        old_space: ByteSize,
        /// New desired plot size
        new_space: ByteSize,
    },
    /// Wrong chain (genesis hash)
    #[error(
        "Genesis hash of plot {id} {wrong_chain} is different from {correct_chain} when plot was \
        created, it is not possible to use plot on a different chain"
    )]
    WrongChain {
        /// Plot ID
        id: SingleDiskPlotId,
        /// Hex-encoded genesis hash during plot creation
        // TODO: Wrapper type with `Display` impl for genesis hash
        correct_chain: String,
        /// Hex-encoded current genesis hash
        wrong_chain: String,
    },
    /// Public key in identity doesn't match metadata
    #[error(
        "Public key of plot {id} {wrong_public_key} is different from {correct_public_key} when \
        plot was created, something went wrong, likely due to manual edits"
    )]
    IdentityMismatch {
        /// Plot ID
        id: SingleDiskPlotId,
        /// Public key used during plot creation
        correct_public_key: PublicKey,
        /// Current public key
        wrong_public_key: PublicKey,
    },
    /// Failed to decode metadata header
    #[error("Failed to decode metadata header: {0}")]
    FailedToDecodeMetadataHeader(parity_scale_codec::Error),
    /// Unexpected metadata version
    #[error("Unexpected metadata version {0}")]
    UnexpectedMetadataVersion(u8),
    /// Node RPC error
    #[error("Node RPC error: {0}")]
    NodeRpcError(Box<dyn std::error::Error + Send + Sync + 'static>),
}

/// Errors that happen during plotting
#[derive(Debug, Error)]
pub enum PlottingError {
    /// Failed to retriever farmer protocol info
    #[error("Failed to retriever farmer protocol info: {error}")]
    FailedToGetFarmerProtocolInfo {
        /// Lower-level error
        error: rpc_client::Error,
    },
    /// Piece not found, can't create sector, this should never happen
    #[error("Piece {piece_index} not found, can't create sector, this should never happen")]
    PieceNotFound {
        /// Piece index
        piece_index: PieceIndex,
    },
    /// Failed to retrieve piece
    #[error("Failed to retrieve piece {piece_index}: {error}")]
    FailedToRetrievePiece {
        /// Piece index
        piece_index: PieceIndex,
        /// Lower-level error
        error: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    /// I/O error occurred
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Errors that happen during farming
#[derive(Debug, Error)]
pub enum FarmingError {
    /// Failed to retriever farmer protocol info
    #[error("Failed to retriever farmer protocol info: {error}")]
    FailedToGetFarmerProtocolInfo {
        /// Lower-level error
        error: rpc_client::Error,
    },
    /// Failed to create memory mapping for plot
    #[error("Failed to create memory mapping for plot: {error}")]
    FailedToMapPlot {
        /// Lower-level error
        error: io::Error,
    },
    /// Failed to create memory mapping for metadata
    #[error("Failed to create memory mapping for metadata: {error}")]
    FailedToMapMetadata {
        /// Lower-level error
        error: io::Error,
    },
    /// Failed to decode sector metadata
    #[error("Failed to decode sector metadata: {error}")]
    FailedToDecodeMetadata {
        /// Lower-level error
        error: parity_scale_codec::Error,
    },
    /// Failed to submit solutions response
    #[error("Failed to submit solutions response: {error}")]
    FailedToSubmitSolutionsResponse {
        /// Lower-level error
        error: rpc_client::Error,
    },
    /// I/O error occurred
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Errors that happen in background tasks
#[derive(Debug, Error)]
pub enum BackgroundTaskError {
    /// Plotting error
    #[error(transparent)]
    Plotting(#[from] PlottingError),
    /// Farming error
    #[error(transparent)]
    Farming(#[from] FarmingError),
}

type BackgroundTask = Pin<Box<dyn Future<Output = Result<(), BackgroundTaskError>> + Send>>;

/// Single disk plot abstraction is a container for everything necessary to plot/farm with a single
/// disk plot.
///
/// Plot starts operating during creation and doesn't stop until dropped (or error happens).
#[must_use = "Plot does not function properly unless run() method is called"]
pub struct SingleDiskPlot {
    id: SingleDiskPlotId,
    span: Span,
    tasks: FuturesUnordered<BackgroundTask>,
    _plotting_join_handle: JoinOnDrop,
    _farming_join_handle: JoinOnDrop,
    shutting_down: Arc<AtomicBool>,
}

impl Drop for SingleDiskPlot {
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::SeqCst);
    }
}

impl SingleDiskPlot {
    const PLOT_FILE: &'static str = "plot.bin";
    const METADATA_FILE: &'static str = "metadata.bin";

    /// Create new single disk plot instance
    pub fn new<RC>(options: SingleDiskPlotOptions<RC>) -> Result<Self, SingleDiskPlotError>
    where
        RC: RpcClient,
    {
        let handle = Handle::current();

        let SingleDiskPlotOptions {
            directory,
            allocated_space,
            rpc_client,
            reward_address,
        } = options;

        fs::create_dir_all(&directory)?;

        // TODO: Parametrize concurrency, much higher default due to SSD focus
        // TODO: Use this or remove
        let _single_disk_semaphore =
            SingleDiskSemaphore::new(NonZeroU16::new(10).expect("Not a zero; qed"));

        // TODO: Update `Identity` to use more specific error type and remove this `.unwrap()`
        let identity = Identity::open_or_create(&directory).unwrap();
        let public_key = identity.public_key().to_bytes().into();

        let farmer_protocol_info = tokio::task::block_in_place(|| {
            Handle::current()
                .block_on(rpc_client.farmer_protocol_info())
                .map_err(SingleDiskPlotError::NodeRpcError)
        })?;
        // TODO: In case `space_l` changes on the fly, code below will break horribly
        let space_l = farmer_protocol_info.space_l;
        let plot_sector_size = plot_sector_size(space_l);

        assert_eq!(
            plot_sector_size % PIECE_SIZE as u64,
            0,
            "Sector size must be multiple of piece size"
        );

        let single_disk_plot_info = match SingleDiskPlotInfo::load_from(&directory)? {
            Some(single_disk_plot_info) => {
                if allocated_space != single_disk_plot_info.allocated_space() {
                    return Err(SingleDiskPlotError::CantResize {
                        id: *single_disk_plot_info.id(),
                        old_space: ByteSize::b(single_disk_plot_info.allocated_space()),
                        new_space: ByteSize::b(allocated_space),
                    });
                }

                if &farmer_protocol_info.genesis_hash != single_disk_plot_info.genesis_hash() {
                    return Err(SingleDiskPlotError::WrongChain {
                        id: *single_disk_plot_info.id(),
                        correct_chain: hex::encode(single_disk_plot_info.genesis_hash()),
                        wrong_chain: hex::encode(farmer_protocol_info.genesis_hash),
                    });
                }

                if &public_key != single_disk_plot_info.public_key() {
                    return Err(SingleDiskPlotError::IdentityMismatch {
                        id: *single_disk_plot_info.id(),
                        correct_public_key: *single_disk_plot_info.public_key(),
                        wrong_public_key: public_key,
                    });
                }

                single_disk_plot_info
            }
            None => {
                // TODO: Global generator that makes sure to avoid returning the same sector index
                //  for multiple disks
                let first_sector_index = SystemTime::UNIX_EPOCH
                    .elapsed()
                    .expect("Unix epoch is always in the past; qed")
                    .as_secs()
                    .wrapping_mul(u64::from(u32::MAX));

                let single_disk_plot_info = SingleDiskPlotInfo::new(
                    SingleDiskPlotId::new(),
                    farmer_protocol_info.genesis_hash,
                    public_key,
                    first_sector_index,
                    allocated_space,
                );

                single_disk_plot_info.store_to(&directory)?;

                single_disk_plot_info
            }
        };

        let single_disk_plot_id = *single_disk_plot_info.id();
        let first_sector_index = single_disk_plot_info.first_sector_index();

        // TODO: Account for plot overhead
        let target_sector_count = allocated_space / plot_sector_size;

        // TODO: Consider file locking to prevent other apps from modifying it
        let mut metadata_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(directory.join(Self::METADATA_FILE))?;

        let (metadata_header, mut metadata_header_mmap) = if metadata_file.seek(SeekFrom::End(0))?
            == 0
        {
            let metadata_header = PlotMetadataHeader {
                version: 0,
                sector_count: 0,
            };

            metadata_file.preallocate(
                RESERVED_PLOT_METADATA
                    + SectorMetadata::encoded_size() as u64 * target_sector_count,
            )?;
            metadata_file.write_all_at(metadata_header.encode().as_slice(), 0)?;

            let metadata_header_mmap = unsafe {
                MmapOptions::new()
                    .len(PlotMetadataHeader::encoded_size())
                    .map_mut(&metadata_file)?
            };

            (metadata_header, metadata_header_mmap)
        } else {
            let metadata_header_mmap = unsafe {
                MmapOptions::new()
                    .len(PlotMetadataHeader::encoded_size())
                    .map_mut(&metadata_file)?
            };

            let metadata_header = PlotMetadataHeader::decode(&mut metadata_header_mmap.as_ref())
                .map_err(SingleDiskPlotError::FailedToDecodeMetadataHeader)?;

            if metadata_header.version != 0 {
                return Err(SingleDiskPlotError::UnexpectedMetadataVersion(
                    metadata_header.version,
                ));
            }

            (metadata_header, metadata_header_mmap)
        };

        let metadata_header = Arc::new(Mutex::new(metadata_header));

        let mut metadata_mmap_mut = unsafe {
            MmapOptions::new()
                .offset(RESERVED_PLOT_METADATA)
                .len(SectorMetadata::encoded_size() * target_sector_count as usize)
                .map_mut(&metadata_file)?
        };

        let plot_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(directory.join(Self::PLOT_FILE))?;

        plot_file.preallocate(plot_sector_size * target_sector_count)?;

        let mut plot_mmap_mut = unsafe { MmapMut::map_mut(&plot_file)? };

        let (error_sender, error_receiver) = oneshot::channel();
        let error_sender = Arc::new(Mutex::new(Some(error_sender)));

        let tasks = FuturesUnordered::<BackgroundTask>::new();

        tasks.push(Box::pin(async move {
            if let Ok(error) = error_receiver.await {
                return Err(error);
            }

            Ok(())
        }));

        let shutting_down = Arc::new(AtomicBool::new(false));

        let plotting_join_handle = thread::Builder::new()
            .name(format!("p-{single_disk_plot_id}"))
            .spawn({
                let handle = handle.clone();
                let metadata_header = Arc::clone(&metadata_header);
                let shutting_down = Arc::clone(&shutting_down);
                let rpc_client = rpc_client.clone();
                let error_sender = Arc::clone(&error_sender);

                move || {
                    let _tokio_handle_guard = handle.enter();
                    let span = info_span!("single_disk_plot", %single_disk_plot_id);
                    let _span_guard = span.enter();

                    // Initial plotting
                    let initial_plotting_result = try {
                        let chunked_sectors = plot_mmap_mut
                            .as_mut()
                            .chunks_exact_mut(plot_sector_size as usize);
                        let chunked_metadata = metadata_mmap_mut
                            .as_mut()
                            .chunks_exact_mut(SectorMetadata::encoded_size());
                        let plot_initial_sector = chunked_sectors
                            .zip(chunked_metadata)
                            .enumerate()
                            .skip(
                                // Some sectors may already be plotted, skip them
                                metadata_header.lock().sector_count as usize,
                            )
                            .map(|(sector_index, (sector, metadata))| {
                                (sector_index as u64 + first_sector_index, sector, metadata)
                            });

                        // TODO: Concurrency
                        for (sector_index, sector, sector_metadata) in plot_initial_sector {
                            if shutting_down.load(Ordering::Acquire) {
                                debug!(
                                    %sector_index,
                                    "Instance is shutting down, interrupting plotting"
                                );
                                return;
                            }

                            let farmer_protocol_info =
                                handle.block_on(rpc_client.farmer_protocol_info()).map_err(
                                    |error| PlottingError::FailedToGetFarmerProtocolInfo { error },
                                )?;

                            match handle.block_on(plot_sector(
                                &public_key,
                                sector_index,
                                |piece_index| rpc_client.get_piece(piece_index),
                                &shutting_down,
                                &farmer_protocol_info,
                                io::Cursor::new(sector),
                                io::Cursor::new(sector_metadata),
                            ))? {
                                PlottingStatus::PlottedSuccessfully => {
                                    let mut metadata_header = metadata_header.lock();
                                    metadata_header.sector_count += 1;
                                    metadata_header_mmap
                                        .copy_from_slice(metadata_header.encode().as_slice());
                                }
                                PlottingStatus::Cancelled => {
                                    return;
                                }
                            }
                        }
                    };

                    if let Err(error) = initial_plotting_result {
                        if let Some(error_sender) = error_sender.lock().take() {
                            if let Err(error) = error_sender.send(error) {
                                error!(%error, "Plotting failed to send error to background task");
                            }
                        }
                    }
                }
            })?;

        let farming_join_handle = thread::Builder::new()
            .name(format!("f-{single_disk_plot_id}"))
            .spawn({
                let shutting_down = Arc::clone(&shutting_down);
                let identity = identity.clone();
                let rpc_client = rpc_client.clone();

                move || {
                    let _tokio_handle_guard = handle.enter();
                    let span = info_span!("single_disk_plot", %single_disk_plot_id);
                    let _span_guard = span.enter();

                    let farming_result = try {
                        info!("Subscribing to slot info notifications");
                        let mut slot_info_notifications = handle
                            .block_on(rpc_client.subscribe_slot_info())
                            .map_err(|error| FarmingError::FailedToGetFarmerProtocolInfo {
                                error,
                            })?;

                        while let Some(slot_info) = handle.block_on(slot_info_notifications.next())
                        {
                            debug!(?slot_info, "New slot");

                            let sector_count = metadata_header.lock().sector_count;
                            let plot_mmap = unsafe {
                                MmapOptions::new()
                                    .len((plot_sector_size * sector_count) as usize)
                                    .map(&plot_file)
                                    .map_err(|error| FarmingError::FailedToMapPlot { error })?
                            };
                            #[cfg(unix)]
                            {
                                plot_mmap.advise(memmap2::Advice::Random).unwrap();
                            }
                            let metadata_mmap = unsafe {
                                MmapOptions::new()
                                    .offset(RESERVED_PLOT_METADATA)
                                    .len(SectorMetadata::encoded_size() * sector_count as usize)
                                    .map(&metadata_file)
                                    .map_err(|error| FarmingError::FailedToMapMetadata { error })?
                            };
                            #[cfg(unix)]
                            {
                                metadata_mmap.advise(memmap2::Advice::Random).unwrap();
                            }
                            let shutting_down = Arc::clone(&shutting_down);

                            let mut solutions = Vec::<Solution<PublicKey, PublicKey>>::new();

                            for (sector_index, sector, sector_metadata) in plot_mmap
                                .chunks_exact(plot_sector_size as usize)
                                .zip(metadata_mmap.chunks_exact(SectorMetadata::encoded_size()))
                                .enumerate()
                                .map(|(sector_index, (sector, metadata))| {
                                    (sector_index as u64 + first_sector_index, sector, metadata)
                                })
                            {
                                if shutting_down.load(Ordering::Acquire) {
                                    debug!(
                                        %sector_index,
                                        "Instance is shutting down, interrupting plotting"
                                    );
                                    return;
                                }

                                let eligible_sector = match audit_sector(
                                    &public_key,
                                    sector_index,
                                    &farmer_protocol_info,
                                    &slot_info.global_challenge,
                                    slot_info.voting_solution_range,
                                    sector,
                                )? {
                                    Some(eligible_sector) => eligible_sector,
                                    None => {
                                        continue;
                                    }
                                };

                                let solution = match create_solution(
                                    &identity,
                                    eligible_sector,
                                    reward_address,
                                    &farmer_protocol_info,
                                    sector_metadata,
                                )? {
                                    Some(solution) => solution,
                                    None => {
                                        continue;
                                    }
                                };

                                debug!("Solution found");
                                trace!(?solution, "Solution found");

                                solutions.push(solution);
                            }

                            handle
                                .block_on(rpc_client.submit_solution_response(SolutionResponse {
                                    slot_number: slot_info.slot_number,
                                    solutions,
                                }))
                                .map_err(|error| FarmingError::FailedToSubmitSolutionsResponse {
                                    error,
                                })?;
                        }
                    };

                    if let Err(error) = farming_result {
                        if let Some(error_sender) = error_sender.lock().take() {
                            if let Err(error) = error_sender.send(error) {
                                error!(%error, "Farming failed to send error to background task");
                            }
                        }
                    }
                }
            })?;

        tasks.push(Box::pin(async move {
            // TODO: Error handling here
            reward_signing(rpc_client, identity).await.unwrap().await;

            Ok(())
        }));

        let farm = Self {
            id: single_disk_plot_id,
            span: Span::current(),
            tasks,
            _plotting_join_handle: JoinOnDrop::new(plotting_join_handle),
            _farming_join_handle: JoinOnDrop::new(farming_join_handle),
            shutting_down,
        };

        Ok(farm)
    }

    /// Collect summary of single disk plot for presentational purposes
    pub fn collect_summary(directory: PathBuf) -> SingleDiskPlotSummary {
        let single_disk_plot_info = match SingleDiskPlotInfo::load_from(&directory) {
            Ok(Some(single_disk_plot_info)) => single_disk_plot_info,
            Ok(None) => {
                return SingleDiskPlotSummary::NotFound { directory };
            }
            Err(error) => {
                return SingleDiskPlotSummary::Error { directory, error };
            }
        };

        SingleDiskPlotSummary::Found {
            info: single_disk_plot_info,
            directory,
        }
    }

    /// ID of this farm
    pub fn id(&self) -> &SingleDiskPlotId {
        &self.id
    }

    /// Wait for background threads to exit or return an error
    pub async fn wait(mut self) -> anyhow::Result<()> {
        while let Some(result) = self.tasks.next().instrument(self.span.clone()).await {
            result?;
        }

        Ok(())
    }

    /// Wipe everything that belongs to this single disk plot
    pub fn wipe(directory: &Path) -> io::Result<()> {
        let single_disk_plot_info_path = directory.join(SingleDiskPlotInfo::FILE_NAME);
        let single_disk_plot_info = SingleDiskPlotInfo::load_from(directory)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "Single disk plot info not found at {}",
                    single_disk_plot_info_path.display()
                ),
            )
        })?;

        info!("Found single disk plot {}", single_disk_plot_info.id());

        {
            let plot = directory.join(Self::PLOT_FILE);
            info!("Deleting plot file at {}", plot.display());
            fs::remove_file(plot)?;
        }
        {
            let metadata = directory.join(Self::METADATA_FILE);
            info!("Deleting metadata file at {}", metadata.display());
            fs::remove_file(metadata)?;
        }
        // TODO: Identity should be able to wipe itself instead of assuming a specific file name
        //  here
        {
            let identity = directory.join("identity.bin");
            info!("Deleting identity file at {}", identity.display());
            fs::remove_file(identity)?;
        }

        info!(
            "Deleting info file at {}",
            single_disk_plot_info_path.display()
        );
        fs::remove_file(single_disk_plot_info_path)
    }
}
