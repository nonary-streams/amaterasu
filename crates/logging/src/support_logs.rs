//! Bounded support bundles assembled from the active writer's retained sessions.
use crate::{LogSnapshot, MAX_LOG_SNAPSHOT_CHUNK_BYTES, MAX_ROLLOVERS};
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use regex::Regex;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::net::IpAddr;
use std::sync::OnceLock;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const MAX_FILES: usize = 32;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 96 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 10_000;
const MAX_REPLACEMENTS: usize = 100_000;

pub(crate) struct LogSource {
    pub directory: Dir,
    pub base_name: String,
    pub active_label: String,
    pub current: LogSnapshot,
}

pub(crate) struct SupportBundle {
    pub bytes: Vec<u8>,
    pub sessions: usize,
    pub truncated: bool,
}

/// The archive never contains caller-selected paths. Each historical filename is
/// recognized from the writer's fixed naming scheme and opened relative to its
/// already-held directory capability without following a final symlink.
pub(crate) fn build(source: LogSource) -> io::Result<SupportBundle> {
    let mut groups: BTreeMap<String, BTreeMap<usize, OsString>> = BTreeMap::new();
    for (count, result) in source.directory.entries()?.enumerate() {
        if count >= MAX_DIRECTORY_ENTRIES {
            return Err(io::Error::other("log directory has too many entries"));
        }
        let entry = result?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(text) = name.to_str() else { continue };
        let Some((stamp, index)) = parse_segment(text, &source.base_name) else {
            continue;
        };
        if format!("{}-{stamp}", source.base_name) == source.active_label {
            continue;
        }
        groups
            .entry(stamp.to_owned())
            .or_default()
            .insert(index, name);
    }

    let mut remaining = MAX_BYTES;
    let mut remaining_files = MAX_FILES.saturating_sub(source.current.segments.len());
    let current_prefix_cut = source.current.earlier_segments_discarded() > 0;
    let mut truncated = current_prefix_cut;
    let current = read_current(&source.current, &mut remaining)?;
    if !current.is_empty() && current.last() != Some(&b'\n') {
        truncated = true;
    }
    let mut sessions = vec![(
        "logs/current-session.log".to_owned(),
        complete_lines(current, current_prefix_cut),
    )];

    for (stamp, entries) in groups.into_iter().rev() {
        if remaining == 0 || remaining_files == 0 {
            truncated = true;
            break;
        }
        if !entries.contains_key(&0) {
            truncated = true;
            continue;
        }
        let max_index = *entries.keys().next_back().expect("nonempty history group");
        if (0..=max_index).any(|index| !entries.contains_key(&index)) {
            truncated = true;
            continue;
        }
        let mut pieces = Vec::new();
        let mut prefix_cut = max_index == MAX_ROLLOVERS;
        if prefix_cut {
            truncated = true;
        }
        for index in 0..=max_index {
            if remaining == 0 || remaining_files == 0 {
                prefix_cut = true;
                truncated = true;
                break;
            }
            let name = entries.get(&index).expect("contiguous history was checked");
            let (bytes, cut) = read_history_segment(&source.directory, name, &mut remaining)?;
            pieces.push(bytes);
            remaining_files -= 1;
            if cut {
                prefix_cut = true;
                truncated = true;
                break;
            }
        }
        if pieces.len() < max_index + 1 {
            prefix_cut = true;
        }
        pieces.reverse();
        let length: usize = pieces.iter().map(Vec::len).sum();
        let mut combined = Vec::with_capacity(length);
        for piece in pieces {
            combined.extend(piece);
        }
        if !combined.is_empty() && combined.last() != Some(&b'\n') {
            truncated = true;
        }
        sessions.push((
            format!("logs/session-{stamp}.log"),
            complete_lines(combined, prefix_cut),
        ));
    }

    let mut sanitizer = Sanitizer::default();
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    let mut sanitized_bytes = 0usize;
    for (name, bytes) in &sessions {
        let text = String::from_utf8_lossy(bytes);
        let sanitized = sanitizer.sanitize(&text)?;
        sanitized_bytes = sanitized_bytes
            .checked_add(sanitized.len())
            .ok_or_else(|| io::Error::other("log export is too large"))?;
        if sanitized_bytes > MAX_OUTPUT_BYTES {
            return Err(io::Error::other("log export is too large"));
        }
        archive
            .start_file(name, options)
            .map_err(io::Error::other)?;
        archive.write_all(sanitized.as_bytes())?;
    }
    archive
        .start_file("README.txt", options)
        .map_err(io::Error::other)?;
    writeln!(
        archive,
        "Amaterasu retained host logs. {} session(s).",
        sessions.len()
    )?;
    writeln!(
        archive,
        "Common usernames, addresses, and credentials are redacted; review before sharing."
    )?;
    if truncated {
        writeln!(
            archive,
            "Some older data or incomplete boundary lines were omitted."
        )?;
    }
    let bytes = archive.finish().map_err(io::Error::other)?.into_inner();
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err(io::Error::other("log export is too large"));
    }
    Ok(SupportBundle {
        bytes,
        sessions: sessions.len(),
        truncated,
    })
}

