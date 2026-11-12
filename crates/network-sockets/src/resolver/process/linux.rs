pub(super) use super::unix::Process;
use super::*;
use std::{
    fs::File,
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::PermissionsExt,
    },
};

pub(super) struct Executable {
    file: File,
    #[cfg(test)]
    arguments: Vec<String>,
}

impl Executable {
    pub(super) fn new(bytes: &[u8], _private_state_root: Option<&Path>) -> io::Result<Self> {
        let flags = libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING;
        // SAFETY: the static C string is terminated and flags are kernel values.
        let mut descriptor = unsafe {
            libc::memfd_create(
                c"amaterasu-network-resolver".as_ptr(),
                flags | libc::MFD_EXEC,
            )
        };
        if descriptor < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
            // Kernels predating MFD_EXEC reject that flag. Their default is
            // executable memory; permission/policy errors never get a fallback.
            descriptor =
                unsafe { libc::memfd_create(c"amaterasu-network-resolver".as_ptr(), flags) };
        }
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: memfd_create returned a new descriptor owned by this File.
        let mut file = unsafe { File::from_raw_fd(descriptor) };
        file.write_all(bytes)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o500))?;
        let seals =
            libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
        // SAFETY: the owned memfd permits seals and has no writable mappings.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            file,
            #[cfg(test)]
            arguments: Vec::new(),
        })
    }

    #[cfg(test)]
    pub(super) fn set_test_arguments(&mut self, arguments: &[&str]) {
        self.arguments = arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect();
    }

    pub(super) fn spawn(&self, request: &[u8]) -> io::Result<Process> {
        let path = std::path::PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()));
        #[cfg(test)]
        let arguments = &self.arguments;
        #[cfg(not(test))]
        let arguments = &[];
        super::unix::spawn(&path, request, arguments)
    }
}

#[cfg(test)]
pub(super) fn assert_sealed(executable: &Executable, bytes: &[u8]) {
    use std::os::unix::fs::FileExt;
    let mut stored = vec![0; bytes.len()];
    executable.file.read_exact_at(&mut stored, 0).unwrap();
    assert_eq!(stored, bytes);
    assert!(executable.file.write_at(b"substitute", 0).is_err());
    assert!(executable.file.set_len(0).is_err());
    let seals = unsafe { libc::fcntl(executable.file.as_raw_fd(), libc::F_GET_SEALS) };
    assert_ne!(seals & libc::F_SEAL_SEAL, 0);
    assert_ne!(seals & libc::F_SEAL_WRITE, 0);
}
