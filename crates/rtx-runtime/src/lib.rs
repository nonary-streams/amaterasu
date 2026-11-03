//! Verified staging of the NVIDIA TrueHDR runtime. Networking is supplied by
//! an injected HTTP agent, which production must construct with the Network SDK.
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub mod remote;
mod zip_guard;
pub use remote::RemoteWheel;
pub const WHEEL_URL: &str = "https://pypi.nvidia.com/nvidia-vfx/nvidia_vfx-0.2.0.0-cp312-abi3-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl";
pub const WHEEL_SHA256: &str = "0252866a7b798a6f92c8543443d88f716e8c5bbc6a7ddd77a2754728dada9fa8";
pub const DIRECTORY: &str = "runtime";

struct Entry {
    name: &'static str,
    sha256: &'static str,
    size: u64,
}
// Digests are independently pinned from the SHA-256-verified NVIDIA VFX 0.2.0.0 wheel.
const ENTRIES: &[Entry] = &[
    Entry {
        name: "nvidia_vfx-0.2.0.0.dist-info/licenses/packaging/NVIDIA-Open-Model-License-Agreements-24-10-2025.pdf",
        sha256: "4d2fb590aa9b30c47f2058bff17291df7fa2aa0c1bd775a20703da9bb267cfab",
        size: 79005,
    },
    Entry {
        name: "nvidia_vfx-0.2.0.0.dist-info/licenses/packaging/NVIDIA-Software-License-Agreement-2025.05.05.pdf",
        sha256: "887d7381eed7cd2e6d9fbfa9c9bbda4e32df20bf506cff79dc5c8dae20ccc531",
        size: 186823,
    },
    Entry {
        name: "nvidia_vfx-0.2.0.0.dist-info/licenses/packaging/THIRD_PARTY.md",
        sha256: "2a19287af2bfb99b5017a09ab67f6fb084e591013b6d50ef89b847904eddd41e",
        size: 13210,
    },
    Entry {
        name: "nvidia_vfx-0.2.0.0.dist-info/licenses/packaging/ThirdPartyLicenses_VFX_SDK.txt",
        sha256: "207b323c79c8d73a1570378597b77dd6ce15d05d0391d9c4d1aef628490bbe70",
        size: 1383377,
    },
    Entry {
        name: "nvidia_vfx-0.2.0.0.dist-info/licenses/packaging/product-specific-terms-for-nvidia-ai-products-2025.05.05.pdf",
        sha256: "5c755ab1145deb6ce422c2d376f4c3adb55da9f6f2492f35e6d4759e7e3e5499",
        size: 66627,
    },
    Entry {
        name: "nvidia_vfx.libs/libcudart-256e6409.so.12.9.79",
        sha256: "f7d7885be683ed3cccb2571db2a83af2d9cd8e3e0dcf3841b3a37c489e8ee2df",
        size: 2115321,
    },
    Entry {
        name: "nvvfx/libs/libNVCVImage.so",
        sha256: "3ead060c31084e137eab1868535a6a8ec4135e00611d54bd7a96b3d2e17e95bb",
        size: 4943832,
    },
    Entry {
        name: "nvvfx/libs/libVideoFX.so",
        sha256: "19b111cbb98d1051f8cb4ec10ba36c44d6226d3a9565897bb752d28e8514e382",
        size: 4198857,
    },
    Entry {
        name: "nvvfx/libs/libVideoFXLocal.so",
        sha256: "ce0933ae473ba77bb906827f4ac6f2a7157fa73f1451000ef00718d085148fdd",
        size: 4206473,
    },
    Entry {
        name: "nvvfx/libs/libcudnn.so.9",
        sha256: "947352ab457779068ab09bb36712cbe61329821171bb3a80f6a6abbc6f4382dc",
        size: 125136,
    },
    Entry {
        name: "nvvfx/libs/libnppc.so.12",
        sha256: "69c1468de02b2951a3c9755a76b8246b83fbf4d8f137fd1e843767a76c344ae7",
        size: 1656080,
    },
    Entry {
        name: "nvvfx/libs/libnppial.so.12",
        sha256: "d37c9d285930dca5da32ccce15594bccdadde6da71fd1c297f79d7b435b50ce6",
        size: 22046288,
    },
    Entry {
        name: "nvvfx/libs/libnppicc.so.12",
        sha256: "bc1f7c1797fda52d0b333f91d65af2add1deeaa0e4cf5b5ae8a58e1d54117fe2",
        size: 9373288,
    },
    Entry {
        name: "nvvfx/libs/libnppidei.so.12",
        sha256: "8397ce991612229cf673dce3b594187c61ada782d5cf61f4a7212cdd84e1e552",
        size: 13633464,
    },
    Entry {
        name: "nvvfx/libs/libnppig.so.12",
        sha256: "f24d72d82ceea1b0833a2429cebd6903f0d9ca961841ee413cf6bdea7d0d1129",
        size: 55871152,
    },
    Entry {
        name: "nvvfx/libs/libnvVFXTrueHDR.so",
        sha256: "7851e321ba2c31f89314b872cd40b1075122832c1d2d57cc901a36222dbd20a6",
        size: 4203601,
    },
    Entry {
        name: "nvvfx/libs/libnvidia-ngx-truehdr.so.1.2.3",
        sha256: "2ec41bc1a5664c44f2a8a97ab59e60890ab3becda143b753337d12588c993688",
        size: 3845656,
    },
    Entry {
        name: "nvvfx/libs/libnvngxruntime.so",
        sha256: "4279a4595247e4bbc0b67cc6bc243c66875bf5797741be4e12b0c6f4d844b3fc",
        size: 79776,
    },
];
fn bad(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn cancelled(flag: &AtomicBool) -> io::Result<()> {
    if flag.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "runtime installation cancelled",
        ))
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn require_private_parent(parent: &Path) -> io::Result<()> {
    let md = fs::symlink_metadata(parent)?;
    // SAFETY: geteuid has no preconditions.
    if !md.is_dir()
        || md.file_type().is_symlink()
        || md.uid() != unsafe { libc::geteuid() }
        || md.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "runtime parent must be a private service-owned directory",
        ));
    }
    Ok(())
}

