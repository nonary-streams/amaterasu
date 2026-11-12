//! Private executable images for Unix targets without executable sealed memfds.
//!
//! Directory ownership and mode exclude other users. Native code running as the
//! same UID remains trusted; advisory locks are not described as a sandbox.
pub(super) use super::unix::Process;
use super::*;
use sha2::{Digest, Sha256};
use std::{
    fs::{DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Component, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const MAX_IMAGES: usize = 16;
const MAX_DIRECTORY_ENTRIES: usize = 256;
const SLOT_PREFIX: &str = ".resolver-";
const IMAGE_PREFIX: &str = "helper-";
static SLOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) struct Executable {
    path: PathBuf,
    image: Option<File>,
    directory: File,
    directory_path: PathBuf,
    root: File,
    _ancestors: Vec<File>,
    _image_lock: File,
    #[cfg(test)]
    arguments: Vec<String>,
    #[cfg(test)]
    _test_root: Option<TestRoot>,
}

impl Executable {
    pub(super) fn new(bytes: &[u8], private_state_root: Option<&Path>) -> io::Result<Self> {
        #[cfg(test)]
        let test_root = private_state_root
            .is_none()
            .then(TestRoot::new)
            .transpose()?;
        #[cfg(test)]
        let private_state_root =
            private_state_root.or_else(|| test_root.as_ref().map(|root| root.path.as_path()));
        let (root_path, ancestors) = private_root(private_state_root)?;
        let root = open_directory(&root_path)?;
        let cache_lock = open_lock(&root_path.join(".cache.lock"), false)?;
        acquire_cache_lock(&cache_lock)?;
        reserve_image_slot(&root, &root_path)?;
        let directory_path = create_image_directory(&root_path)?;
        let directory = match open_directory(&directory_path) {
            Ok(directory) => directory,
            Err(error) => {
                let _ = unlink(
                    &root,
                    directory_path.file_name().ok_or_else(invalid_image)?,
                    true,
                );
                return Err(error);
            }
        };
        let image_lock =
            match open_lock(&directory_path.join(".image.lock"), true).and_then(|lock| {
                lock_exclusive(&lock)?;
                Ok(lock)
            }) {
                Ok(lock) => lock,
                Err(error) => {
                    let _ = unlink(&directory, std::ffi::OsStr::new(".image.lock"), false);
                    let _ = unlink(
                        &root,
                        directory_path.file_name().ok_or_else(invalid_image)?,
                        true,
                    );
                    return Err(error);
                }
            };
        let path = directory_path.join(image_name(bytes));
        let mut executable = Self {
            path,
            image: None,
            directory,
            directory_path,
            root,
            _ancestors: ancestors,
            _image_lock: image_lock,
            #[cfg(test)]
            arguments: Vec::new(),
            #[cfg(test)]
            _test_root: test_root,
        };
        let mut output = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&executable.path)?;
        output.write_all(bytes)?;
        output.sync_all()?;
        output.set_permissions(std::fs::Permissions::from_mode(0o500))?;
        let image = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&executable.path)?;
        let written = output.metadata()?;
        let reopened = image.metadata()?;
        validate_private_file(&reopened)?;
        if written.dev() != reopened.dev()
            || written.ino() != reopened.ino()
            || reopened.len() != bytes.len() as u64
        {
            return Err(invalid_image());
        }
        let mut stored = Vec::with_capacity(bytes.len());
        (&image)
            .take(bytes.len() as u64 + 1)
            .read_to_end(&mut stored)?;
        if stored != bytes {
            return Err(invalid_image());
        }
        executable.image = Some(image);
        drop(cache_lock);
        Ok(executable)
    }

    #[cfg(test)]
    pub(super) fn set_test_arguments(&mut self, arguments: &[&str]) {
        self.arguments = arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect();
    }

    pub(super) fn spawn(&self, request: &[u8]) -> io::Result<Process> {
        let expected = self.image.as_ref().ok_or_else(invalid_image)?.metadata()?;
        let named = std::fs::symlink_metadata(&self.path)?;
        validate_private_file(&named)?;
        if expected.dev() != named.dev()
            || expected.ino() != named.ino()
            || expected.len() != named.len()
        {
            return Err(invalid_image());
        }
        #[cfg(test)]
        let arguments = &self.arguments;
        #[cfg(not(test))]
        let arguments = &[];
        super::unix::spawn_with_fifos(&self.path, request, arguments, &self.directory)
    }
}

