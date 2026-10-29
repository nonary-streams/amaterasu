//! Root-owned approval records, separated from package-controlled policy.
use crate::approval::{ApprovalRecord, approval_id, validate_digest};
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{DirBuilderExt, MetadataExt},
    },
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const MAX_APPROVAL_BYTES: u64 = 65_536;
static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(1);

pub struct ApprovalStore {
    path: PathBuf,
    directory: File,
    pinned: Option<PinnedApproval>,
}

struct PinnedApproval {
    identifier: String,
    revision: RecordRevision,
    // Keeps the revoked inode allocated, preventing inode reuse from restoring
    // an old session when the identical approval is written again.
    file: File,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RecordRevision {
    device: u64,
    inode: u64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl RecordRevision {
    fn of(file: &File) -> Result<Self, String> {
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
}

struct OpenedApproval {
    record: ApprovalRecord,
    file: File,
    revision: RecordRevision,
}

impl ApprovalStore {
    /// Opens a protected store. Every ancestor must be a real root-owned
    /// directory without group/other write access; the final directory is private.
    pub fn open(path: &Path) -> Result<Self, String> {
        let directory = open_protected_directory(path, true)?;
        Ok(Self {
            path: path.to_owned(),
            directory,
            pinned: None,
        })
    }

    /// Creates only the final directory beneath an already protected parent.
    /// This never fixes ownership or permissions on an existing directory.
    pub fn create(path: &Path) -> Result<Self, String> {
        require_root()?;
        validate_absolute_path(path)?;
        let parent = path.parent().ok_or("approval store requires a parent")?;
        let _parent = open_protected_directory(parent, false)?;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
        Self::open(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Shares the pinned protected directory with another broker session.
    pub fn try_clone(&self) -> Result<Self, String> {
        check_directory(&self.directory, true)?;
        Ok(Self {
            path: self.path.clone(),
            directory: self
                .directory
                .try_clone()
                .map_err(|error| error.to_string())?,
            pinned: self
                .pinned
                .as_ref()
                .map(|pinned| {
                    Ok::<_, String>(PinnedApproval {
                        identifier: pinned.identifier.clone(),
                        revision: pinned.revision,
                        file: pinned.file.try_clone().map_err(|error| error.to_string())?,
                    })
                })
                .transpose()?,
        })
    }

    /// Pins the exact approval instance used to start a worker. Revoking and
    /// reapproving identical bytes never restores authority to an old session.
    pub fn pin(&mut self, identifier: &str, expected: &ApprovalRecord) -> Result<(), String> {
        if self.pinned.is_some() {
            return Err("approval store is already bound to an active approval instance".into());
        }
        let _lock = self.lock(false)?;
        let opened = self.read_record(identifier)?;
        if opened.record != *expected {
            return Err("protected approval changed before worker authorization".into());
        }
        self.pinned = Some(PinnedApproval {
            identifier: identifier.to_owned(),
            revision: opened.revision,
            file: opened.file,
        });
        Ok(())
    }

    pub fn put(&self, record: &ApprovalRecord) -> Result<String, String> {
        require_root()?;
        let _lock = self.lock(true)?;
        let identifier = approval_id(record)?;
        let encoded = serde_json::to_vec(record).map_err(|error| error.to_string())?;
        if encoded.len() as u64 > MAX_APPROVAL_BYTES {
            return Err("approval record exceeds size limit".into());
        }
        let temporary = format!(
            ".approval-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        );
        let source = std::ffi::CString::new(temporary.as_bytes()).unwrap();
        let result = (|| {
            // SAFETY: the temporary name is a NUL-free simple component and
            // creation is relative to the same pinned directory that is locked.
            let descriptor = unsafe {
                libc::openat(
                    self.directory.as_raw_fd(),
                    source.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if descriptor < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            // SAFETY: openat returned a new descriptor owned only by this value.
            let mut file = unsafe { File::from_raw_fd(descriptor) };
            check_private_file(&file)?;
            file.write_all(&encoded)
                .map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
            let destination = record_name(&identifier)?;
            // SAFETY: both names are bounded simple filenames and directory is
            // an owned open descriptor; rename occurs inside that same directory.
            if unsafe {
                libc::renameat(
                    self.directory.as_raw_fd(),
                    source.as_ptr(),
                    self.directory.as_raw_fd(),
                    destination.as_ptr(),
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
            self.directory.sync_all().map_err(|error| error.to_string())
        })();
        if result.is_err() {
            // SAFETY: cleanup uses the generated simple name in the pinned
            // directory; it cannot remove a path outside the approval store.
            unsafe { libc::unlinkat(self.directory.as_raw_fd(), source.as_ptr(), 0) };
        }
        result.map(|()| identifier)
    }

    /// Reopens the record each time, so revocation and replacement are observed.
    /// A running store pins its directory inode. Administrators revoke records
    /// through this API; replacing the entire directory requires restarting the
    /// broker rather than silently rebinding its trusted state.
    pub fn get(&self, identifier: &str) -> Result<ApprovalRecord, String> {
        let opened = self.read_record(identifier)?;
        if let Some(pinned) = &self.pinned
            && pinned.identifier == identifier
            && pinned.revision != opened.revision
        {
            return Err("active approval instance was revoked or replaced".into());
        }
        Ok(opened.record)
    }

    fn read_record(&self, identifier: &str) -> Result<OpenedApproval, String> {
        check_directory(&self.directory, true)?;
        let filename = record_name(identifier)?;
        // SAFETY: filename is a NUL-free simple name and the directory descriptor
        // remains open. O_NOFOLLOW rejects links before any record bytes are read.
        let descriptor = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                filename.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if descriptor < 0 {
            return Err(format!(
                "approval unavailable or revoked: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: openat returned a new descriptor owned only by this value.
        let file = unsafe { File::from_raw_fd(descriptor) };
        check_private_file(&file)?;
        let revision = RecordRevision::of(&file)?;
        let mut encoded = Vec::new();
        (&file)
            .take(MAX_APPROVAL_BYTES + 1)
            .read_to_end(&mut encoded)
            .map_err(|error| error.to_string())?;
        if encoded.len() as u64 > MAX_APPROVAL_BYTES {
            return Err("approval record exceeds size limit".into());
        }
        let record: ApprovalRecord =
            serde_json::from_slice(&encoded).map_err(|error| error.to_string())?;
        if approval_id(&record)? != identifier {
            return Err("protected approval record identity mismatch".into());
        }
        if RecordRevision::of(&file)? != revision {
            return Err("protected approval changed during read".into());
        }
        Ok(OpenedApproval {
            record,
            file,
            revision,
        })
    }

    /// Linearizes a bounded privileged operation against approval mutations.
    /// The closure must not mutate this store or wait for unbounded peer input.
    pub fn with_current<T>(
        &self,
        identifier: &str,
        expected: &ApprovalRecord,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _lock = self.lock(false)?;
        if self.get(identifier)? != *expected {
            return Err("protected approval no longer matches the active authority".into());
        }
        operation()
    }

    pub fn remove(&self, identifier: &str) -> Result<(), String> {
        require_root()?;
        let _lock = self.lock(true)?;
        let filename = record_name(identifier)?;
        // SAFETY: filename is a validated single path component, and the owned
        // directory descriptor pins the actual store rather than a path alias.
        if unsafe { libc::unlinkat(self.directory.as_raw_fd(), filename.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        self.directory.sync_all().map_err(|error| error.to_string())
    }

    fn lock(&self, exclusive: bool) -> Result<DirectoryLock, String> {
        check_directory(&self.directory, true)?;
        let lock = DirectoryLock::acquire(&self.directory, exclusive)?;
        check_directory(&self.directory, true)?;
        Ok(lock)
    }
}

/// Each lock owns a new open file description. Duplicating a directory File
/// would share flock state and allow one session to release another's lock.
struct DirectoryLock {
    _descriptor: File,
}

impl DirectoryLock {
    fn acquire(directory: &File, exclusive: bool) -> Result<Self, String> {
        // SAFETY: the literal is NUL-terminated and the pinned directory is
        // alive. Opening "." obtains a fresh independent open description.
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                c".".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: openat returned a new descriptor owned only by this value.
        let descriptor = unsafe { File::from_raw_fd(descriptor) };
        let operation = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        loop {
            // SAFETY: flock operates on this owned, open directory descriptor.
            if unsafe { libc::flock(descriptor.as_raw_fd(), operation) } == 0 {
                return Ok(Self {
                    _descriptor: descriptor,
                });
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.to_string());
            }
        }
    }
}

fn record_name(identifier: &str) -> Result<std::ffi::CString, String> {
    validate_digest(identifier)?;
    Ok(std::ffi::CString::new(format!("{identifier}.json")).unwrap())
}

pub fn require_root() -> Result<(), String> {
    // SAFETY: geteuid has no arguments or memory access preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return Err("administrative OS authorization is required".into());
    }
    Ok(())
}

pub fn validate_absolute_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err("protected path must be absolute without parent traversal".into());
    }
    Ok(())
}

/// Opens a directory chain without following any symlink. Root-owned paths
/// writable by ordinary users are refused, including sticky temporary folders.
pub fn open_protected_directory(path: &Path, private: bool) -> Result<File, String> {
    use std::os::unix::ffi::OsStrExt;
    validate_absolute_path(path)?;
    let mut directory = File::open("/").map_err(|error| error.to_string())?;
    check_directory(&directory, false)?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let name = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| "protected path contains a NUL".to_string())?;
        // SAFETY: each component is a validated simple name. The directory
        // descriptor pins its parent; O_NOFOLLOW forbids a symlink redirect.
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if descriptor < 0 {
            return Err(format!(
                "cannot open protected directory: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: openat returned a new descriptor owned only by this value.
        directory = unsafe { File::from_raw_fd(descriptor) };
        check_directory(&directory, false)?;
    }
    check_directory(&directory, private)?;
    Ok(directory)
}

fn check_directory(file: &File, private: bool) -> Result<(), String> {
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    validate_directory_facts(metadata.is_dir(), metadata.uid(), metadata.mode(), private)
}

fn validate_directory_facts(
    is_directory: bool,
    owner: u32,
    mode: u32,
    private: bool,
) -> Result<(), String> {
    let prohibited = if private { 0o077 } else { 0o022 };
    if !is_directory || owner != 0 || mode & prohibited != 0 || mode & 0o6000 != 0 {
        return Err("approval path is not a protected root-owned directory".into());
    }
    Ok(())
}

fn check_private_file(file: &File) -> Result<(), String> {
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o7077 != 0
        || metadata.nlink() != 1
    {
        return Err("approval record is not a private root-owned regular file".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_ancestors_reject_shared_writable_and_user_owned_paths() {
        assert!(validate_directory_facts(true, 0, 0o755, false).is_ok());
        assert!(validate_directory_facts(true, 0, 0o700, true).is_ok());
        for (owner, mode, private) in [
            (1000, 0o700, true),
            (0, 0o777, false),
            (0, 0o1777, false),
            (0, 0o775, false),
            (0, 0o755, true),
            (0, 0o2700, true),
        ] {
            assert!(validate_directory_facts(true, owner, mode, private).is_err());
        }
        assert!(validate_directory_facts(false, 0, 0o700, true).is_err());
    }

    #[test]
    fn retained_record_descriptor_distinguishes_identical_replacement() {
        let directory = std::env::temp_dir().join(format!(
            "amaterasu-approval-revision-test-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("approval.json");
        std::fs::write(&path, b"identical record").unwrap();
        let retained = File::open(&path).unwrap();
        let original = RecordRevision::of(&retained).unwrap();
        let replacement = directory.join("replacement.json");
        std::fs::write(&replacement, b"identical record").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let current = File::open(&path).unwrap();
        let revised = RecordRevision::of(&current).unwrap();
        // The retained descriptor keeps the old inode allocated even though
        // its path now names byte-for-byte identical replacement approval.
        assert_ne!(original.inode, revised.inode);
        assert_ne!(original, revised);
        drop(current);
        drop(retained);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn directory_locks_are_independent_and_exclude_mutation() {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-approval-lock-test-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        let directory = File::open(&path).unwrap();
        let first = DirectoryLock::acquire(&directory, false).unwrap();
        let second = DirectoryLock::acquire(&directory, false).unwrap();
        let contender = File::open(&path).unwrap();
        let try_exclusive = || {
            // SAFETY: contender is an owned descriptor of this test directory.
            unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }
        };
        assert_ne!(try_exclusive(), 0);
        drop(first);
        // Releasing one shared lock must not release the other session's lock.
        assert_ne!(try_exclusive(), 0);
        drop(second);
        assert_eq!(try_exclusive(), 0);
        drop(contender);
        drop(directory);
        std::fs::remove_dir(&path).unwrap();
    }

    #[test]
    #[ignore = "requires an administrative process and creates a private directory under /run"]
    fn protected_store_persists_tamper_checks_and_revocation() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        require_root().unwrap();
        let root = PathBuf::from(format!(
            "/run/amaterasu-authority-store-test-{}",
            std::process::id()
        ));
        let store = ApprovalStore::create(&root).unwrap();
        let record = crate::approval::tests::approval();
        let identifier = store.put(&record).unwrap();
        assert_eq!(store.get(&identifier).unwrap(), record);
        assert_eq!(
            store.with_current(&identifier, &record, || Ok(42)).unwrap(),
            42
        );
        let mut mismatched = record.clone();
        mismatched.uid += 1;
        assert!(
            store
                .with_current::<()>(&identifier, &mismatched, || {
                    panic!("mismatched authority must never execute")
                })
                .is_err()
        );
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(store.get(&identifier).is_err());
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let duplicate = store.try_clone().unwrap();
        assert_eq!(duplicate.get(&identifier).unwrap(), record);
        let record_path = root.join(format!("{identifier}.json"));
        std::fs::write(&record_path, b"{}").unwrap();
        assert!(store.get(&identifier).is_err());
        store.put(&record).unwrap();
        std::fs::set_permissions(&record_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.get(&identifier).is_err());
        store.put(&record).unwrap();
        std::thread::scope(|scope| {
            use std::sync::mpsc::{self, RecvTimeoutError};
            use std::time::Duration;
            let (entered, entry) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let active_store = &store;
            let active_identifier = &identifier;
            let active_record = &record;
            let active = scope.spawn(move || {
                active_store
                    .with_current(active_identifier, active_record, || {
                        entered.send(()).unwrap();
                        released.recv().unwrap();
                        Ok(())
                    })
                    .unwrap();
            });
            entry.recv().unwrap();
            let (removing, removal_started) = mpsc::channel();
            let (removed, removal_done) = mpsc::channel();
            let revocation_store = &duplicate;
            let revocation_identifier = &identifier;
            let revocation = scope.spawn(move || {
                removing.send(()).unwrap();
                revocation_store.remove(revocation_identifier).unwrap();
                removed.send(()).unwrap();
            });
            removal_started.recv().unwrap();
            let while_active = removal_done.recv_timeout(Duration::from_millis(25));
            release.send(()).unwrap();
            active.join().unwrap();
            revocation.join().unwrap();
            assert_eq!(while_active, Err(RecvTimeoutError::Timeout));
            removal_done.recv().unwrap();
        });
        assert!(duplicate.get(&identifier).is_err());
        assert!(
            store
                .with_current::<()>(&identifier, &record, || {
                    panic!("revoked authority must never execute")
                })
                .is_err()
        );
        store.put(&record).unwrap();
        let mut pinned = store.try_clone().unwrap();
        pinned.pin(&identifier, &record).unwrap();
        let pinned_clone = pinned.try_clone().unwrap();
        assert_eq!(pinned.get(&identifier).unwrap(), record);
        store.remove(&identifier).unwrap();
        store.put(&record).unwrap();
        assert!(pinned.get(&identifier).is_err());
        assert!(pinned_clone.get(&identifier).is_err());
        assert!(
            pinned
                .with_current(&identifier, &record, || Ok(()))
                .is_err()
        );
        let fresh = ApprovalStore::open(&root).unwrap();
        assert_eq!(fresh.get(&identifier).unwrap(), record);
        drop(fresh);
        drop(pinned_clone);
        drop(pinned);
        store.remove(&identifier).unwrap();
        symlink("/etc/passwd", &record_path).unwrap();
        assert!(store.get(&identifier).is_err());
        std::fs::remove_file(&record_path).unwrap();
        drop(duplicate);
        drop(store);
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn untrusted_paths_and_identifiers_are_refused_before_io() {
        assert!(ApprovalStore::open(Path::new("relative")).is_err());
        assert!(ApprovalStore::open(Path::new("/tmp")).is_err());
        assert!(ApprovalStore::open(Path::new("/var/../tmp")).is_err());
        for identifier in ["..", "../record", "", &"F".repeat(64), &"a".repeat(63)] {
            assert!(record_name(identifier).is_err());
        }
        assert!(record_name(&"a".repeat(64)).is_ok());
    }
}
