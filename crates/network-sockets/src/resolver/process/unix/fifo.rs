//! Atomically close-on-exec FIFO descriptors inside an owned private image slot.
use std::{
    ffi::{CStr, CString, OsStr},
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) const MAX_NAMES: usize = 32;
const PREFIX: &str = ".ipc-";
static NEXT_NAME: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
pub(crate) struct Identity {
    device: libc::dev_t,
    inode: libc::ino_t,
}

pub(crate) fn known_name(name: &OsStr) -> bool {
    name.to_str()
        .and_then(|name| name.strip_prefix(PREFIX))
        .is_some_and(|number| {
            number
                .parse::<usize>()
                .ok()
                .is_some_and(|index| index < MAX_NAMES && index.to_string() == number)
        })
}

pub(crate) fn pair(directory: &File) -> io::Result<(File, File)> {
    for _ in 0..MAX_NAMES {
        let index = NEXT_NAME.fetch_add(1, Ordering::Relaxed) % MAX_NAMES as u64;
        let name = CString::new(format!("{PREFIX}{index}")).expect("fixed ASCII FIFO name");
        match pair_at_name(directory, &name) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            result => return result,
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "resolver FIFO namespace is full of retained entries",
    ))
}

fn pair_at_name(directory: &File, name: &CStr) -> io::Result<(File, File)> {
    // SAFETY: this is one terminated component beneath a held private directory;
    // mkfifoat never replaces an existing entry, regardless of its type.
    if unsafe { libc::mkfifoat(directory.as_raw_fd(), name.as_ptr(), 0o600) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let identity = inspect(directory, name)?;
    open_created_pair(directory, name, identity)
}

fn open_created_pair(
    directory: &File,
    name: &CStr,
    identity: Identity,
) -> io::Result<(File, File)> {
    let mut created = CreatedName {
        directory,
        name,
        identity,
        armed: true,
    };
    let read = open_end(directory, name, libc::O_RDONLY)?;
    let write = open_end(directory, name, libc::O_WRONLY)?;
    verify_end(&read, identity)?;
    verify_end(&write, identity)?;
    created.remove()?;
    Ok((read, write))
}

fn open_end(directory: &File, name: &CStr, direction: libc::c_int) -> io::Result<File> {
    // SAFETY: openat atomically creates a noninheritable, nonblocking descriptor;
    // O_NOFOLLOW refuses a substituted symlink before any FIFO wait can occur.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            direction | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned this new uniquely owned descriptor.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

pub(crate) fn inspect_name(directory: &File, name: &OsStr) -> io::Result<Identity> {
    if !known_name(name) {
        return Err(invalid_fifo());
    }
    inspect(
        directory,
        &CString::new(name.as_bytes()).map_err(|_| invalid_fifo())?,
    )
}

fn inspect(directory: &File, name: &CStr) -> io::Result<Identity> {
    // SAFETY: stat storage is writable; no link is followed during inspection.
    let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            &mut metadata,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    validate(&metadata)
}

fn verify_end(file: &File, expected: Identity) -> io::Result<()> {
    // SAFETY: this owned descriptor remains live and stat storage is writable.
    let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(file.as_raw_fd(), &mut metadata) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let actual = validate(&metadata)?;
    if actual.device != expected.device || actual.inode != expected.inode {
        return Err(invalid_fifo());
    }
    Ok(())
}

fn validate(metadata: &libc::stat) -> io::Result<Identity> {
    // SAFETY: geteuid takes no arguments and borrows no memory.
    let current_user = unsafe { libc::geteuid() };
    if metadata.st_mode & libc::S_IFMT != libc::S_IFIFO
        || metadata.st_uid != current_user
        || metadata.st_mode & 0o077 != 0
    {
        return Err(invalid_fifo());
    }
    Ok(Identity {
        device: metadata.st_dev,
        inode: metadata.st_ino,
    })
}

pub(crate) fn remove_name(directory: &File, name: &OsStr, expected: Identity) -> io::Result<()> {
    if !known_name(name) {
        return Err(invalid_fifo());
    }
    remove(
        directory,
        &CString::new(name.as_bytes()).map_err(|_| invalid_fifo())?,
        expected,
    )
}

fn remove(directory: &File, name: &CStr, expected: Identity) -> io::Result<()> {
    let actual = inspect(directory, name)?;
    if actual.device != expected.device || actual.inode != expected.inode {
        return Err(invalid_fifo());
    }
    // SAFETY: the held directory and unchanged verified FIFO identity still own
    // this exact name. Unlinking releases only the name, not open pipe endpoints.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

struct CreatedName<'a> {
    directory: &'a File,
    name: &'a CStr,
    identity: Identity,
    armed: bool,
}

impl CreatedName<'_> {
    fn remove(&mut self) -> io::Result<()> {
        remove(self.directory, self.name, self.identity)?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for CreatedName<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.remove();
        }
    }
}

fn invalid_fifo() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "resolver FIFO identity, type, or owner changed",
    )
}

#[cfg(test)]
pub(crate) fn assert_pair_flags_and_eof(directory: &File) {
    use std::io::Read;
    let (mut read, write) = pair(directory).unwrap();
    for end in [&read, &write] {
        // SAFETY: both owned FIFO descriptors remain open during flag inspection.
        assert_ne!(
            unsafe { libc::fcntl(end.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        assert_ne!(
            unsafe { libc::fcntl(end.as_raw_fd(), libc::F_GETFL) } & libc::O_NONBLOCK,
            0
        );
    }
    drop(write);
    assert_eq!(read.read(&mut [0; 1]).unwrap(), 0);
}

#[cfg(test)]
pub(crate) fn assert_replaced_fifo_is_preserved(directory: &File) {
    let name = c".ipc-0";
    // SAFETY: these fixed names are created only beneath this private test slot.
    assert_eq!(
        unsafe { libc::mkfifoat(directory.as_raw_fd(), name.as_ptr(), 0o600) },
        0
    );
    let original = inspect(directory, name).unwrap();
    let _original_end = open_end(directory, name, libc::O_RDONLY).unwrap();
    assert_eq!(
        unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) },
        0
    );
    assert_eq!(
        unsafe { libc::mkfifoat(directory.as_raw_fd(), name.as_ptr(), 0o600) },
        0
    );
    let replacement = inspect(directory, name).unwrap();
    assert!(open_created_pair(directory, name, original).is_err());
    let retained = inspect(directory, name).unwrap();
    assert_eq!(retained.device, replacement.device);
    assert_eq!(retained.inode, replacement.inode);
    remove(directory, name, replacement).unwrap();
}
