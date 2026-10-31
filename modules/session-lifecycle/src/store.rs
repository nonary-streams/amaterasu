//! Atomic, bounded storage for lifecycle recovery state.
//!
//! Callers validate the decoded document and serialize all access to a path.
//! An error during replacement makes its outcome uncertain; reopen and load the
//! store before admitting another effect. The parent directory must already
//! exist and be private to the host process; an existing journal must be owned
//! by the same user as the writer so replacement preserves ownership. On Unix,
//! the journal must be private (0600); replacement inherits Windows parent ACLs.
//! Windows uses the shared write-through replacement primitive; platform crash
//! durability remains subject to filesystem guarantees.

use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_DOCUMENT_BYTES: u64 = 1024 * 1024;
#[cfg(test)]
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct LifecycleStore {
    path: PathBuf,
    poisoned: bool,
}

impl LifecycleStore {
    /// The containing directory must already exist and be controlled by the host.
    pub fn new(path: PathBuf) -> io::Result<Self> {
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid journal path",
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "journal has no parent"))?;
        if !fs::metadata(parent)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "journal parent is not a directory",
            ));
        }
        validate_parent(parent)?;
        Ok(Self {
            path,
            poisoned: false,
        })
    }

    /// A missing journal is distinct from a corrupt or unreadable journal.
    pub fn load<T: DeserializeOwned>(&self) -> io::Result<Option<T>> {
        validate_parent(self.path.parent().expect("validated parent"))?;
        if !validate_file(&self.path)? {
            return Ok(None);
        }
        let bytes = amaterasu_storage::read_bounded(&self.path, MAX_DOCUMENT_BYTES)
            .map_err(io::Error::other)?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    /// Persist before issuing the effect represented by `value`.
    pub fn replace<T: Serialize>(&mut self, value: &T) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::other(
                "journal requires reopening after failed replacement",
            ));
        }
        let value = serde_json::to_value(value).map_err(io::Error::other)?;
        let bytes = serde_json::to_vec_pretty(&value).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "journal too large",
            ));
        }
        let result = (|| {
            validate_parent(self.path.parent().expect("validated parent"))?;
            validate_file(&self.path)?;
            amaterasu_storage::write_json_atomically(&self.path, &value).map_err(io::Error::other)
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
}

fn validate_parent(parent: &Path) -> io::Result<()> {
    for component in parent.ancestors() {
        let metadata = fs::symlink_metadata(component)?;
        if !metadata.is_dir() || is_link(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "journal path crosses a link or non-directory",
            ));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(parent)?.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "journal directory is not private",
            ));
        }
    }
    Ok(())
}

fn validate_file(path: &Path) -> io::Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || is_link(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid journal file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let parent = fs::metadata(path.parent().expect("validated parent"))?;
        if metadata.uid() != parent.uid() || metadata.permissions().mode() & 0o7777 != 0o600 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "journal owner or permissions are unsafe",
            ));
        }
    }
    Ok(true)
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0 || metadata.file_type().is_symlink()
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct State {
        revision: u64,
        obligations: Vec<String>,
    }

    fn test_dir() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-lifecycle-store-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        root
    }

    #[test]
    fn replacement_survives_reopen_and_corruption_is_not_empty_state() {
        let root = test_dir();
        let path = root.join("journal.json");
        let mut store = LifecycleStore::new(path.clone()).unwrap();
        assert_eq!(store.load::<State>().unwrap(), None);
        let state = State {
            revision: 1,
            obligations: vec!["display".into()],
        };
        store.replace(&state).unwrap();
        store
            .replace(&State {
                revision: 2,
                ..state
            })
            .unwrap();
        let reopened = LifecycleStore::new(path.clone()).unwrap();
        assert_eq!(reopened.load::<State>().unwrap().unwrap().revision, 2);
        fs::write(&path, b"{").unwrap();
        assert_eq!(
            reopened.load::<State>().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_replacement_fences_further_writes() {
        let root = test_dir();
        let path = root.join("journal.json");
        let mut store = LifecycleStore::new(path.clone()).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(store.replace(&1).is_err());
        fs::remove_dir(&path).unwrap();
        assert!(store.replace(&2).is_err());
        assert!(
            LifecycleStore::new(path)
                .unwrap()
                .load::<u64>()
                .unwrap()
                .is_none()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn replacement_rejects_unsafe_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let root = test_dir();
        let path = root.join("journal.json");
        let mut store = LifecycleStore::new(path.clone()).unwrap();
        store.replace(&1).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(store.replace(&2).is_err());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fs::remove_dir_all(root).unwrap();
    }
}
