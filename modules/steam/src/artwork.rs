//! Bounded local Steam artwork discovery for catalog synchronization.
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const MAX_ARTWORK_ROOTS: usize = 4;
const MAX_USER_DIRECTORIES: usize = 32;
const MAX_HASH_DIRECTORIES: usize = 32;
const MAX_ARTWORK_BYTES: u64 = 8 * 1024 * 1024;
pub const ARTWORK_CHUNK_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtworkMetadata {
    pub source_path: Option<String>,
    pub revision: Option<String>,
}

#[derive(Debug)]
pub struct ArtworkChunk {
    pub revision: String,
    pub total_bytes: u64,
    pub next_offset: Option<u64>,
    pub mime_type: &'static str,
    pub data: String,
}

/// Finds an existing local portrait first, then a header or icon. It reads only
/// bounded Steam cache and grid directory entries and never follows an artwork
/// file symlink outside a canonical Steam root.
pub fn discover(app_id: u32, library: &Path, configured_roots: &[PathBuf]) -> ArtworkMetadata {
    let mut roots = Vec::new();
    let mut seen_roots = BTreeSet::new();
    for root in std::iter::once(library).chain(configured_roots.iter().map(PathBuf::as_path)) {
        let Ok(root) = root.canonicalize() else {
            continue;
        };
        if seen_roots.insert(root.clone()) {
            roots.push(root);
        }
        if roots.len() == MAX_ARTWORK_ROOTS {
            break;
        }
    }

    for root in roots {
        if let Some(metadata) =
            from_library_cache(app_id, &root).or_else(|| from_user_grid(app_id, &root))
        {
            return metadata;
        }
    }
    ArtworkMetadata::default()
}

fn from_library_cache(app_id: u32, root: &Path) -> Option<ArtworkMetadata> {
    let cache_directories = [
        root.join("appcache/librarycache"),
        root.join("librarycache"),
    ];
    let file_suffixes = [
        "_library_600x900_2x",
        "_library_600x900",
        "_header",
        "_icon",
    ];
    let extensions = ["jpg", "png", "webp"];

    for cache in &cache_directories {
        for suffix in file_suffixes {
            for extension in extensions {
                let candidate = cache.join(format!("{app_id}{suffix}.{extension}"));
                if let Some(metadata) = inspect_candidate(&candidate, root) {
                    return Some(metadata);
                }
            }
        }
        if let Some(metadata) = from_hashed_library_cache(app_id, cache, root) {
            return Some(metadata);
        }
    }
    None
}

fn from_hashed_library_cache(
    app_id: u32,
    cache: &Path,
    canonical_root: &Path,
) -> Option<ArtworkMetadata> {
    let app_directory = cache.join(app_id.to_string());
    let mut hash_directories = bounded_directories(&app_directory, MAX_HASH_DIRECTORIES)?;
    hash_directories.sort();
    for hash_directory in hash_directories {
        for name in [
            "library_600x900_2x.jpg",
            "library_600x900_2x.png",
            "library_600x900_2x.webp",
            "library_600x900.jpg",
            "library_600x900.png",
            "library_600x900.webp",
            "library_capsule.jpg",
            "library_capsule.png",
            "library_capsule.webp",
            "library_header.jpg",
            "library_header.png",
            "library_header.webp",
        ] {
            if let Some(metadata) = inspect_candidate(&hash_directory.join(name), canonical_root) {
                return Some(metadata);
            }
        }
    }
    None
}

fn from_user_grid(app_id: u32, root: &Path) -> Option<ArtworkMetadata> {
    let mut users = bounded_directories(&root.join("userdata"), MAX_USER_DIRECTORIES)?;
    users.sort();
    let suffixes = [
        "p.png",
        "p.jpg",
        "_p.png",
        "_p.jpg",
        "_hero.png",
        "_hero.jpg",
        "_icon.png",
        "_icon.jpg",
        ".png",
    ];
    for user in users {
        let grid = user.join("config/grid");
        for suffix in suffixes {
            if let Some(metadata) = inspect_candidate(&grid.join(format!("{app_id}{suffix}")), root)
            {
                return Some(metadata);
            }
        }
    }
    None
}

fn bounded_directories(directory: &Path, limit: usize) -> Option<Vec<PathBuf>> {
    let entries = fs::read_dir(directory).ok()?;
    let mut directories = Vec::new();
    for entry in entries.take(limit.saturating_add(1)) {
        let entry = entry.ok()?;
        if entry.file_type().ok()?.is_dir() {
            directories.push(entry.path());
            if directories.len() > limit {
                return None;
            }
        }
    }
    Some(directories)
}

fn inspect_candidate(candidate: &Path, canonical_root: &Path) -> Option<ArtworkMetadata> {
    let file_type = fs::symlink_metadata(candidate).ok()?.file_type();
    if !file_type.is_file() {
        return None;
    }
    let canonical_path = candidate.canonicalize().ok()?;
    if !canonical_path.starts_with(canonical_root) {
        return None;
    }
    let metadata = fs::metadata(&canonical_path).ok()?;
    if metadata.len() == 0 || metadata.len() > MAX_ARTWORK_BYTES {
        return None;
    }
    image_mime(&canonical_path)?;
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |modified| modified.as_nanos());
    let revision = revision(&canonical_path, metadata.len(), modified_nanos);
    Some(ArtworkMetadata {
        source_path: canonical_path.to_str().map(str::to_owned),
        revision: Some(revision),
    })
}

