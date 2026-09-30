//! Compare-before-write transactions for existing RTSS profile files.
use crate::lease::{ProfileValues, ProviderError};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::Range;
#[cfg(all(windows, target_pointer_width = "64"))]
use std::path::Path;

const MAX_PROFILE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone)]
struct ProfileValueSpan {
    range: Range<usize>,
    value: i32,
}

fn profile_value_spans(content: &[u8]) -> Result<[ProfileValueSpan; 3], ProviderError> {
    let mut spans: [Option<ProfileValueSpan>; 3] = [None, None, None];
    let mut framerate_section_seen = false;
    let mut in_framerate_section = false;
    let mut line_start = 0;

    while line_start < content.len() {
        let line_end = content[line_start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(content.len(), |offset| line_start + offset);
        let content_end = if line_end > line_start && content[line_end - 1] == b'\r' {
            line_end - 1
        } else {
            line_end
        };
        let mut visible_start = line_start;
        if line_start == 0 && content.starts_with(b"\xEF\xBB\xBF") {
            visible_start += 3;
        }
        let raw_line = &content[visible_start..content_end];
        let line_leading_space = raw_line
            .iter()
            .take_while(|byte| byte.is_ascii_whitespace())
            .count();
        let line_start_in_content = visible_start + line_leading_space;
        let line = trim_ascii(raw_line);

        if line.first() == Some(&b'[') && line.last() == Some(&b']') {
            let section = trim_ascii(&line[1..line.len() - 1]);
            in_framerate_section = section.eq_ignore_ascii_case(b"Framerate");
            if in_framerate_section {
                if framerate_section_seen {
                    return Err(profile_format_error());
                }
                framerate_section_seen = true;
            }
        } else if in_framerate_section
            && !line.is_empty()
            && !line.starts_with(b";")
            && !line.starts_with(b"#")
        {
            let Some(equals_offset) = line.iter().position(|byte| *byte == b'=') else {
                line_start = next_line_start(line_end, content.len());
                continue;
            };
            let key = trim_ascii(&line[..equals_offset]);
            let Some(index) = profile_key_index(key) else {
                line_start = next_line_start(line_end, content.len());
                continue;
            };
            if spans[index].is_some() {
                return Err(profile_format_error());
            }

            let value_start = line_start_in_content + equals_offset + 1;
            let raw_value = &content[value_start..content_end];
            let leading_space = raw_value
                .iter()
                .take_while(|byte| byte.is_ascii_whitespace())
                .count();
            let trimmed_value = trim_ascii(raw_value);
            let value_end = value_start + leading_space + trimmed_value.len();
            let value = std::str::from_utf8(trimmed_value)
                .ok()
                .and_then(|text| text.parse::<i32>().ok())
                .ok_or_else(profile_format_error)?;
            spans[index] = Some(ProfileValueSpan {
                range: value_start + leading_space..value_end,
                value,
            });
        }

        line_start = next_line_start(line_end, content.len());
    }

    if !framerate_section_seen {
        return Err(profile_format_error());
    }

    let Some(frame_limit) = spans[0].clone() else {
        return Err(profile_format_error());
    };
    let Some(frame_limit_denominator) = spans[1].clone() else {
        return Err(profile_format_error());
    };
    let Some(sync_limiter) = spans[2].clone() else {
        return Err(profile_format_error());
    };
    Ok([frame_limit, frame_limit_denominator, sync_limiter])
}

fn trim_ascii(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(u8::is_ascii_whitespace) {
        value = &value[1..];
    }
    while value.last().is_some_and(u8::is_ascii_whitespace) {
        value = &value[..value.len() - 1];
    }
    value
}

fn next_line_start(line_end: usize, content_len: usize) -> usize {
    if line_end < content_len {
        line_end + 1
    } else {
        content_len
    }
}

fn profile_key_index(key: &[u8]) -> Option<usize> {
    if key.eq_ignore_ascii_case(b"Limit") {
        Some(0)
    } else if key.eq_ignore_ascii_case(b"LimitDenominator") {
        Some(1)
    } else if key.eq_ignore_ascii_case(b"SyncLimiter") {
        Some(2)
    } else {
        None
    }
}

fn profile_format_error() -> ProviderError {
    ProviderError::ReadFailed("rtss_profile_values_unreadable")
}

fn profile_values(spans: &[ProfileValueSpan; 3]) -> ProfileValues {
    ProfileValues {
        frame_limit: spans[0].value,
        frame_limit_denominator: spans[1].value,
        sync_limiter: spans[2].value,
    }
}

fn profile_targets(values: ProfileValues) -> [i32; 3] {
    [
        values.frame_limit,
        values.frame_limit_denominator,
        values.sync_limiter,
    ]
}

fn replace_profile_values(
    content: &[u8],
    expected: ProfileValues,
    values: ProfileValues,
) -> Result<Vec<u8>, ProviderError> {
    let spans = profile_value_spans(content)?;
    let observed = profile_values(&spans);
    if observed != expected {
        return Err(ProviderError::ProfileMismatch {
            code: "rtss_profile_changed_before_write",
            observed,
        });
    }

    let mut replacements = Vec::with_capacity(spans.len());
    for (span, value) in spans.iter().zip(profile_targets(values)) {
        replacements.push((span.range.clone(), value.to_string().into_bytes()));
    }
    replacements.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));

    let mut updated = content.to_vec();
    for (range, value) in replacements {
        updated.splice(range, value);
    }
    if updated.len() as u64 > MAX_PROFILE_BYTES {
        return Err(ProviderError::ReadFailed("rtss_profile_too_large"));
    }
    Ok(updated)
}