/// Verify all files of an already installed runtime, including the exact
/// inventory, before reporting readiness. This reads approximately 128 MiB;
/// callers should perform it on a worker and cache the result until invalidated.
#[cfg(target_os = "linux")]
pub fn verify(parent: &Path, cancel: &AtomicBool) -> io::Result<PathBuf> {
    verify_entries(parent, cancel, ENTRIES)
}

#[cfg(target_os = "linux")]
fn verify_entries(parent: &Path, cancel: &AtomicBool, entries: &[Entry]) -> io::Result<PathBuf> {
    require_private_parent(parent)?;
    let root = parent.join(DIRECTORY);
    let mut pending = vec![root.clone()];
    let mut files = HashSet::new();
    while let Some(dir) = pending.pop() {
        cancelled(cancel)?;
        let md = fs::symlink_metadata(&dir)?;
        // SAFETY: geteuid has no preconditions.
        if !md.is_dir()
            || md.file_type().is_symlink()
            || md.uid() != unsafe { libc::geteuid() }
            || md.mode() & 0o022 != 0
        {
            return Err(bad("runtime directory is not protected"));
        }
        for item in fs::read_dir(dir)? {
            cancelled(cancel)?;
            let item = item?;
            let path = item.path();
            let md = fs::symlink_metadata(&path)?;
            // SAFETY: geteuid has no preconditions.
            if md.file_type().is_symlink()
                || md.uid() != unsafe { libc::geteuid() }
                || md.mode() & 0o022 != 0
            {
                return Err(bad("runtime entry is not protected"));
            }
            if md.is_dir() {
                let relative = path.strip_prefix(&root).map_err(io::Error::other)?;
                if !entries
                    .iter()
                    .any(|entry| Path::new(entry.name).starts_with(relative))
                {
                    return Err(bad("unexpected runtime directory"));
                }
                pending.push(path);
                continue;
            }
            if !md.is_file() {
                return Err(bad("unexpected runtime entry"));
            }
            let relative = path.strip_prefix(&root).map_err(io::Error::other)?;
            let entry = entries
                .iter()
                .find(|entry| Path::new(entry.name) == relative)
                .ok_or_else(|| bad("unexpected runtime file"))?;
            if md.len() != entry.size {
                return Err(bad("runtime file size mismatch"));
            }
            let mut file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)?;
            let mut digest = Sha256::new();
            let mut size = 0_u64;
            let mut buffer = [0_u8; 65536];
            loop {
                cancelled(cancel)?;
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                size = size
                    .checked_add(n as u64)
                    .ok_or_else(|| bad("runtime file too large"))?;
                if size > entry.size {
                    return Err(bad("runtime file too large"));
                }
                digest.update(&buffer[..n]);
            }
            if size != entry.size || format!("{:x}", digest.finalize()) != entry.sha256 {
                return Err(bad("runtime integrity verification failed"));
            }
            files.insert(entry.name);
        }
    }
    if files.len() != entries.len() {
        return Err(bad("runtime file missing"));
    }
    Ok(root)
}

/// Extract exactly the pinned TrueHDR files and licenses from the supplied wheel.
/// `parent` must be an existing private directory controlled by this service
/// account. Existing installations are never overwritten. The caller owns the
/// network worker and must join it on cancellation/shutdown.
#[cfg(target_os = "linux")]
pub fn install<R: Read + Seek>(
    reader: R,
    parent: &Path,
    cancel: &AtomicBool,
) -> io::Result<PathBuf> {
    install_entries(reader, parent, cancel, ENTRIES)
}

