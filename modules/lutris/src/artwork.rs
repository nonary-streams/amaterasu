//! Opaque, bounded reads of artwork owned by the discovered Lutris user.

use crate::discovery::Snapshot;
use serde_json::{Value, json};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use std::cell::RefCell;
use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_ARTWORK_BYTES: u64 = 16 * 1024 * 1024;
const CHUNK_BYTES: u64 = 8 * 1024;
const MAX_SCAN_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SCAN_TIME: Duration = Duration::from_secs(8);

pub struct ArtworkScan {
    pub revisions: BTreeMap<i64, String>,
    pub incomplete: bool,
    pub scanned_games: usize,
}

#[derive(Default)]
pub struct ArtworkStore {
    lutris_data: PathBuf,
    slugs: BTreeMap<i64, String>,
    #[cfg(target_os = "linux")]
    cache: RefCell<Option<CachedArtwork>>,
}

#[derive(Clone)]
struct ArtworkFile {
    bytes: Arc<[u8]>,
    revision: String,
    mime: &'static str,
}

#[cfg(target_os = "linux")]
struct CachedArtwork {
    path: PathBuf,
    identity: FileIdentity,
    file: ArtworkFile,
}

#[cfg(target_os = "linux")]
#[derive(Eq, PartialEq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

#[cfg(target_os = "linux")]
impl FileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            len: metadata.len(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        }
    }
}

impl ArtworkStore {
    #[cfg(test)]
    pub fn contains_game_for_test(&self, game_id: i64) -> bool {
        self.slugs.contains_key(&game_id)
    }
    pub fn clear(&mut self) {
        self.lutris_data.clear();
        self.slugs.clear();
        #[cfg(target_os = "linux")]
        self.cache.replace(None);
    }

    pub fn update_if_current(
        &mut self,
        snapshot: &Snapshot,
        captured_epoch: u64,
        current_epoch: u64,
        enabled: bool,
    ) {
        if enabled && captured_epoch == current_epoch {
            self.update(snapshot);
        }
    }