fn read_profile_file(file: &mut File) -> Result<Vec<u8>, ProviderError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|_| ProviderError::ReadFailed("rtss_profile_read_failed"))?;
    let mut content = Vec::new();
    file.take(MAX_PROFILE_BYTES + 1)
        .read_to_end(&mut content)
        .map_err(|_| ProviderError::ReadFailed("rtss_profile_read_failed"))?;
    if content.len() as u64 > MAX_PROFILE_BYTES {
        return Err(ProviderError::ReadFailed("rtss_profile_too_large"));
    }
    Ok(content)
}

fn write_profile_file(file: &mut File, content: &[u8]) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.write_all(content)?;
    file.set_len(content.len() as u64)?;
    file.flush()?;
    file.sync_all()
}

fn restore_profile_file(file: &mut File, original: &[u8]) -> std::io::Result<()> {
    write_profile_file(file, original)?;
    file.seek(SeekFrom::Start(0))?;
    let mut restored = Vec::with_capacity(original.len());
    file.read_to_end(&mut restored)?;
    if restored == original {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "RTSS profile rollback did not verify",
        ))
    }
}

fn update_locked_profile_file(
    file: &mut File,
    expected: ProfileValues,
    values: ProfileValues,
) -> Result<(), ProviderError> {
    let original = read_profile_file(file)?;
    let updated = replace_profile_values(&original, expected, values)?;
    if updated == original {
        return Ok(());
    }

    if write_profile_file(file, &updated).is_err() {
        if restore_profile_file(file, &original).is_err() {
            return Err(ProviderError::VerifyFailed(
                "rtss_profile_write_rollback_failed",
            ));
        }
        return Err(ProviderError::Unavailable("rtss_profile_write_failed"));
    }

    let written = match read_profile_file(file) {
        Ok(written) => written,
        Err(error) => {
            if restore_profile_file(file, &original).is_err() {
                return Err(ProviderError::VerifyFailed(
                    "rtss_profile_write_rollback_failed",
                ));
            }
            return Err(error);
        }
    };
    if written == updated {
        Ok(())
    } else if restore_profile_file(file, &original).is_err() {
        Err(ProviderError::VerifyFailed(
            "rtss_profile_write_rollback_failed",
        ))
    } else {
        Err(ProviderError::Unavailable("rtss_profile_write_failed"))
    }
}

#[cfg(all(windows, target_pointer_width = "64"))]
pub(crate) fn write_profile_file_verified(
    path: &Path,
    expected: ProfileValues,
    values: ProfileValues,
) -> Result<(), ProviderError> {
    let mut profile = LockedProfileFile::open(path)?;
    update_locked_profile_file(&mut profile.file, expected, values)
}

#[cfg(all(windows, target_pointer_width = "64"))]
struct LockedProfileFile {
    file: File,
    lock: Overlapped,
}

