//! Restrict untrusted wheel ZIP metadata before zip's central-directory parser.
//!
//! The pinned wheel is single-disk ZIP32 with no archive comment. Freeze its
//! bounded directory and footer before parsing, then hide earlier file data
//! until parsing succeeds. Otherwise a malformed last directory could cause
//! the ZIP parser to search for an attacker-controlled earlier footer.
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const EOCD_BYTES: u64 = 22;
const CENTRAL_HEADER_BYTES: usize = 46;
const MAX_ENTRIES: usize = 4096;
const MAX_DIRECTORY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_WHEEL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_FIELD_BYTES: usize = 4096;

/// Open the pinned wheel format after validating and freezing its metadata.
/// File payload becomes readable only after the ZIP parser has succeeded.
pub fn open<R: Read + Seek>(mut reader: R) -> io::Result<zip::ZipArchive<GuardedZip<R>>> {
    let length = reader.seek(SeekFrom::End(0))?;
    if !(EOCD_BYTES..=MAX_WHEEL_BYTES).contains(&length) {
        return Err(invalid("wheel length outside permitted bounds"));
    }
    reader.seek(SeekFrom::End(-(EOCD_BYTES as i64)))?;
    let mut footer = [0_u8; EOCD_BYTES as usize];
    reader.read_exact(&mut footer)?;
    if &footer[..4] != b"PK\x05\x06" || u16_at(&footer, 20) != 0 {
        return Err(invalid("wheel must end with an uncommented ZIP32 footer"));
    }
    let count = usize::from(u16_at(&footer, 10));
    if u16_at(&footer, 4) != 0
        || u16_at(&footer, 6) != 0
        || usize::from(u16_at(&footer, 8)) != count
        || count == 0
        || count > MAX_ENTRIES
    {
        return Err(invalid("invalid wheel directory count or disk"));
    }
    let size = u64::from(u32_at(&footer, 12));
    let directory_start = u64::from(u32_at(&footer, 16));
    if size > MAX_DIRECTORY_BYTES
        || size < (count * CENTRAL_HEADER_BYTES) as u64
        || directory_start.checked_add(size) != Some(length - EOCD_BYTES)
    {
        return Err(invalid("wheel directory bounds invalid"));
    }
    let mut metadata = vec![0_u8; (size + EOCD_BYTES) as usize];
    reader.seek(SeekFrom::Start(directory_start))?;
    reader.read_exact(&mut metadata)?;
    if metadata[size as usize..] != footer {
        return Err(invalid("wheel footer changed during validation"));
    }
    if metadata
        .windows(4)
        .enumerate()
        .any(|(offset, signature)| signature == b"PK\x05\x06" && offset != size as usize)
    {
        return Err(invalid("ambiguous wheel footer signature"));
    }
    if size >= 20 && &metadata[size as usize - 20..size as usize - 16] == b"PK\x06\x07" {
        return Err(invalid("ZIP64 locator is unsupported"));
    }
    validate_directory(&metadata[..size as usize], count, directory_start)?;
    let unlocked = Arc::new(AtomicBool::new(false));
    let protected = GuardedZip {
        reader,
        metadata,
        directory_start,
        position: 0,
        unlocked: unlocked.clone(),
    };
    let archive = zip::ZipArchive::new(protected).map_err(io::Error::other)?;
    unlocked.store(true, Ordering::Release);
    Ok(archive)
}

fn validate_directory(bytes: &[u8], count: usize, directory_start: u64) -> io::Result<()> {
    let mut offset = 0_usize;
    for _ in 0..count {
        let end = offset
            .checked_add(CENTRAL_HEADER_BYTES)
            .ok_or_else(|| invalid("invalid directory entry"))?;
        let header = bytes
            .get(offset..end)
            .ok_or_else(|| invalid("truncated directory entry"))?;
        if &header[..4] != b"PK\x01\x02"
            || u16_at(header, 34) != 0
            || u32_at(header, 20) == u32::MAX
            || u32_at(header, 24) == u32::MAX
            || u32_at(header, 42) == u32::MAX
            || u64::from(u32_at(header, 42)) >= directory_start
        {
            return Err(invalid("unsupported wheel directory entry"));
        }
        let name = usize::from(u16_at(header, 28));
        let extra = usize::from(u16_at(header, 30));
        let comment = usize::from(u16_at(header, 32));
        if name == 0
            || name > MAX_FIELD_BYTES
            || extra > MAX_FIELD_BYTES
            || comment > MAX_FIELD_BYTES
        {
            return Err(invalid("wheel directory field exceeds permitted bounds"));
        }
        offset = end
            .checked_add(name)
            .and_then(|x| x.checked_add(extra))
            .and_then(|x| x.checked_add(comment))
            .ok_or_else(|| invalid("directory entry overflow"))?;
        if offset > bytes.len() {
            return Err(invalid("truncated wheel directory fields"));
        }
    }
    if offset != bytes.len() {
        return Err(invalid("wheel directory size or count mismatch"));
    }
    Ok(())
}