impl Drop for Executable {
    fn drop(&mut self) {
        // Remove only this newly created private slot through the held directory
        // handles. Unknown entries prevent directory removal and are preserved.
        drop(self.image.take());
        if let Some(name) = self.path.file_name() {
            let _ = unlink(&self.directory, name, false);
        }
        let _ = unlink(&self.directory, std::ffi::OsStr::new(".image.lock"), false);
        if let Some(name) = self.directory_path.file_name() {
            let _ = unlink(&self.root, name, true);
        }
    }
}

fn private_root(selected: Option<&Path>) -> io::Result<(PathBuf, Vec<File>)> {
    let requested = match selected {
        Some(path) => path.to_owned(),
        None => std::env::temp_dir()
            .canonicalize()?
            .join(format!("amaterasu-network-resolver-{}", current_uid())),
    };
    if !requested.is_absolute()
        || requested
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resolver state directory is not an absolute normalized path",
        ));
    }
    let mut absent = Vec::new();
    let mut existing = requested.as_path();
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(invalid_image());
                }
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                absent.push(existing.file_name().ok_or_else(invalid_image)?.to_owned());
                existing = existing.parent().ok_or_else(invalid_image)?;
            }
            Err(error) => return Err(error),
        }
    }
    let mut root = existing.canonicalize()?;
    let mut ancestors = validate_ancestors(&root)?;
    for component in absent.iter().rev() {
        root.push(component);
        match DirBuilder::new().mode(0o700).create(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let directory = open_directory(&root)?;
        validate_private_directory(&directory.metadata()?)?;
        ancestors.push(directory);
    }
    validate_private_directory(&open_directory(&root)?.metadata()?)?;
    Ok((root, ancestors))
}

fn validate_ancestors(path: &Path) -> io::Result<Vec<File>> {
    let mut current = PathBuf::new();
    let mut directories = Vec::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let directory = open_directory(&current)?;
        let metadata = directory.metadata()?;
        let trusted_owner = metadata.uid() == 0 || metadata.uid() == current_uid();
        let writable = metadata.mode() & 0o022 != 0;
        // MetadataExt exposes u32 modes even where libc::mode_t is u16.
        let sticky = metadata.mode() & 0o1000 != 0;
        if !trusted_owner || (writable && !sticky) {
            return Err(invalid_image());
        }
        directories.push(directory);
    }
    Ok(directories)
}

fn validate_private_directory(metadata: &std::fs::Metadata) -> io::Result<()> {
    if !metadata.is_dir() || metadata.uid() != current_uid() || metadata.mode() & 0o077 != 0 {
        return Err(invalid_image());
    }
    Ok(())
}

fn validate_private_file(metadata: &std::fs::Metadata) -> io::Result<()> {
    if !metadata.is_file() || metadata.uid() != current_uid() || metadata.mode() & 0o077 != 0 {
        return Err(invalid_image());
    }
    Ok(())
}

fn current_uid() -> u32 {
    // SAFETY: geteuid has no arguments or retained pointers.
    unsafe { libc::geteuid() }
}

fn open_directory(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
}

fn open_lock(path: &Path, exclusive_creation: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    if exclusive_creation {
        options.create_new(true);
    } else {
        options.create(true).truncate(false);
    }
    let file = options.open(path)?;
    validate_private_file(&file.metadata()?)?;
    Ok(file)
}

fn lock_exclusive(file: &File) -> io::Result<()> {
    // SAFETY: the owned lock descriptor remains live; LOCK_NB never waits.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn acquire_cache_lock(file: &File) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_millis(100);
    loop {
        match lock_exclusive(file) {
            Ok(()) => return Ok(()),
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(1))
            }
            Err(error) => return Err(error),
        }
    }
}