fn parse_segment<'a>(name: &'a str, base: &str) -> Option<(&'a str, usize)> {
    let suffix = name.strip_prefix(base)?.strip_prefix('-')?;
    let stamp = suffix.get(..19)?;
    let valid = stamp.bytes().enumerate().all(|(index, value)| {
        if index == 8 || index == 15 {
            value == b'-'
        } else {
            value.is_ascii_digit()
        }
    });
    if !valid {
        return None;
    }
    let ending = suffix.get(19..)?.strip_prefix(".log")?;
    let index = if ending.is_empty() {
        0
    } else {
        ending.strip_prefix('.')?.parse::<usize>().ok()?
    };
    (index <= MAX_ROLLOVERS
        && (index == 0 && ending.is_empty() || index > 0 && ending == format!(".{index}")))
    .then_some((stamp, index))
}

fn read_current(snapshot: &LogSnapshot, remaining: &mut usize) -> io::Result<Vec<u8>> {
    let length = usize::try_from(snapshot.len()).map_err(io::Error::other)?;
    if length > *remaining {
        return Err(io::Error::other("current log exceeds export budget"));
    }
    let mut result = Vec::with_capacity(length);
    while result.len() < length {
        let part = snapshot.read_chunk(result.len() as u64, MAX_LOG_SNAPSHOT_CHUNK_BYTES)?;
        if part.is_empty() {
            return Err(io::Error::other("log snapshot did not advance"));
        }
        result.extend(part);
    }
    *remaining -= length;
    Ok(result)
}

fn read_history_segment(
    directory: &Dir,
    name: &OsString,
    remaining: &mut usize,
) -> io::Result<(Vec<u8>, bool)> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = directory.open_with(name, &options)?.into_std();
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::other("retained log is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(io::Error::other("retained log has multiple links"));
        }
    }
    #[cfg(windows)]
    {
        use std::mem::MaybeUninit;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: the owned file remains open for the duration of this call,
        // and Windows initializes the output structure on success.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful call above initialized the structure.
        if unsafe { info.assume_init() }.nNumberOfLinks != 1 {
            return Err(io::Error::other("retained log has multiple links"));
        }
    }
    let length = metadata.len();
    let count = usize::try_from(length.min(*remaining as u64)).map_err(io::Error::other)?;
    let start = length - count as u64;
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![0; count];
    file.read_exact(&mut bytes)?;
    *remaining -= count;
    if start == 0 && bytes.starts_with(crate::BOM) {
        bytes.drain(..crate::BOM.len());
    }
    Ok((bytes, start != 0))
}

fn complete_lines(mut bytes: Vec<u8>, prefix_cut: bool) -> Vec<u8> {
    if prefix_cut {
        let start = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |index| index + 1);
        bytes.drain(..start);
    }
    if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        let end = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        bytes.truncate(end);
    }
    bytes
}

#[derive(Default)]
struct Sanitizer {
    identities: HashMap<(String, String), usize>,
    next: usize,
}
struct Replacement {
    start: usize,
    end: usize,
    kind: &'static str,
    priority: u8,
}

impl Sanitizer {
    fn sanitize(&mut self, text: &str) -> io::Result<String> {
        let mut result = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            self.sanitize_line(line, &mut result)?;
        }
        Ok(result)
    }

    fn sanitize_line(&mut self, text: &str, result: &mut String) -> io::Result<()> {
        let mut matches = Vec::new();
        for (pattern, group, kind, priority) in patterns() {
            for capture in pattern.captures_iter(text) {
                let Some(value) = capture.get(*group) else {
                    continue;
                };
                if *kind == "IP" && value.as_str().parse::<IpAddr>().is_err() {
                    continue;
                }
                if *kind == "USER"
                    && matches!(
                        value.as_str().to_ascii_lowercase().as_str(),
                        "root"
                            | "system"
                            | "daemon"
                            | "nobody"
                            | "localsystem"
                            | "networkservice"
                            | "localservice"
                    )
                {
                    continue;
                }
                if matches.len() >= MAX_REPLACEMENTS {
                    return Err(io::Error::other("too many values to sanitize"));
                }
                matches.push(Replacement {
                    start: value.start(),
                    end: value.end(),
                    kind,
                    priority: *priority,
                });
            }
        }
        matches.sort_by_key(|value| (value.start, std::cmp::Reverse(value.end), value.priority));
        let mut position = 0;
        for value in matches {
            if value.start < position {
                continue;
            }
            result.push_str(&text[position..value.start]);
            let raw = &text[value.start..value.end];
            let key = (
                value.kind.to_owned(),
                if value.kind == "USER" || value.kind == "SECRET" {
                    raw.to_owned()
                } else {
                    raw.to_ascii_lowercase()
                },
            );
            if !self.identities.contains_key(&key) && self.identities.len() >= MAX_REPLACEMENTS {
                return Err(io::Error::other("too many identities to sanitize"));
            }
            let number = *self.identities.entry(key).or_insert_with(|| {
                self.next += 1;
                self.next
            });
            result.push_str(&format!("<{}_{number}>", value.kind));
            position = value.end;
            if result.len() > MAX_OUTPUT_BYTES {
                return Err(io::Error::other("sanitized log is too large"));
            }
        }
        result.push_str(&text[position..]);
        if result.len() > MAX_OUTPUT_BYTES {
            return Err(io::Error::other("sanitized log is too large"));
        }
        Ok(())
    }
}