/// Reader that preserves verified directory bytes across all subsequent reads.
pub struct GuardedZip<R> {
    reader: R,
    metadata: Vec<u8>,
    directory_start: u64,
    position: u64,
    unlocked: Arc<AtomicBool>,
}

impl<R: Read + Seek> Read for GuardedZip<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let length = self.directory_start + self.metadata.len() as u64;
        if self.position >= length {
            return Ok(0);
        }
        if self.position < self.directory_start {
            let count = out
                .len()
                .min((self.directory_start - self.position) as usize);
            if !self.unlocked.load(Ordering::Acquire) {
                if (out.len() as u64) < self.directory_start - self.position {
                    return Err(invalid(
                        "wheel payload unavailable during directory parsing",
                    ));
                }
                out[..count].fill(0);
                self.position += count as u64;
                return Ok(count);
            }
            self.reader.seek(SeekFrom::Start(self.position))?;
            let actual = self.reader.read(&mut out[..count])?;
            self.position += actual as u64;
            return Ok(actual);
        }
        let offset = (self.position - self.directory_start) as usize;
        let count = out.len().min(self.metadata.len() - offset);
        out[..count].copy_from_slice(&self.metadata[offset..offset + count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl<R: Read + Seek> Seek for GuardedZip<R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let length = self.directory_start + self.metadata.len() as u64;
        let target = match from {
            SeekFrom::Start(x) => i128::from(x),
            SeekFrom::Current(x) => i128::from(self.position) + i128::from(x),
            SeekFrom::End(x) => i128::from(length) + i128::from(x),
        };
        self.position = u64::try_from(target)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid wheel seek"))?;
        Ok(self.position)
    }
}
fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn wheel() -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("a", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"payload").unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn normal_archive_opens_and_payload_is_available_after_parsing() {
        let mut archive = open(Cursor::new(wheel())).unwrap();
        let mut data = String::new();
        archive
            .by_name("a")
            .unwrap()
            .read_to_string(&mut data)
            .unwrap();
        assert_eq!(data, "payload");
    }

    #[test]
    fn rejects_zip64_and_invalid_directory() {
        let mut bytes = wheel();
        let footer = bytes.len() - 22;
        bytes[footer + 10..footer + 12].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(open(Cursor::new(bytes)).is_err());
        let mut bytes = wheel();
        let footer = bytes.len() - 22;
        bytes[footer + 12..footer + 16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(open(Cursor::new(bytes)).is_err());
    }

    #[test]
    fn rejects_embedded_footer_and_zip64_locator_signatures() {
        let mut bytes = wheel();
        let footer = bytes.len() - 22;
        let directory = u32_at(&bytes, footer + 16) as usize;
        // A footer signature in entry metadata must not become a fallback target.
        bytes[directory + 8..directory + 12].copy_from_slice(b"PK\x05\x06");
        assert!(open(Cursor::new(bytes)).is_err());
        let mut bytes = wheel();
        let footer = bytes.len() - 22;
        bytes[footer - 20..footer - 16].copy_from_slice(b"PK\x06\x07");
        assert!(open(Cursor::new(bytes)).is_err());
    }

    #[test]
    fn parser_cannot_fall_back_to_footer_hidden_in_payload() {
        let prefix = wheel();
        let mut second = wheel();
        let footer = second.len() - 22;
        let directory = u32_at(&second, footer + 16) as usize;
        second[footer + 16..footer + 20]
            .copy_from_slice(&((directory + prefix.len()) as u32).to_le_bytes());
        second[directory + 42..directory + 46]
            .copy_from_slice(&(prefix.len() as u32).to_le_bytes());
        // AES without its mandatory metadata is rejected by the ZIP parser.
        second[directory + 10..directory + 12].copy_from_slice(&99_u16.to_le_bytes());
        let mut combined = prefix;
        combined.extend(second);
        assert!(zip::ZipArchive::new(Cursor::new(combined.clone())).is_ok());
        assert!(open(Cursor::new(combined)).is_err());
    }

    #[test]
    fn rejects_trailing_false_footer_instead_of_falling_back() {
        let mut bytes = wheel();
        let original_footer = bytes.len() - 22;
        let false_footer = bytes[original_footer..].to_vec();
        bytes.extend(false_footer);
        let last = bytes.len() - 22;
        bytes[last + 10..last + 12].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(open(Cursor::new(bytes)).is_err());
    }
}
