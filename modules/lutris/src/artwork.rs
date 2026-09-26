//! Opaque, bounded reads of artwork owned by the discovered Lutris user.

use crate::discovery::Snapshot;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const MAX_ARTWORK_BYTES: u64 = 16 * 1024 * 1024;
const CHUNK_BYTES: u64 = 8 * 1024;

#[derive(Default)]
pub struct ArtworkStore {
    lutris_data: PathBuf,
    slugs: BTreeMap<i64, String>,
}

struct ArtworkFile {
    path: PathBuf,
    revision: String,
    size: u64,
    mime: &'static str,
}

impl ArtworkStore {
    pub fn update(&mut self, snapshot: &Snapshot) {
        self.lutris_data = snapshot
            .database_path
            .parent()
            .unwrap_or(Path::new(""))
            .to_owned();
        self.slugs = snapshot
            .games
            .iter()
            .filter(|game| safe_slug(&game.slug))
            .map(|game| (game.id, game.slug.clone()))
            .collect();
    }

    pub fn metadata(&self, game_id: i64) -> Result<Value, &'static str> {
        let file = self.resolve(game_id)?;
        Ok(json!({
            "ok":true,"game_id":game_id,"revision":file.revision,
            "total_bytes":file.size,"mime_type":file.mime
        }))
    }

    pub fn read(&self, game_id: i64, revision: &str, offset: u64) -> Result<Value, &'static str> {
        if !valid_revision(revision)
            || !offset.is_multiple_of(CHUNK_BYTES)
            || offset >= MAX_ARTWORK_BYTES
        {
            return Err("invalid_cursor");
        }
        let source = self.resolve(game_id)?;
        if source.revision != revision {
            return Err("artwork_changed");
        }
        if offset >= source.size {
            return Err("invalid_cursor");
        }
        let mut file = File::open(&source.path).map_err(|_| "artwork_unavailable")?;
        let opened = file.metadata().map_err(|_| "artwork_unavailable")?;
        if revision_for(&source.path, &opened) != revision {
            return Err("artwork_changed");
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| "artwork_unavailable")?;
        let mut bytes = vec![0; (source.size - offset).min(CHUNK_BYTES) as usize];
        file.read_exact(&mut bytes).map_err(|_| "artwork_changed")?;
        if revision_for(
            &source.path,
            &file.metadata().map_err(|_| "artwork_changed")?,
        ) != revision
        {
            return Err("artwork_changed");
        }
        let next = offset + bytes.len() as u64;
        Ok(json!({
            "ok":true,"game_id":game_id,"revision":revision,"offset":offset,
            "next_offset":(next < source.size).then_some(next),
            "total_bytes":source.size,"mime_type":source.mime,"data":base64(&bytes)
        }))
    }

    fn resolve(&self, game_id: i64) -> Result<ArtworkFile, &'static str> {
        let slug = self.slugs.get(&game_id).ok_or("artwork_unavailable")?;
        let data = self
            .lutris_data
            .canonicalize()
            .map_err(|_| "artwork_unavailable")?;
        let home = data.parent().ok_or("artwork_unavailable")?;
        let cover = data.join("coverart");
        for extension in ["png", "jpg", "jpeg", "webp"] {
            if let Some(file) = inspect(&cover.join(format!("{slug}.{extension}")), &data) {
                return Ok(file);
            }
        }
        for size in ["256x256", "128x128", "64x64", "48x48"] {
            if let Some(file) = inspect(
                &home
                    .join("icons/hicolor")
                    .join(size)
                    .join("apps")
                    .join(format!("lutris_{slug}.png")),
                home,
            ) {
                return Ok(file);
            }
        }
        Err("artwork_unavailable")
    }
}

fn safe_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 256
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_revision(revision: &str) -> bool {
    !revision.is_empty()
        && revision.len() <= 20
        && revision.bytes().all(|byte| byte.is_ascii_digit())
}

fn inspect(path: &Path, root: &Path) -> Option<ArtworkFile> {
    if !fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
    let path = path.canonicalize().ok()?;
    if !path.starts_with(root) {
        return None;
    }
    let metadata = fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_ARTWORK_BYTES {
        return None;
    }
    let mut header = [0; 12];
    let count = File::open(&path).ok()?.read(&mut header).ok()?;
    let mime = mime(&header[..count])?;
    Some(ArtworkFile {
        revision: revision_for(&path, &metadata),
        size: metadata.len(),
        path,
        mime,
    })
}

fn mime(header: &[u8]) -> Option<&'static str> {
    if header.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if header.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if header.len() >= 12 && &header[..4] == b"RIFF" && &header[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn revision_for(path: &Path, metadata: &fs::Metadata) -> String {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |time| time.as_nanos());
    let mut hash = 1_469_598_103_934_665_603_u64;
    for byte in path
        .to_string_lossy()
        .bytes()
        .chain(metadata.len().to_le_bytes())
        .chain(modified.to_le_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    hash.to_string()
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        result.push(ALPHABET[(a >> 2) as usize] as char);
        result.push(ALPHABET[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        result.push(if chunk.len() > 1 {
            ALPHABET[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            ALPHABET[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    result
}