fn patterns() -> &'static [(Regex, usize, &'static str, u8)] {
    static PATTERNS: OnceLock<Vec<(Regex, usize, &'static str, u8)>> = OnceLock::new();
    PATTERNS.get_or_init(|| [
        (r#"(?i)(?:[a-z]:[\\/]+users[\\/]+)([^\\/,:;"'<>()\x5b\x5d{}|\r\n]+)"#, 1, "USER", 10),
        (r#"(?i)/(?:home|users)/([^/\s,:;"'<>()\x5b\x5d{}|]+)"#, 1, "USER", 10),
        (r#"(?i)\b(?:username|user_name|user)\b["']?\s*[:=]\s*"([^"\r\n]+)""#, 1, "USER", 10),
        (r#"(?i)\b(?:username|user_name|user)\b["']?\s*[:=]\s*'([^'\r\n]+)'"#, 1, "USER", 10),
        (r#"(?i)\b(?:username|user_name|user)\b["']?\s*[:=]\s*["']?([a-z0-9._-]+)"#, 1, "USER", 10),
        (r#"(?i)\bcookie\b["']?\s*[:=]\s*([^\r\n]+)"#, 1, "SECRET", 0),
        (r#"(?i)\b(?:password|passwd|secret|(?:access[_-]?|refresh[_-]?)?token|api[_-]?key|authorization|cookie)\b["']?\s*[:=]\s*(?:bearer\s+)?"([^"\r\n]+)""#, 1, "SECRET", 0),
        (r#"(?i)\b(?:password|passwd|secret|(?:access[_-]?|refresh[_-]?)?token|api[_-]?key|authorization|cookie)\b["']?\s*[:=]\s*(?:bearer\s+)?'([^'\r\n]+)'"#, 1, "SECRET", 0),
        (r#"(?i)\b(?:password|passwd|secret|(?:access[_-]?|refresh[_-]?)?token|api[_-]?key|authorization|cookie)\b["']?\s*[:=]\s*(?:bearer\s+)?([^\s"',;]+)"#, 1, "SECRET", 0),
        (r"(?i)\bama_[a-f0-9]{64}\b", 0, "SECRET", 0),
        (r"(?i)\b[0-9a-f]{2}(?::[0-9a-f]{2}){5}\b|\b[0-9a-f]{2}(?:-[0-9a-f]{2}){5}\b", 0, "MAC", 20),
        (r"(?i)\b[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)+\b", 0, "EMAIL", 20),
        (r"(?:[0-9]{1,3}\.){3}[0-9]{1,3}", 0, "IP", 30),
        (r"[0-9A-Fa-f:.]{2,}", 0, "IP", 30),
    ].into_iter().map(|(pattern, group, kind, priority)| (Regex::new(pattern).expect("fixed redaction pattern"), group, kind, priority)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionWriter;
    use std::fs;
    use std::path::PathBuf;
    use zip::ZipArchive;

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "amaterasu-support-{}",
                crate::host_logs::random_id().unwrap()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn unpack(bytes: Vec<u8>) -> BTreeMap<String, String> {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut files = BTreeMap::new();
        for index in 0..archive.len() {
            let mut file = archive.by_index(index).unwrap();
            let name = file.name().to_owned();
            let mut data = String::new();
            file.read_to_string(&mut data).unwrap();
            files.insert(name, data);
        }
        files
    }

    #[test]
    fn export_combines_rotated_sessions_and_sanitizes_consistently() {
        let root = Directory::new();
        let path = root.0.join("product.log");
        let mut previous = SessionWriter::with_limits(&path, &root.0, 55, 4).unwrap();
        previous.write_line(b"previous username=alice ip=192.168.12.3 email=alice@example.com\npassword=\"a private secret\" home=/home/alice/game\n").unwrap();
        drop(previous);
        let mut current = SessionWriter::with_limits(&path, &root.0, 1024, 4).unwrap();
        current
            .write_line(b"current username=alice ip=192.168.12.3 mac=aa:bb:cc:dd:ee:ff\n")
            .unwrap();
        let bundle = build(current.support_log_source().unwrap()).unwrap();
        assert_eq!(bundle.sessions, 2);
        assert!(!bundle.truncated);
        let files = unpack(bundle.bytes);
        assert_eq!(files.len(), 3);
        let history = files
            .iter()
            .find(|(name, _)| name.starts_with("logs/session-"))
            .unwrap()
            .1;
        let current = &files["logs/current-session.log"];
        for private in [
            "alice",
            "192.168.12.3",
            "alice@example.com",
            "a private secret",
        ] {
            assert!(!history.contains(private), "leaked {private}");
        }
        assert!(!current.contains("alice"));
        assert!(!current.contains("aa:bb:cc:dd:ee:ff"));
        let user_token = current
            .split("username=")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        assert!(history.contains(user_token));
        assert!(history.contains("previous"));
        assert!(history.contains("home=/home/"));
    }

    #[test]
    fn accepted_names_are_exact_and_partial_boundaries_are_discarded() {
        assert_eq!(
            parse_segment("product-20261026-102030-123.log.4", "product"),
            Some(("20261026-102030-123", 4))
        );
        for invalid in [
            "product-20261026-102030-123.log.0",
            "product-20261026-102030-123.log.5",
            "product-20261026-102030-123.log.1.bak",
            "product-20261026-102030-123.log/../../private",
            "other-20261026-102030-123.log",
        ] {
            assert!(
                parse_segment(invalid, "product").is_none(),
                "accepted {invalid}"
            );
        }
        assert_eq!(
            complete_lines(b"partial\nwhole\ntail".to_vec(), true),
            b"whole\n"
        );
    }

    #[test]
    fn redaction_covers_addresses_with_ports_quoted_credentials_and_cookies() {
        let source = "address=192.168.2.9:443 ipv6=[fd00::1234]:1234 user=alice path=C:\\Users\\alice\\game\n\"username\": \"Alice Private Name\"\n\"access_token\": \"two private words\"\nCookie: first=abc; second=def\n";
        let result = Sanitizer::default().sanitize(source).unwrap();
        for private in [
            "192.168.2.9",
            "fd00::1234",
            "alice",
            "Alice Private Name",
            "two private words",
            "abc",
            "def",
        ] {
            assert!(!result.contains(private), "leaked {private} in {result}");
        }
        assert!(result.contains(":443"));
    }

    #[test]
    fn newest_retained_segments_are_bounded_and_omissions_are_reported() {
        let root = Directory::new();
        let mut current = SessionWriter::open(&root.0.join("product.log"), &root.0).unwrap();
        current.write_line(b"current\n").unwrap();
        let logs = root.0.join("logs");
        for index in 0..35 {
            let path = logs.join(format!("product-20200101-{index:06}-001.log"));
            fs::write(path, format!("\u{feff}historic-{index}\n")).unwrap();
        }
        let bundle = build(current.support_log_source().unwrap()).unwrap();
        assert_eq!(bundle.sessions, 32);
        assert!(bundle.truncated);
        let files = unpack(bundle.bytes);
        assert!(files.values().any(|value| value.contains("historic-34")));
        assert!(!files.values().any(|value| value.contains("historic-0\n")));
        assert!(files["README.txt"].contains("omitted"));
    }

    #[test]
    fn repeated_identifiers_in_large_logs_do_not_exhaust_the_redaction_budget() {
        let input = "peer=192.0.2.9\n".repeat(MAX_REPLACEMENTS + 1);
        let output = Sanitizer::default().sanitize(&input).unwrap();
        assert_eq!(output.lines().count(), MAX_REPLACEMENTS + 1);
        assert!(!output.contains("192.0.2.9"));
    }

    #[cfg(unix)]
    #[test]
    fn links_cannot_redirect_export_to_an_unrelated_file() {
        use std::os::unix::fs::symlink;
        let root = Directory::new();
        let mut writer = SessionWriter::open(&root.0.join("product.log"), &root.0).unwrap();
        writer.write_line(b"current\n").unwrap();
        let private = root.0.join("private");
        fs::write(&private, b"private contents\n").unwrap();
        let logs = root.0.join("logs");
        symlink(&private, logs.join("product-99999999-999999-999.log")).unwrap();
        let archive = unpack(build(writer.support_log_source().unwrap()).unwrap().bytes);
        assert!(
            !archive
                .values()
                .any(|value| value.contains("private contents"))
        );
        fs::hard_link(&private, logs.join("product-99999999-999999-998.log")).unwrap();
        assert!(build(writer.support_log_source().unwrap()).is_err());
    }
}
