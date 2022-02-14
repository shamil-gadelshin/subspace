use super::CommitmentError;
use log::error;
use parking_lot::Mutex;
use rocksdb::{Options, DB};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use subspace_core_primitives::Salt;

const COMMITMENTS_KEY: &[u8] = b"commitments";

#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
enum CommitmentStatus {
    /// In-progress commitment to the part of the plot
    InProgress,
    /// Commitment to the whole plot and not some in-progress partial commitment
    Created,
}

pub(super) struct CreateDbEntryResult {
    pub(super) db_entry: Arc<DbEntry>,
    pub(super) removed_entry_salt: Option<Salt>,
}

pub(super) struct DbEntry {
    salt: Salt,
    db: Mutex<Option<Arc<DB>>>,
}

impl fmt::Debug for DbEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DbEntry").field("salt", &self.salt).finish()
    }
}

impl Deref for DbEntry {
    type Target = Mutex<Option<Arc<DB>>>;

    fn deref(&self) -> &Self::Target {
        &self.db
    }
}

#[derive(Debug)]
pub(super) struct CommitmentDatabases {
    base_directory: PathBuf,
    first_db: Mutex<Option<Arc<DbEntry>>>,
    second_db: Mutex<Option<Arc<DbEntry>>>,
    metadata: Mutex<CommitmentMetadata>,
    switch: AtomicBool,
}

#[derive(Debug)]
pub(super) struct CommitmentMetadata {
    metadata_cache: HashMap<Salt, CommitmentStatus>,
    metadata_db: Arc<DB>,
}

impl CommitmentDatabases {
    pub(super) fn new(base_directory: PathBuf) -> Result<Self, CommitmentError> {
        let metadata_db = DB::open_default(base_directory.join("metadata"))
            .map_err(CommitmentError::MetadataDb)?;
        let metadata_cache: HashMap<Salt, CommitmentStatus> = metadata_db
            .get(COMMITMENTS_KEY)
            .map_err(CommitmentError::MetadataDb)?
            .map(|bytes| {
                serde_json::from_slice::<HashMap<String, CommitmentStatus>>(&bytes)
                    .unwrap()
                    .into_iter()
                    .map(|(salt, status)| (hex::decode(salt).unwrap().try_into().unwrap(), status))
                    .collect()
            })
            .unwrap_or_default();

        let commitment_metadata = CommitmentMetadata {
            metadata_cache,
            metadata_db: Arc::new(metadata_db),
        };

        let commitment_databases = CommitmentDatabases {
            base_directory: base_directory.clone(),
            first_db: Mutex::new(None),
            second_db: Mutex::new(None),
            metadata: Mutex::new(commitment_metadata),
            switch: AtomicBool::new(true),
        };

        if commitment_databases
            .metadata
            .lock()
            .metadata_cache
            .drain_filter(|salt, status| match status {
                CommitmentStatus::InProgress => {
                    if let Err(error) =
                        std::fs::remove_dir_all(base_directory.join(hex::encode(salt)))
                    {
                        error!(
                            "Failed to remove old in progress commitment {}: {}",
                            hex::encode(salt),
                            error
                        );
                    }
                    true
                }
                CommitmentStatus::Created => false,
            })
            .next()
            .is_some()
        {
            commitment_databases.persist_metadata_cache()?;
        }

        // Open databases that were fully created during previous run
        let guard = commitment_databases.metadata.lock();
        let first_salt = guard.metadata_cache.keys().next();
        let second_salt = guard.metadata_cache.keys().next();

        // Open the first one
        if let Some(first_salt) = first_salt {
            let db = DB::open(
                &Options::default(),
                base_directory.join(hex::encode(first_salt)),
            )
            .map_err(CommitmentError::CommitmentDb)?;
            commitment_databases
                .first_db
                .lock()
                .replace(Arc::new(DbEntry {
                    salt: *first_salt,
                    db: Mutex::new(Some(Arc::new(db))),
                }));
        }
        // Open the second one
        if let Some(second_salt) = second_salt {
            let db = DB::open(
                &Options::default(),
                base_directory.join(hex::encode(second_salt)),
            )
            .map_err(CommitmentError::CommitmentDb)?;
            commitment_databases
                .second_db
                .lock()
                .replace(Arc::new(DbEntry {
                    salt: *second_salt,
                    db: Mutex::new(Some(Arc::new(db))),
                }));
        }
        drop(guard);

        Ok::<_, CommitmentError>(commitment_databases)
    }