pub fn read_chunk(
    source_path: &str,
    expected_revision: &str,
    allowed_roots: &[PathBuf],
    offset: u64,
) -> Result<ArtworkChunk, &'static str> {
    if expected_revision.is_empty()
        || expected_revision.len() > 20
        || !expected_revision.bytes().all(|byte| byte.is_ascii_digit())
        || !offset.is_multiple_of(ARTWORK_CHUNK_BYTES as u64)
        || offset > MAX_ARTWORK_BYTES
    {
        return Err("invalid_cursor");
    }

    let source_path = Path::new(source_path);
    let source_type = fs::symlink_metadata(source_path).map_err(|_| "artwork_unavailable")?;
    if !source_type.file_type().is_file() {
        return Err("artwork_unavailable");
    }
    let canonical_source = source_path
        .canonicalize()
        .map_err(|_| "artwork_unavailable")?;
    let canonical_roots = allowed_roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .take(MAX_ARTWORK_ROOTS)
        .collect::<Vec<_>>();
    if canonical_roots.is_empty()
        || !canonical_roots
            .iter()
            .any(|root| canonical_source.starts_with(root))
    {
        return Err("artwork_outside_roots");
    }

    let mut file = File::open(&canonical_source).map_err(|_| "artwork_unavailable")?;
    let file_metadata = file.metadata().map_err(|_| "artwork_unavailable")?;
    if !file_metadata.is_file()
        || file_metadata.len() == 0
        || file_metadata.len() > MAX_ARTWORK_BYTES
    {
        return Err("artwork_unavailable");
    }
    if source_path.canonicalize().map_err(|_| "artwork_changed")? != canonical_source {
        return Err("artwork_changed");
    }
    let file_revision = revision_for_metadata(&canonical_source, &file_metadata);
    if file_revision != expected_revision {
        return Err("artwork_changed");
    }

    let mut header = [0_u8; 12];
    let header_bytes = file.read(&mut header).map_err(|_| "artwork_unavailable")?;
    let mime_type = image_mime_from_header(&header[..header_bytes]).ok_or("unsupported_artwork")?;
    let total_bytes = file_metadata.len();
    if offset >= total_bytes {
        return Err("invalid_cursor");
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| "artwork_unavailable")?;
    let chunk_size = (total_bytes - offset).min(ARTWORK_CHUNK_BYTES as u64) as usize;
    let mut chunk = vec![0; chunk_size];
    file.read_exact(&mut chunk).map_err(|_| "artwork_changed")?;
    if revision_for_metadata(
        &canonical_source,
        &file.metadata().map_err(|_| "artwork_changed")?,
    ) != expected_revision
    {
        return Err("artwork_changed");
    }

    let next = offset + chunk_size as u64;
    Ok(ArtworkChunk {
        revision: expected_revision.to_owned(),
        total_bytes,
        next_offset: (next < total_bytes).then_some(next),
        mime_type,
        data: base64_encode(&chunk),
    })
}

fn revision_for_metadata(path: &Path, metadata: &fs::Metadata) -> String {
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |modified| modified.as_nanos());
    revision(path, metadata.len(), modified_nanos)
}

fn image_mime(path: &Path) -> Option<&'static str> {
    let mut file = File::open(path).ok()?;
    let mut header = [0_u8; 12];
    let bytes = file.read(&mut header).ok()?;
    image_mime_from_header(&header[..bytes])
}

fn image_mime_from_header(header: &[u8]) -> Option<&'static str> {
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

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied();
        let third = chunk.get(2).copied();
        encoded.push(ALPHABET[(first >> 2) as usize] as char);
        encoded
            .push(ALPHABET[(((first & 0x03) << 4) | (second.unwrap_or(0) >> 4)) as usize] as char);
        match second {
            Some(second) => {
                encoded.push(
                    ALPHABET[(((second & 0x0f) << 2) | (third.unwrap_or(0) >> 6)) as usize] as char,
                );
            }
            None => encoded.push('='),
        }
        match third {
            Some(third) => encoded.push(ALPHABET[(third & 0x3f) as usize] as char),
            None => encoded.push('='),
        }
    }
    encoded
}