#[cfg(target_os = "linux")]
fn install_entries<R: Read + Seek>(
    reader: R,
    parent: &Path,
    cancel: &AtomicBool,
    entries: &[Entry],
) -> io::Result<PathBuf> {
    cancelled(cancel)?;
    require_private_parent(parent)?;
    let mut archive = zip_guard::open(reader)?;
    if archive.len() > 4096 {
        return Err(bad("wheel has too many entries"));
    }
    let mut indexes = Vec::with_capacity(entries.len());
    // Look up pinned names in the central directory without visiting unrelated
    // local records; those can contain hundreds of megabytes of other effects.
    for entry in entries {
        cancelled(cancel)?;
        let index = archive
            .index_for_name(entry.name)
            .ok_or_else(|| bad("wheel is missing required files"))?;
        let item = archive.by_index(index).map_err(io::Error::other)?;
        if item.is_dir()
            || item.size() != entry.size
            || item.enclosed_name().as_deref() != Some(Path::new(entry.name))
            || item
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 != 0 && mode & 0o170000 != 0o100000)
        {
            return Err(bad("invalid pinned wheel entry"));
        }
        indexes.push((index, entry));
    }
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
    let stage = parent.join(format!(
        ".rtx-hdr-stage-{}",
        random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ));
    fs::DirBuilder::new().mode(0o700).create(&stage)?;
    let result = (|| {
        for (index, entry) in indexes {
            cancelled(cancel)?;
            let path = stage.join(entry.name);
            if let Some(dir) = path.parent() {
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(dir)?;
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            let mut input = archive.by_index(index).map_err(io::Error::other)?;
            let mut digest = Sha256::new();
            let mut size = 0_u64;
            let mut buffer = [0_u8; 65536];
            loop {
                cancelled(cancel)?;
                let n = input.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                size = size
                    .checked_add(n as u64)
                    .ok_or_else(|| bad("wheel entry is too large"))?;
                if size > entry.size {
                    return Err(bad("wheel entry is too large"));
                }
                digest.update(&buffer[..n]);
                output.write_all(&buffer[..n])?;
            }
            if size != entry.size || format!("{:x}", digest.finalize()) != entry.sha256 {
                return Err(bad("wheel entry failed integrity verification"));
            }
            output.sync_all()?;
        }
        cancelled(cancel)?;
        // renameat2 NOREPLACE makes publication atomic and prevents replacing
        // an installation another worker or an administrator already uses.
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let from =
            CString::new(stage.as_os_str().as_bytes()).map_err(|_| bad("invalid staging path"))?;
        let destination = parent.join(DIRECTORY);
        let to = CString::new(destination.as_os_str().as_bytes())
            .map_err(|_| bad("invalid runtime path"))?;
        // SAFETY: both C strings remain alive; flags request no replacement.
        if unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        File::open(parent)?.sync_all()?;
        Ok(destination)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;
    fn root() -> PathBuf {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).unwrap();
        let path = std::env::temp_dir().join(format!(
            "rtx-runtime-{}",
            random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        path
    }
    fn wheel(items: &[(&str, &[u8])]) -> Cursor<Vec<u8>> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in items {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        let mut result = zip.finish().unwrap();
        result.set_position(0);
        result
    }
    const FIXTURE: &[Entry] = &[Entry {
        name: "nvvfx/libs/test.so",
        size: 3,
        sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    }];
    #[test]
    fn verifies_contents_and_only_publishes_selected_files() {
        let dir = root();
        let cancel = AtomicBool::new(false);
        let installed = install_entries(
            wheel(&[
                ("nvvfx/libs/test.so", b"abc"),
                ("nvvfx/libs/unrelated.so", b"evil"),
            ]),
            &dir,
            &cancel,
            FIXTURE,
        )
        .unwrap();
        assert_eq!(
            fs::read(installed.join("nvvfx/libs/test.so")).unwrap(),
            b"abc"
        );
        assert!(!installed.join("nvvfx/libs/unrelated.so").exists());
        verify_entries(&dir, &cancel, FIXTURE).unwrap();
        fs::write(installed.join("nvvfx/libs/unrelated.so"), b"evil").unwrap();
        assert!(verify_entries(&dir, &cancel, FIXTURE).is_err());
        assert!(
            install_entries(
                wheel(&[("nvvfx/libs/test.so", b"abc")]),
                &dir,
                &cancel,
                FIXTURE
            )
            .is_err()
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn rejects_modified_missing_entries_and_cancellation() {
        let dir = root();
        let cancel = AtomicBool::new(false);
        for input in [wheel(&[("nvvfx/libs/test.so", b"bad")]), wheel(&[])] {
            assert!(install_entries(input, &dir, &cancel, FIXTURE).is_err());
            assert!(!dir.join(DIRECTORY).exists());
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        }
        cancel.store(true, Ordering::Release);
        assert_eq!(
            install_entries(
                wheel(&[("nvvfx/libs/test.so", b"abc")]),
                &dir,
                &cancel,
                FIXTURE
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