    /// Get salts for all current database entries
    pub(super) fn get_salts(&self) -> Vec<Salt> {
        self.metadata
            .lock()
            .metadata_cache
            .iter()
            .map(|(salt, _commitment_status)| *salt)
            .collect()
    }

    pub(super) fn get_db_entry(&self, salt: &Salt) -> Option<Arc<DbEntry>> {
        // try to acquire the locks, if we cannot acquire them,
        // it means they are being swapped/overwritten, and we don't want to query them
        let first_guard = self.first_db.try_lock();
        let second_guard = self.second_db.try_lock();

        // check if salt matches with the firstDB
        if let Some(guard) = first_guard {
            if guard.is_some() && &guard.as_ref().unwrap().salt == salt {
                return guard.clone();
            }
        }
        // check if salt matches with the secondDB
        if let Some(guard) = second_guard {
            if guard.is_some() && &guard.as_ref().unwrap().salt == salt {
                return guard.clone();
            }
        }

        // if there is no DB with the given salt
        None
    }

    /// Returns `Ok(None)` if entry for this salt already exists.
    pub(super) fn create_db_entry(
        &self,
        salt: Salt,
    ) -> Result<Option<CreateDbEntryResult>, CommitmentError> {
        if self.get_db_entry(&salt).is_some() {
            return Ok(None);
        }
        // create the new db_entry
        let db_entry = Arc::new(DbEntry {
            salt,
            db: Mutex::new(None),
        });
        let mut removed_entry_salt = None;

        // we are using a boolean switch to determine which database to override.
        // in each round, either firstDB or secondDB will become the old database, in round-robin fashion.
        // find which database is the old one with the help of the switch, and flip it afterwards via `XOR 1`
        let condition = self.switch.fetch_xor(true, Ordering::SeqCst);

        // don't touch the active database, only replace the old database
        let old_db_entry = match condition {
            true => self.first_db.lock().replace(Arc::clone(&db_entry)),
            false => self.second_db.lock().replace(Arc::clone(&db_entry)),
        };

        if let Some(old_db_entry) = old_db_entry {
            let old_salt = old_db_entry.salt;
            removed_entry_salt.replace(old_salt);
            let old_db_path = self.base_directory.join(hex::encode(old_salt));

            // Remove old commitments for `old_salt` from the metadata_cache
            self.metadata.lock().metadata_cache.remove(&old_salt);

            tokio::task::spawn_blocking(move || {
                // Take a lock to make sure database was released by whatever user there was and we
                // have an exclusive access to it, then drop it
                old_db_entry.db.lock().take();

                if let Err(error) = std::fs::remove_dir_all(old_db_path) {
                    error!(
                        "Failed to remove old commitment for salt {}: {}",
                        hex::encode(old_salt),
                        error
                    );
                }
            });
        }

        self.mark_in_progress(salt)?;

        Ok(Some(CreateDbEntryResult {
            db_entry,
            removed_entry_salt,
        }))
    }

    pub(super) fn mark_in_progress(&self, salt: Salt) -> Result<(), CommitmentError> {
        self.update_status(salt, CommitmentStatus::InProgress)
    }

    pub(super) fn mark_created(&self, salt: Salt) -> Result<(), CommitmentError> {
        self.update_status(salt, CommitmentStatus::Created)
    }

    fn update_status(&self, salt: Salt, status: CommitmentStatus) -> Result<(), CommitmentError> {
        self.metadata.lock().metadata_cache.insert(salt, status);
        self.persist_metadata_cache()
    }

    fn persist_metadata_cache(&self) -> Result<(), CommitmentError> {
        let prepared_metadata_cache: HashMap<String, CommitmentStatus> = self
            .metadata
            .lock()
            .metadata_cache
            .iter()
            .map(|(salt, status)| (hex::encode(salt), *status))
            .collect();

        self.metadata
            .lock()
            .metadata_db
            .put(
                COMMITMENTS_KEY,
                &serde_json::to_vec(&prepared_metadata_cache).unwrap(),
            )
            .map_err(CommitmentError::MetadataDb)
    }
}