fn revision(path: &Path, size: u64, modified_nanos: u128) -> String {
    let mut hash = 1_469_598_103_934_665_603_u64;
    for byte in path.to_string_lossy().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    for byte in size
        .to_le_bytes()
        .into_iter()
        .chain(modified_nanos.to_le_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    hash.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TemporaryRoot(PathBuf);

    impl TemporaryRoot {
        fn new() -> Self {
            let unique = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let now = std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "amaterasu-steam-artwork-{}-{now}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }

    impl Drop for TemporaryRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn prefers_local_portrait_and_changes_revision_when_source_changes() {
        let root = TemporaryRoot::new();
        let cache = root.0.join("appcache/librarycache");
        fs::create_dir_all(&cache).unwrap();
        let portrait = cache.join("42_library_600x900_2x.jpg");
        let header = cache.join("42_header.jpg");
        fs::write(&portrait, b"\x89PNG\r\n\x1a\nportrait").unwrap();
        fs::write(&header, b"\xff\xd8\xffheader").unwrap();

        let first = discover(42, &root.0, &[]);
        assert_eq!(first.source_path.as_deref(), portrait.to_str());
        assert!(first.revision.is_some());

        fs::write(&portrait, b"\x89PNG\r\n\x1a\nupdated portrait bytes").unwrap();
        let second = discover(42, &root.0, &[]);
        assert_eq!(second.source_path.as_deref(), portrait.to_str());
        assert_ne!(second.revision, first.revision);
    }

    #[test]
    fn falls_back_to_user_grid_and_ignores_unbounded_or_empty_sources() {
        let root = TemporaryRoot::new();
        let grid = root.0.join("userdata/100/config/grid");
        fs::create_dir_all(&grid).unwrap();
        fs::write(grid.join("42p.png"), b"\x89PNG\r\n\x1a\ncustom portrait").unwrap();

        let artwork = discover(42, &root.0, &[]);
        assert_eq!(
            artwork.source_path.as_deref(),
            grid.join("42p.png").to_str()
        );
        assert!(artwork.revision.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_artwork_symlinks_that_escape_the_steam_root() {
        use std::os::unix::fs::symlink;

        let root = TemporaryRoot::new();
        let outside = root.0.parent().unwrap().join(format!(
            "amaterasu-steam-artwork-outside-{}",
            std::process::id()
        ));
        fs::write(&outside, b"\x89PNG\r\n\x1a\noutside artwork").unwrap();
        let cache = root.0.join("appcache/librarycache");
        fs::create_dir_all(&cache).unwrap();
        symlink(&outside, cache.join("42_library_600x900_2x.jpg")).unwrap();

        assert_eq!(discover(42, &root.0, &[]), ArtworkMetadata::default());
        let _ = fs::remove_file(outside);
    }

    #[test]
    fn artwork_reads_are_fixed_chunks_bound_to_the_current_revision_and_mime() {
        let root = TemporaryRoot::new();
        let cache = root.0.join("appcache/librarycache");
        fs::create_dir_all(&cache).unwrap();
        let image = cache.join("42_library_600x900_2x.jpg");
        let mut bytes = vec![0xff, 0xd8, 0xff];
        bytes.resize(ARTWORK_CHUNK_BYTES + 3, 7);
        fs::write(&image, &bytes).unwrap();
        let discovered = discover(42, &root.0, &[]);
        let source_path = discovered.source_path.unwrap();
        let revision = discovered.revision.unwrap();

        let first = read_chunk(&source_path, &revision, std::slice::from_ref(&root.0), 0).unwrap();
        assert_eq!(first.mime_type, "image/jpeg");
        assert!(first.data.starts_with("/9j/"));
        assert_eq!(first.total_bytes, bytes.len() as u64);
        assert_eq!(first.next_offset, Some(ARTWORK_CHUNK_BYTES as u64));
        assert_eq!(first.data, base64_encode(&bytes[..ARTWORK_CHUNK_BYTES]));

        let last = read_chunk(
            &source_path,
            &revision,
            std::slice::from_ref(&root.0),
            ARTWORK_CHUNK_BYTES as u64,
        )
        .unwrap();
        assert_eq!(last.next_offset, None);
        assert_eq!(last.data, base64_encode(&bytes[ARTWORK_CHUNK_BYTES..]));
        assert_eq!(
            read_chunk(&source_path, &revision, std::slice::from_ref(&root.0), 1).unwrap_err(),
            "invalid_cursor"
        );
        assert_eq!(
            read_chunk(&source_path, "123", std::slice::from_ref(&root.0), 0).unwrap_err(),
            "artwork_changed"
        );
        assert_eq!(
            read_chunk(&source_path, &revision, &[], 0).unwrap_err(),
            "artwork_outside_roots"
        );
    }

    #[test]
    fn base64_encoder_matches_standard_padding() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
    }

    #[test]
    fn discovery_rejects_unsupported_and_oversized_image_files() {
        let root = TemporaryRoot::new();
        let cache = root.0.join("appcache/librarycache");
        fs::create_dir_all(&cache).unwrap();
        let candidate = cache.join("42_library_600x900_2x.jpg");
        fs::write(&candidate, b"not an image").unwrap();
        assert_eq!(discover(42, &root.0, &[]), ArtworkMetadata::default());

        fs::write(&candidate, b"\x89PNG\r\n\x1a\nimage").unwrap();
        let file = File::options().write(true).open(&candidate).unwrap();
        file.set_len(MAX_ARTWORK_BYTES + 1).unwrap();
        assert_eq!(discover(42, &root.0, &[]), ArtworkMetadata::default());
    }
}