#[cfg(all(windows, target_pointer_width = "64"))]
impl LockedProfileFile {
    fn open(path: &Path) -> Result<Self, ProviderError> {
        use std::os::windows::{ffi::OsStrExt, io::FromRawHandle};

        let path: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // Keep read/write sharing so this can coordinate with an already-open
        // RTSS profile handle. Omitting FILE_SHARE_DELETE prevents replacement
        // while the byte-range lock protects ordinary read/write I/O.
        // SAFETY: `path` is NUL-terminated UTF-16 and the remaining pointers are
        // either null optional attributes or a null template handle, as allowed
        // for OPEN_EXISTING. A successful handle is owned below by `File`.
        let raw_file = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if raw_file == INVALID_HANDLE_VALUE {
            return Err(ProviderError::Unavailable("rtss_profile_lock_unavailable"));
        }

        // SAFETY: CreateFileW returned a valid owned handle, now transferred to
        // File so every early return closes it.
        let file = unsafe { File::from_raw_handle(raw_file) };
        let metadata = file
            .metadata()
            .map_err(|_| ProviderError::ReadFailed("rtss_profile_path_invalid"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(ProviderError::ReadFailed("rtss_profile_path_invalid"));
        }

        let mut lock = Overlapped::default();
        // Lock the full addressable byte range, including future extension.
        // SAFETY: `file` is a live handle for this regular disk file and `lock`
        // is a stable, zero-initialized OVERLAPPED record owned until unlock.
        // The nonblocking flag prevents a pending asynchronous lock operation.
        let locked = unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                u32::MAX,
                u32::MAX,
                &mut lock,
            )
        };
        if locked == 0 {
            return Err(ProviderError::Unavailable("rtss_profile_lock_unavailable"));
        }
        Ok(Self { file, lock })
    }
}

#[cfg(all(windows, target_pointer_width = "64"))]
impl Drop for LockedProfileFile {
    fn drop(&mut self) {
        use std::os::windows::io::AsRawHandle;

        // SAFETY: the lock and its OVERLAPPED offset belong to this still-open
        // file handle and are released before File closes the handle.
        unsafe {
            UnlockFileEx(
                self.file.as_raw_handle(),
                0,
                u32::MAX,
                u32::MAX,
                &mut self.lock,
            );
        }
    }
}

#[cfg(all(windows, target_pointer_width = "64"))]
#[repr(C)]
#[derive(Default)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: *mut std::ffi::c_void,
}

#[cfg(all(windows, target_pointer_width = "64"))]
use std::os::windows::io::AsRawHandle;

#[cfg(all(windows, target_pointer_width = "64"))]
const GENERIC_READ: u32 = 0x8000_0000;
#[cfg(all(windows, target_pointer_width = "64"))]
const GENERIC_WRITE: u32 = 0x4000_0000;
#[cfg(all(windows, target_pointer_width = "64"))]
const FILE_SHARE_READ: u32 = 0x0000_0001;
#[cfg(all(windows, target_pointer_width = "64"))]
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
#[cfg(all(windows, target_pointer_width = "64", test))]
const FILE_SHARE_DELETE: u32 = 0x0000_0004;
#[cfg(all(windows, target_pointer_width = "64"))]
const OPEN_EXISTING: u32 = 3;
#[cfg(all(windows, target_pointer_width = "64"))]
const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
#[cfg(all(windows, target_pointer_width = "64"))]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
#[cfg(all(windows, target_pointer_width = "64"))]
const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;
#[cfg(all(windows, target_pointer_width = "64"))]
const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;
#[cfg(all(windows, target_pointer_width = "64"))]
const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1_isize as *mut std::ffi::c_void;

