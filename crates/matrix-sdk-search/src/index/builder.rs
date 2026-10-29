use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, PoisonError, RwLock},
};

use ruma::{OwnedRoomId, RoomId};
use tantivy::{
    Index, TantivyError,
    directory::{MmapDirectory, error::OpenDirectoryError},
};
use tracing::warn;
use zeroize::Zeroizing;

use crate::{
    encrypted::encrypted_dir::EncryptedMmapDirectory,
    error::IndexError,
    index::RoomIndex,
    schema::{MatrixSearchIndexSchema, RoomMessageSchema},
};

/// Builder for [`RoomIndex`].
pub struct RoomIndexBuilder {}

impl RoomIndexBuilder {
    /// Make an index on disk
    pub fn new_on_disk<R: Into<OwnedRoomId>>(
        path: PathBuf,
        room_id: R,
    ) -> PhysicalRoomIndexBuilder {
        PhysicalRoomIndexBuilder::new(path, room_id.into())
    }

    /// Make an index in memory
    pub fn new_in_memory<R: Into<OwnedRoomId>>(room_id: R) -> MemoryRoomIndexBuilder {
        MemoryRoomIndexBuilder::new(room_id.into())
    }
}

/// Incomplete builder for [`RoomIndex`] on disk.
pub struct PhysicalRoomIndexBuilder {
    path: PathBuf,
    room_id: OwnedRoomId,
}

impl PhysicalRoomIndexBuilder {
    /// Make an new [`PhysicalRoomIndexBuilder`]
    pub(crate) fn new(path: PathBuf, room_id: OwnedRoomId) -> PhysicalRoomIndexBuilder {
        PhysicalRoomIndexBuilder { path, room_id }
    }

    /// Make an unencrypted index
    pub fn unencrypted(&self) -> UnencryptedPhysicalRoomIndexBuilder {
        UnencryptedPhysicalRoomIndexBuilder {
            path: self.path.clone(),
            room_id: self.room_id.clone(),
        }
    }

    /// Make an index encrypted with the given high-entropy key
    pub fn encrypted<K: Into<String>>(&self, key: K) -> EncryptedPhysicalRoomIndexBuilder {
        EncryptedPhysicalRoomIndexBuilder {
            path: self.path.clone(),
            room_id: self.room_id.clone(),
            key: Zeroizing::new(key.into()),
        }
    }
}

/// Complete builder for [`RoomIndex`] on disk.
pub struct UnencryptedPhysicalRoomIndexBuilder {
    path: PathBuf,
    room_id: OwnedRoomId,
}

impl UnencryptedPhysicalRoomIndexBuilder {
    /// Build the [`RoomIndex`].
    ///
    /// An existing index with another schema, e.g. from an older version, is
    /// deleted and recreated.
    pub fn build(&self) -> Result<RoomIndex, IndexError> {
        open_or_recreate(
            &self.path,
            &self.room_id,
            |err| matches!(err, IndexError::TantivyError(TantivyError::SchemaError(_))),
            |path| self.open(path),
        )
    }

    fn open(&self, path: &Path) -> Result<RoomIndex, IndexError> {
        let mmap_dir = match MmapDirectory::open(path) {
            Ok(dir) => Ok(dir),
            Err(err) => match err {
                OpenDirectoryError::DoesNotExist(path) => {
                    fs::create_dir_all(path.clone()).map_err(|err| {
                        OpenDirectoryError::IoError {
                            io_error: Arc::new(err),
                            directory_path: path.to_path_buf(),
                        }
                    })?;
                    MmapDirectory::open(path)
                }
                _ => Err(err),
            },
        }?;
        let schema = RoomMessageSchema::new();
        let index = Index::open_or_create(mmap_dir, schema.as_tantivy_schema())?;
        Ok(RoomIndex::new_with(index, schema, &self.room_id))
    }
}

/// Complete builder for [`RoomIndex`] on disk.
pub struct EncryptedPhysicalRoomIndexBuilder {
    path: PathBuf,
    room_id: OwnedRoomId,
    key: Zeroizing<String>,
}

impl EncryptedPhysicalRoomIndexBuilder {
    /// Build the [`RoomIndex`].
    ///
    /// An existing index that can't be opened, e.g. because it was encrypted
    /// with another key or in an older format, is deleted and recreated.
    pub fn build(&self) -> Result<RoomIndex, IndexError> {
        // Any failure to open recreates the index, as it can't tell a wrong
        // key from other errors.
        open_or_recreate(&self.path, &self.room_id, |_| true, |path| self.open(path))
    }

    fn open(&self, path: &Path) -> Result<RoomIndex, IndexError> {
        let mmap_dir = EncryptedMmapDirectory::open_or_create(path, &self.key)?;
        let schema = RoomMessageSchema::new();
        let index = Index::open_or_create(mmap_dir, schema.as_tantivy_schema())?;
        Ok(RoomIndex::new_with(index, schema, &self.room_id))
    }
}

/// Taken for writing while an index is recreated, so that no other thread opens
/// the files being deleted.
static RECREATE_LOCK: RwLock<()> = RwLock::new(());

/// Open the index of `room_id` in `base`, or delete and recreate it if opening
/// fails with an error for which `should_recreate` is true.
fn open_or_recreate(
    base: &Path,
    room_id: &RoomId,
    should_recreate: impl Fn(&IndexError) -> bool,
    open: impl Fn(&Path) -> Result<RoomIndex, IndexError>,
) -> Result<RoomIndex, IndexError> {
    let path = base.join(room_id.as_str());

    let err = {
        let _lock = RECREATE_LOCK.read().unwrap_or_else(PoisonError::into_inner);
        match open(&path) {
            Ok(index) => return Ok(index),
            Err(err) => err,
        }
    };
    if !should_recreate(&err) {
        return Err(err);
    }

    let _lock = RECREATE_LOCK.write().unwrap_or_else(PoisonError::into_inner);

    // Another thread may have recreated it while this one waited.
    if let Ok(index) = open(&path) {
        return Ok(index);
    }

    warn!(%room_id, "Recreating the search index: {err}");
    fs::remove_dir_all(&path)?;
    open(&path)
}

/// Builder for [`RoomIndex`] in memory
pub struct MemoryRoomIndexBuilder {
    room_id: OwnedRoomId,
}

impl MemoryRoomIndexBuilder {
    /// Make an new [`MemoryIndexBuilder`]
    pub(crate) fn new(room_id: OwnedRoomId) -> MemoryRoomIndexBuilder {
        MemoryRoomIndexBuilder { room_id }
    }

    /// Build the [`RoomIndex`]
    pub fn build(&self) -> RoomIndex {
        let schema = RoomMessageSchema::new();
        let index = Index::create_in_ram(schema.as_tantivy_schema());
        RoomIndex::new_with(index, schema, &self.room_id)
    }
}