    pub fn update(&mut self, snapshot: &Snapshot) {
        #[cfg(target_os = "linux")]
        self.cache.replace(None);
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
            "total_bytes":file.bytes.len(),"mime_type":file.mime
        }))
    }

    pub fn revisions(snapshot: &Snapshot) -> ArtworkScan {
        Self::revisions_with_budget(snapshot, MAX_SCAN_BYTES, MAX_SCAN_TIME)
    }

    fn revisions_with_budget(
        snapshot: &Snapshot,
        max_bytes: u64,
        max_time: Duration,
    ) -> ArtworkScan {
        let mut store = Self::default();
        store.update(snapshot);
        let started = Instant::now();
        let mut total_bytes = 0_u64;
        let mut revisions = BTreeMap::new();
        for (index, game) in snapshot.games.iter().enumerate() {
            if started.elapsed() > max_time {
                return ArtworkScan {
                    revisions,
                    incomplete: true,
                    scanned_games: index,
                };
            }
            if let Ok(file) = store.resolve(game.id) {
                let Some(next_total) = total_bytes.checked_add(file.bytes.len() as u64) else {
                    return ArtworkScan {
                        revisions,
                        incomplete: true,
                        scanned_games: index,
                    };
                };
                if next_total > max_bytes || started.elapsed() > max_time {
                    return ArtworkScan {
                        revisions,
                        incomplete: true,
                        scanned_games: index,
                    };
                }
                total_bytes = next_total;
                revisions.insert(game.id, file.revision);
            }
        }
        ArtworkScan {
            revisions,
            incomplete: false,
            scanned_games: snapshot.games.len(),
        }
    }

    pub fn read(&self, game_id: i64, revision: &str, offset: u64) -> Result<Value, &'static str> {
        if !valid_revision(revision)
            || !offset.is_multiple_of(CHUNK_BYTES)
            || offset >= MAX_ARTWORK_BYTES
        {
            return Err("invalid_cursor");
        }
        let source = self.resolve(game_id)?;
        let size = source.bytes.len() as u64;
        if source.revision != revision {
            return Err("artwork_changed");
        }
        if offset >= size {
            return Err("invalid_cursor");
        }
        let next = (offset + CHUNK_BYTES).min(size);
        let bytes = &source.bytes[offset as usize..next as usize];
        Ok(json!({
            "ok":true,"game_id":game_id,"revision":revision,"offset":offset,
            "next_offset":(next < size).then_some(next),
            "total_bytes":size,"mime_type":source.mime,"data":base64(bytes)
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
            if let Some(file) = self.inspect(&cover.join(format!("{slug}.{extension}")), &data) {
                return Ok(file);
            }
        }
        for size in ["256x256", "128x128", "64x64", "48x48"] {
            if let Some(file) = self.inspect(
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

    fn inspect(&self, path: &Path, root: &Path) -> Option<ArtworkFile> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (path, root);
            None
        }
        #[cfg(target_os = "linux")]
        {
            inspect_open_file(path, root, File::open(path).ok()?, &self.cache)
        }
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
    revision.len() == 64
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(target_os = "linux")]
fn inspect_open_file(
    path: &Path,
    root: &Path,
    mut file: File,
    cache: &RefCell<Option<CachedArtwork>>,
) -> Option<ArtworkFile> {
    use std::os::fd::AsRawFd;

    // The descriptor target, not a separately resolved pathname, is the
    // object whose bytes we will return. Unknown descriptor identity fails closed.
    let opened_path = fs::canonicalize(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()?;
    if !opened_path.starts_with(root) || !fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_ARTWORK_BYTES {
        return None;
    }
    let identity = FileIdentity::from_metadata(&metadata);
    if let Some(cached) = cache.borrow().as_ref()
        && cached.path == opened_path
        && cached.identity == identity
    {
        return Some(cached.file.clone());
    }
    let mut bytes = vec![0; metadata.len() as usize];
    file.read_exact(&mut bytes).ok()?;
    let after = file.metadata().ok()?;
    if after.len() != metadata.len() {
        return None;
    }
    let mime = mime(&bytes[..bytes.len().min(12)])?;
    let artwork = ArtworkFile {
        revision: format!("{:x}", Sha256::digest(&bytes)),
        bytes: Arc::from(bytes),
        mime,
    };
    cache.replace(Some(CachedArtwork {
        path: opened_path,
        identity,
        file: artwork.clone(),
    }));
    Some(artwork)
}

#[cfg(target_os = "linux")]
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::discovery::Game;
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn artwork_budget_omits_later_images_without_failing_game_snapshot() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("amaterasu-lutris-budget-{nonce}"));
        let data = root.join("lutris");
        let cover = data.join("coverart");
        fs::create_dir_all(&cover).unwrap();
        let image = [b"\x89PNG\r\n\x1a\n".as_slice(), b"image"].concat();
        fs::write(cover.join("first.png"), &image).unwrap();
        fs::write(cover.join("second.png"), &image).unwrap();
        let game = |id, slug: &str| Game {
            id,
            name: slug.into(),
            slug: slug.into(),
            runner: "wine".into(),
            platform: "linux".into(),
            directory: String::new(),
            config_path: "game.yml".into(),
            service: String::new(),
            service_id: String::new(),
        };
        let source = Snapshot {
            database_path: data.join("pga.db"),
            lutris_executable: PathBuf::from("/usr/bin/lutris"),
            launch_kind: crate::discovery::LaunchKind::Native,
            games: vec![game(1, "first"), game(2, "second")],
            artwork_revisions: BTreeMap::new(),
            artwork_scan_incomplete: false,
            artwork_scanned_games: 0,
        };
        let scan = ArtworkStore::revisions_with_budget(&source, image.len() as u64, MAX_SCAN_TIME);
        assert!(scan.incomplete);
        assert_eq!(scan.scanned_games, 1);
        assert_eq!(scan.revisions.len(), 1);
        assert!(scan.revisions.contains_key(&1));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sequential_chunks_reuse_validated_bytes_and_replacement_invalidates_cache() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = std::env::temp_dir().join(format!("amaterasu-lutris-cache-{nonce}"));
        let root = parent.join("allowed");
        fs::create_dir_all(&root).unwrap();
        let candidate = root.join("cover.png");
        fs::write(
            &candidate,
            [
                b"\x89PNG\r\n\x1a\n".as_slice(),
                &vec![7; CHUNK_BYTES as usize],
            ]
            .concat(),
        )
        .unwrap();
        let cache = RefCell::new(None);
        let canonical_root = root.canonicalize().unwrap();
        let first = inspect_open_file(
            &candidate,
            &canonical_root,
            File::open(&candidate).unwrap(),
            &cache,
        )
        .unwrap();
        let second = inspect_open_file(
            &candidate,
            &canonical_root,
            File::open(&candidate).unwrap(),
            &cache,
        )
        .unwrap();
        assert!(Arc::ptr_eq(&first.bytes, &second.bytes));
        fs::write(
            &candidate,
            [
                b"\x89PNG\r\n\x1a\n".as_slice(),
                &vec![8; CHUNK_BYTES as usize],
            ]
            .concat(),
        )
        .unwrap();
        let replaced = inspect_open_file(
            &candidate,
            &canonical_root,
            File::open(&candidate).unwrap(),
            &cache,
        )
        .unwrap();
        assert!(!Arc::ptr_eq(&first.bytes, &replaced.bytes));
        assert_ne!(first.revision, replaced.revision);
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn opened_descriptor_outside_allowed_root_is_rejected_after_path_swap() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = std::env::temp_dir().join(format!("amaterasu-lutris-fd-{nonce}"));
        let root = parent.join("allowed");
        fs::create_dir_all(&root).unwrap();
        let candidate = root.join("cover.png");
        let outside = parent.join("outside.png");
        let image = [b"\x89PNG\r\n\x1a\n".as_slice(), b"image"].concat();
        fs::write(&candidate, &image).unwrap();
        fs::write(&outside, &image).unwrap();
        let canonical_root = root.canonicalize().unwrap();

        // Model a swap between candidate selection and open: the requested
        // path still looks valid, but the descriptor points to an outside file.
        assert!(
            inspect_open_file(
                &candidate,
                &canonical_root,
                File::open(&outside).unwrap(),
                &RefCell::new(None)
            )
            .is_none()
        );
        fs::remove_file(&candidate).unwrap();
        symlink(&outside, &candidate).unwrap();
        assert!(
            inspect_open_file(
                &candidate,
                &canonical_root,
                File::open(&candidate).unwrap(),
                &RefCell::new(None)
            )
            .is_none()
        );
        fs::remove_dir_all(parent).unwrap();
    }
}