fn create_image_directory(root: &Path) -> io::Result<PathBuf> {
    for _ in 0..128 {
        let path = root.join(format!(
            "{SLOT_PREFIX}{}-{}",
            std::process::id(),
            SLOT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "resolver private image name budget exhausted",
    ))
}

fn image_name(bytes: &[u8]) -> String {
    let digest: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{IMAGE_PREFIX}{digest}")
}

fn valid_slot_name(name: &str) -> bool {
    name.strip_prefix(SLOT_PREFIX)
        .and_then(|name| name.split_once('-'))
        .is_some_and(|(process, sequence)| {
            !process.is_empty()
                && !sequence.is_empty()
                && process
                    .bytes()
                    .chain(sequence.bytes())
                    .all(|byte| byte.is_ascii_digit())
        })
}

fn image_digest(name: &str) -> Option<&str> {
    let digest = name.strip_prefix(IMAGE_PREFIX)?;
    (digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(digest)
}

fn reserve_image_slot(root: &File, path: &Path) -> io::Result<()> {
    let mut slots = Vec::new();
    for (index, entry) in std::fs::read_dir(path)?.enumerate() {
        if index == MAX_DIRECTORY_ENTRIES {
            return Err(io::Error::other(
                "resolver image cache exceeds its scan bound",
            ));
        }
        let entry = entry?;
        if entry.file_name().to_str().is_some_and(valid_slot_name) {
            slots.push(entry.path());
        }
    }
    slots.sort();
    let mut count = slots.len();
    for slot in slots {
        if count < MAX_IMAGES {
            return Ok(());
        }
        if prune_image_slot(root, &slot).unwrap_or(false) {
            count -= 1;
        }
    }
    if count >= MAX_IMAGES {
        return Err(io::Error::other(
            "resolver image cache is full of retained or unverifiable entries",
        ));
    }
    Ok(())
}

fn prune_image_slot(root: &File, path: &Path) -> io::Result<bool> {
    let directory = open_directory(path)?;
    validate_private_directory(&directory.metadata()?)?;
    // Inspection never creates a lock inside an unknown or incomplete slot.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path.join(".image.lock"))?;
    validate_private_file(&lock.metadata()?)?;
    lock_exclusive(&lock)?;
    let mut image_path = None;
    let mut fifo_residue = Vec::new();
    let mut entries = 0;
    for entry in std::fs::read_dir(path)? {
        entries += 1;
        if entries > 2 + super::unix::fifo::MAX_NAMES {
            return Ok(false);
        }
        let entry = entry?;
        if entry.file_name() == ".image.lock" {
            continue;
        }
        if super::unix::fifo::known_name(&entry.file_name()) {
            let identity = super::unix::fifo::inspect_name(&directory, &entry.file_name())?;
            fifo_residue.push((entry.file_name(), identity));
            continue;
        }
        if entry.file_name().to_str().and_then(image_digest).is_none() {
            return Ok(false);
        }
        if image_path.is_some() {
            return Ok(false);
        }
        image_path = Some(entry.path());
    }
    let Some(image_path) = image_path else {
        return Ok(false);
    };
    let mut image = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&image_path)?;
    let metadata = image.metadata()?;
    validate_private_file(&metadata)?;
    if metadata.len() == 0 || metadata.len() > MAX_HELPER_BYTES as u64 {
        return Ok(false);
    }
    let expected = image_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(image_digest)
        .ok_or_else(invalid_image)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        let length = image.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    let actual: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual != expected {
        return Ok(false);
    }
    let named = std::fs::symlink_metadata(path)?;
    let held = directory.metadata()?;
    if named.dev() != held.dev() || named.ino() != held.ino() {
        return Ok(false);
    }
    for (name, identity) in fifo_residue {
        super::unix::fifo::remove_name(&directory, &name, identity)?;
    }
    unlink(
        &directory,
        image_path.file_name().ok_or_else(invalid_image)?,
        false,
    )?;
    unlink(&directory, std::ffi::OsStr::new(".image.lock"), false)?;
    unlink(root, path.file_name().ok_or_else(invalid_image)?, true)?;
    Ok(true)
}

fn unlink(directory: &File, name: &std::ffi::OsStr, is_directory: bool) -> io::Result<()> {
    let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| invalid_image())?;
    // SAFETY: name is one terminated component; unlinkat uses this held owned
    // directory instead of resolving an ambient parent pathname again.
    if unsafe {
        libc::unlinkat(
            directory.as_raw_fd(),
            name.as_ptr(),
            if is_directory { libc::AT_REMOVEDIR } else { 0 },
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn invalid_image() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "resolver helper image or private directory is untrusted",
    )
}

#[cfg(test)]
pub(super) fn assert_private_image_guards(bytes: &[u8]) {
    let executable = Executable::new(bytes, None).unwrap();
    assert_eq!(
        executable.directory.metadata().unwrap().mode() & 0o777,
        0o700
    );
    assert_eq!(
        executable
            .image
            .as_ref()
            .unwrap()
            .metadata()
            .unwrap()
            .mode()
            & 0o777,
        0o500
    );
    let moved = executable.path.with_extension("original");
    std::fs::rename(&executable.path, &moved).unwrap();
    std::fs::write(&executable.path, bytes).unwrap();
    std::fs::set_permissions(&executable.path, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert!(executable.spawn(b"request").is_err());
    std::fs::remove_file(&executable.path).unwrap();
    std::fs::rename(moved, &executable.path).unwrap();
}

#[cfg(test)]
pub(super) fn run_private_image(bytes: &[u8], request: &[u8]) -> Vec<u8> {
    let executable = Executable::new(bytes, None).unwrap();
    let mut child = executable.spawn(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut response = Vec::new();
    let mut closed = false;
    let mut exited = None;
    while !closed || exited.is_none() {
        let mut buffer = [0; 1024];
        match child.try_read(&mut buffer) {
            Ok(0) => closed = true,
            Ok(length) => response.extend_from_slice(&buffer[..length]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("private helper output failed: {error}"),
        }
        exited = child.try_wait().unwrap();
        assert!(Instant::now() < deadline, "private helper failed to retire");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(exited.unwrap().success());
    response
}

#[cfg(test)]
pub(super) fn assert_atomic_fifo_lifetime(fixture: &[u8], request: &[u8]) {
    let mut executable = Executable::new(fixture, None).unwrap();
    executable.set_test_arguments(&["block"]);
    super::unix::fifo::assert_pair_flags_and_eof(&executable.directory);
    super::unix::fifo::assert_replaced_fifo_is_preserved(&executable.directory);
    let (mut read, write) = super::unix::fifo::pair(&executable.directory).unwrap();
    // Spawn an unrelated live helper while both FIFO ends exist. Their atomic
    // CLOEXEC flags must stop that child from retaining an accidental writer.
    let mut unrelated = executable.spawn(request).unwrap();
    assert!(unrelated.try_wait().unwrap().is_none());
    drop(write);
    assert_eq!(read.read(&mut [0; 1]).unwrap(), 0);
    assert!(unrelated.try_wait().unwrap().is_none());
    assert_eq!(
        std::fs::read_dir(&executable.directory_path)
            .unwrap()
            .count(),
        2
    );
    unrelated.kill().unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while unrelated.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "unrelated fixture did not retire"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(test)]
struct TestRoot {
    path: PathBuf,
}

#[cfg(test)]
impl TestRoot {
    fn new() -> io::Result<Self> {
        create_test_root().map(|path| Self { path })
    }
}

#[cfg(test)]
impl Drop for TestRoot {
    fn drop(&mut self) {
        // This guard drops after the image and its retained directory handles.
        // Never recurse: unknown entries keep their owned test root intact.
        let _ = std::fs::remove_file(self.path.join(".cache.lock"));
        let _ = std::fs::remove_dir(&self.path);
    }
}

#[cfg(test)]
fn create_test_root() -> io::Result<PathBuf> {
    let parent = std::env::temp_dir().canonicalize()?;
    for _ in 0..128 {
        let path = parent.join(format!(
            "amaterasu-private-image-test-{}-{}",
            std::process::id(),
            SLOT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "private test root name budget exhausted",
    ))
}

#[cfg(test)]
fn test_root() -> PathBuf {
    create_test_root().unwrap()
}

#[cfg(test)]
pub(super) fn assert_default_test_images_have_isolated_roots(bytes: &[u8]) {
    let first = Arc::new(Executable::new(bytes, None).unwrap());
    let first_root = first.directory_path.parent().unwrap().to_owned();
    let retained = first.clone();
    let mut roots = std::collections::BTreeSet::from([first_root.clone()]);
    // Keep only two images live at once so the test itself does not consume
    // macOS's descriptor allowance retaining many identical ancestor chains.
    for _ in 0..MAX_IMAGES + 4 {
        let executable = Arc::new(Executable::new(bytes, None).unwrap());
        let root = executable.directory_path.parent().unwrap().to_owned();
        assert!(
            roots.insert(root.clone()),
            "default tests shared an image cache"
        );
        assert_eq!(std::fs::metadata(&root).unwrap().mode() & 0o777, 0o700);
        drop(executable);
        assert!(
            !root.exists(),
            "final image release left its private root behind"
        );
    }
    drop(first);
    assert!(
        first_root.exists(),
        "a retained child image lost its private root"
    );
    drop(retained);
    assert!(
        !first_root.exists(),
        "last Arc release failed to clean its root"
    );
    assert!(roots.len() > MAX_IMAGES);
}

#[cfg(test)]
pub(super) fn assert_default_test_cleanup_preserves_unknown_entries(bytes: &[u8]) {
    let executable = Executable::new(bytes, None).unwrap();
    let root = executable.directory_path.parent().unwrap().to_owned();
    let unrelated = root.join("unrelated");
    std::fs::write(&unrelated, b"preserve unknown test data").unwrap();
    drop(executable);
    assert_eq!(
        std::fs::read(&unrelated).unwrap(),
        b"preserve unknown test data"
    );
    assert!(!root.join(".cache.lock").exists());
    std::fs::remove_file(unrelated).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[cfg(test)]
pub(super) fn assert_untrusted_roots_are_preserved(bytes: &[u8]) {
    use std::os::unix::fs::symlink;
    let root = test_root();
    let link = root.with_extension("link");
    symlink(&root, &link).unwrap();
    assert!(Executable::new(bytes, Some(&link)).is_err());
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(Executable::new(bytes, Some(&root)).is_err());
    assert_eq!(std::fs::metadata(&root).unwrap().mode() & 0o777, 0o777);
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::remove_file(link).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[cfg(test)]
pub(super) fn assert_private_image_capacity(bytes: &[u8]) {
    let root = test_root();
    let mut active = Vec::new();
    for _ in 0..MAX_IMAGES {
        active.push(Executable::new(bytes, Some(&root)).unwrap());
    }
    assert!(Executable::new(bytes, Some(&root)).is_err());
    active.pop();
    active.push(Executable::new(bytes, Some(&root)).unwrap());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), MAX_IMAGES + 1);
    drop(active);
    std::fs::remove_file(root.join(".cache.lock")).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[cfg(test)]
pub(super) fn assert_stale_image_cleanup_preserves_unknown_entries(bytes: &[u8]) {
    let root = test_root();
    let stale = root.join(".resolver-999999999-1");
    DirBuilder::new().mode(0o700).create(&stale).unwrap();
    let lock = open_lock(&stale.join(".image.lock"), true).unwrap();
    drop(lock);
    let mut image = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o500)
        .open(stale.join(image_name(bytes)))
        .unwrap();
    image.write_all(bytes).unwrap();
    drop(image);
    let directory = open_directory(&stale).unwrap();
    // SAFETY: fixed FIFO names are created only in this owned private test slot.
    assert_eq!(
        unsafe { libc::mkfifoat(directory.as_raw_fd(), c".ipc-0".as_ptr(), 0o600) },
        0
    );
    assert_eq!(
        unsafe { libc::mkfifoat(directory.as_raw_fd(), c"unrelated-fifo".as_ptr(), 0o600) },
        0
    );
    assert!(!prune_image_slot(&open_directory(&root).unwrap(), &stale).unwrap());
    assert!(
        stale.join(".ipc-0").exists(),
        "inspection removed a FIFO before validating all entries"
    );
    assert!(stale.join("unrelated-fifo").exists());
    unlink(&directory, std::ffi::OsStr::new("unrelated-fifo"), false).unwrap();
    let unrelated = root.join("unrelated");
    std::fs::write(&unrelated, b"preserve unrelated data").unwrap();
    let mut active = Vec::new();
    for _ in 0..MAX_IMAGES {
        active.push(Executable::new(bytes, Some(&root)).unwrap());
    }
    assert!(!stale.exists());
    assert_eq!(
        std::fs::read(&unrelated).unwrap(),
        b"preserve unrelated data"
    );
    drop(active);
    std::fs::remove_file(unrelated).unwrap();
    std::fs::remove_file(root.join(".cache.lock")).unwrap();
    std::fs::remove_dir(root).unwrap();
}