#[cfg(all(windows, target_pointer_width = "64"))]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *mut std::ffi::c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn LockFileEx(
        file: *mut std::ffi::c_void,
        flags: u32,
        reserved: u32,
        bytes_to_lock_low: u32,
        bytes_to_lock_high: u32,
        overlapped: *mut Overlapped,
    ) -> i32;
    fn UnlockFileEx(
        file: *mut std::ffi::c_void,
        reserved: u32,
        bytes_to_unlock_low: u32,
        bytes_to_unlock_high: u32,
        overlapped: *mut Overlapped,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::{replace_profile_values, update_locked_profile_file};
    use crate::lease::{ProfileValues, ProviderError};
    use std::fs::{self, File, OpenOptions};
    use std::io::{Read, Seek, SeekFrom};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);
    const ORIGINAL_VALUES: ProfileValues = ProfileValues {
        frame_limit: 60,
        frame_limit_denominator: 1,
        sync_limiter: 0,
    };
    const APPLIED_VALUES: ProfileValues = ProfileValues {
        frame_limit: 72,
        frame_limit_denominator: 1,
        sync_limiter: 2,
    };

    struct ProfileFixture {
        directory: PathBuf,
        file: Option<File>,
        path: PathBuf,
    }

    impl ProfileFixture {
        fn new(content: &[u8]) -> Self {
            let directory = std::env::temp_dir().join(format!(
                "amaterasu-rtss-profile-test-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory).unwrap();
            let path = directory.join("game.exe.cfg");
            fs::write(&path, content).unwrap();
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            Self {
                directory,
                file: Some(file),
                path,
            }
        }

        fn file_mut(&mut self) -> &mut File {
            self.file.as_mut().unwrap()
        }

        fn contents(&mut self) -> Vec<u8> {
            self.file_mut().seek(SeekFrom::Start(0)).unwrap();
            let mut content = Vec::new();
            self.file_mut().read_to_end(&mut content).unwrap();
            content
        }
    }

    impl Drop for ProfileFixture {
        fn drop(&mut self) {
            drop(self.file.take());
            fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    const PROFILE: &[u8] = b"; retain this comment\r\n[Framerate]\r\nLimit=60\r\nLimitDenominator=1\r\nSyncLimiter=0\r\n[Compatibility]\r\nApp=game.exe\r\n";

    #[test]
    fn verified_update_changes_only_rtss_framerate_values() {
        let mut fixture = ProfileFixture::new(PROFILE);
        update_locked_profile_file(fixture.file_mut(), ORIGINAL_VALUES, APPLIED_VALUES).unwrap();
        assert!(fixture.path.is_file());
        assert_eq!(
            fixture.contents(),
            b"; retain this comment\r\n[Framerate]\r\nLimit=72\r\nLimitDenominator=1\r\nSyncLimiter=2\r\n[Compatibility]\r\nApp=game.exe\r\n"
        );
    }

    #[test]
    fn verified_update_rejects_external_values_without_mutating_fixture() {
        let mut fixture =
            ProfileFixture::new(b"[Framerate]\nLimit=61\nLimitDenominator=1\nSyncLimiter=0\n");
        let before = fixture.contents();
        assert_eq!(
            update_locked_profile_file(fixture.file_mut(), ORIGINAL_VALUES, APPLIED_VALUES),
            Err(ProviderError::ProfileMismatch {
                code: "rtss_profile_changed_before_write",
                observed: ProfileValues {
                    frame_limit: 61,
                    frame_limit_denominator: 1,
                    sync_limiter: 0,
                },
            })
        );
        assert_eq!(fixture.contents(), before);
    }

    #[test]
    fn verified_update_rejects_ambiguous_or_incomplete_profile_without_mutating_fixture() {
        for content in [
            b"[Framerate]\nLimit=60\nLimit=72\nLimitDenominator=1\nSyncLimiter=0\n".as_slice(),
            b"[Framerate]\nLimit=60\nLimitDenominator=1\n".as_slice(),
            b"[Framerate]\nLimit=60x\nLimitDenominator=1\nSyncLimiter=0\n".as_slice(),
            b"[Framerate]\nLimit=60\nLimitDenominator=1\nSyncLimiter=0\n[Framerate]\nLimit=60\n"
                .as_slice(),
        ] {
            let mut fixture = ProfileFixture::new(content);
            let before = fixture.contents();
            assert!(matches!(
                update_locked_profile_file(fixture.file_mut(), ORIGINAL_VALUES, APPLIED_VALUES),
                Err(ProviderError::ReadFailed("rtss_profile_values_unreadable"))
            ));
            assert_eq!(fixture.contents(), before);
        }
    }

    #[cfg(all(windows, target_pointer_width = "64"))]
    #[test]
    fn exclusive_byte_lock_blocks_a_preexisting_writer_handle() {
        use super::{FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, LockedProfileFile};
        use std::io::Write;
        use std::os::windows::fs::OpenOptionsExt;

        let mut fixture = ProfileFixture::new(PROFILE);
        let mut external_writer = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&fixture.path)
            .unwrap();
        let _lock = LockedProfileFile::open(&fixture.path).unwrap();
        external_writer.seek(SeekFrom::Start(0)).unwrap();
        assert!(external_writer.write_all(b"[Changed]").is_err());
        drop(_lock);
        assert_eq!(fixture.contents(), PROFILE);
    }

    #[test]
    fn pure_transform_refuses_a_different_expected_snapshot() {
        assert_eq!(
            replace_profile_values(
                b"[Framerate]\nLimit=60\nLimitDenominator=1\nSyncLimiter=0\n",
                APPLIED_VALUES,
                ORIGINAL_VALUES,
            ),
            Err(ProviderError::ProfileMismatch {
                code: "rtss_profile_changed_before_write",
                observed: ORIGINAL_VALUES,
            })
        );
    }
}
