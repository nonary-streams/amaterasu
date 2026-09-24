//! Bounded local Steam artwork discovery for catalog synchronization.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, UNIX_EPOCH};
use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig};

const MAX_ARTWORK_ROOTS: usize = 4;
const MAX_USER_DIRECTORIES: usize = 32;
const MAX_HASH_DIRECTORIES: usize = 32;
pub const MAX_ARTWORK_BYTES: u64 = 8 * 1024 * 1024;
const MAX_REMOTE_CACHE_IMAGES: usize = 4;
const MAX_REMOTE_FAILURES: usize = 64;
const REMOTE_FETCH_QUEUE_CAPACITY: usize = 1;
const REMOTE_FAILURE_COOLDOWN: Duration = Duration::from_secs(10 * 60);
const REMOTE_FETCH_TIMEOUT: Duration = Duration::from_secs(8);
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

#[derive(Clone)]
struct CachedRemoteArtwork {
    app_id: u32,
    revision: String,
    bytes: Vec<u8>,
}

#[derive(Default)]
pub struct RemoteArtworkCache {
    images: VecDeque<CachedRemoteArtwork>,
    failed_until: BTreeMap<u32, Instant>,
}

pub struct RemoteArtworkRequest {
    pub request_id: u64,
    pub app_id: u32,
    pub last_updated: u64,
    pub revision: String,
    pub offset: u64,
    pub deadline: Instant,
}

pub struct RemoteArtworkCompletion {
    pub request_id: u64,
    pub app_id: u32,
    pub revision: String,
    pub offset: u64,
    pub result: Result<ArtworkChunk, &'static str>,
}

#[derive(Default)]
pub struct RemoteArtworkWorker {
    requests: Option<mpsc::SyncSender<RemoteArtworkRequest>>,
    completions: Option<mpsc::Receiver<RemoteArtworkCompletion>>,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Stable opaque revision for the fixed Steam CDN portrait URL. Including the
/// manifest's last-update stamp changes the reference when Steam replaces the
/// installed app metadata, without exposing a URL or local path to the catalog.
pub fn remote_revision(app_id: u32, last_updated: u64) -> Option<String> {
    (app_id != 0).then(|| {
        let identity = format!("steam-cdn:{app_id}:{last_updated}");
        revision(Path::new(&identity), 0, 0)
    })
}

fn remote_portrait_url(app_id: u32) -> Option<String> {
    (app_id != 0).then(|| {
        format!(
            "https://shared.fastly.steamstatic.com/store_item_assets/steam/apps/{app_id}/library_600x900_2x.jpg"
        )
    })
}

impl RemoteArtworkCache {
    fn read_chunk_with(
        &mut self,
        app_id: u32,
        last_updated: u64,
        expected_revision: &str,
        offset: u64,
        now: Instant,
        fetcher: impl FnOnce(u32) -> Result<Vec<u8>, &'static str>,
    ) -> Result<ArtworkChunk, &'static str> {
        if app_id == 0 {
            return Err("artwork_unavailable");
        }
        if !offset.is_multiple_of(ARTWORK_CHUNK_BYTES as u64) || offset > MAX_ARTWORK_BYTES {
            return Err("invalid_cursor");
        }
        let revision = remote_revision(app_id, last_updated).ok_or("artwork_unavailable")?;
        if expected_revision != revision {
            return Err("artwork_changed");
        }

        if let Some(index) = self
            .images
            .iter()
            .position(|image| image.app_id == app_id && image.revision == revision)
        {
            let image = self
                .images
                .remove(index)
                .expect("located cache item exists");
            let result = chunk_from_bytes(&image.bytes, &image.revision, offset);
            self.images.push_back(image);
            return result;
        }

        if self
            .failed_until
            .get(&app_id)
            .is_some_and(|failed_until| now < *failed_until)
        {
            return Err("artwork_unavailable");
        }
        self.failed_until.remove(&app_id);

        let bytes = match fetcher(app_id).and_then(|bytes| {
            validate_remote_artwork(&bytes)
                .then_some(bytes)
                .ok_or("unsupported_artwork")
        }) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.remember_failure(app_id, now);
                return Err(error);
            }
        };
        if self.images.len() == MAX_REMOTE_CACHE_IMAGES {
            self.images.pop_front();
        }
        self.failed_until.remove(&app_id);
        self.images.push_back(CachedRemoteArtwork {
            app_id,
            revision: revision.clone(),
            bytes,
        });
        let image = self.images.back().expect("new cache item exists");
        chunk_from_bytes(&image.bytes, &image.revision, offset)
    }

    fn remember_failure(&mut self, app_id: u32, now: Instant) {
        if !self.failed_until.contains_key(&app_id)
            && self.failed_until.len() >= MAX_REMOTE_FAILURES
        {
            self.failed_until.pop_first();
        }
        self.failed_until
            .insert(app_id, now + REMOTE_FAILURE_COOLDOWN);
    }
}

impl RemoteArtworkWorker {
    pub fn enqueue(&mut self, request: RemoteArtworkRequest) -> Result<(), &'static str> {
        self.start_if_needed()?;
        let Some(sender) = &self.requests else {
            return Err("artwork_unavailable");
        };
        sender.try_send(request).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => "artwork_busy",
            mpsc::TrySendError::Disconnected(_) => "artwork_unavailable",
        })
    }

    pub fn drain(&mut self) -> Vec<RemoteArtworkCompletion> {
        let Some(receiver) = &self.completions else {
            return Vec::new();
        };
        receiver.try_iter().collect()
    }

    fn start_if_needed(&mut self) -> Result<(), &'static str> {
        self.start_with_fetcher(fetch_remote)
    }

    fn start_with_fetcher(
        &mut self,
        fetcher: impl Fn(u32) -> Result<Vec<u8>, &'static str> + Send + 'static,
    ) -> Result<(), &'static str> {
        if self.requests.is_some() {
            return Ok(());
        }

        let (request_sender, request_receiver) =
            mpsc::sync_channel::<RemoteArtworkRequest>(REMOTE_FETCH_QUEUE_CAPACITY);
        let (completion_sender, completion_receiver) =
            mpsc::sync_channel(REMOTE_FETCH_QUEUE_CAPACITY + 1);
        let stopped = Arc::clone(&self.stopped);
        let worker_thread = thread::Builder::new()
            .name("steam-artwork-fetch".into())
            .spawn(move || {
                let mut cache = RemoteArtworkCache::default();
                while let Ok(request) = request_receiver.recv() {
                    if stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let result = if Instant::now() >= request.deadline {
                        Err("request_ended")
                    } else {
                        cache.read_chunk_with(
                            request.app_id,
                            request.last_updated,
                            &request.revision,
                            request.offset,
                            Instant::now(),
                            &fetcher,
                        )
                    };
                    let result = if Instant::now() >= request.deadline {
                        Err("request_ended")
                    } else {
                        result
                    };
                    if completion_sender
                        .send(RemoteArtworkCompletion {
                            request_id: request.request_id,
                            app_id: request.app_id,
                            revision: request.revision,
                            offset: request.offset,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|_| "artwork_unavailable")?;
        self.requests = Some(request_sender);
        self.completions = Some(completion_receiver);
        self.thread = Some(worker_thread);
        Ok(())
    }
}

impl Drop for RemoteArtworkWorker {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.completions.take();
        self.requests.take();
        if let Some(worker_thread) = self.thread.take() {
            let _ = worker_thread.join();
        }
    }
}

fn chunk_from_bytes(
    bytes: &[u8],
    revision: &str,
    offset: u64,
) -> Result<ArtworkChunk, &'static str> {
    let mime_type =
        image_mime_from_header(bytes.get(..12).unwrap_or(bytes)).ok_or("unsupported_artwork")?;
    let total_bytes = bytes.len() as u64;
    if offset >= total_bytes {
        return Err("invalid_cursor");
    }
    let start = usize::try_from(offset).map_err(|_| "invalid_cursor")?;
    let end = (start + ARTWORK_CHUNK_BYTES).min(bytes.len());
    Ok(ArtworkChunk {
        revision: revision.to_owned(),
        total_bytes,
        next_offset: (end < bytes.len()).then_some(end as u64),
        mime_type,
        data: base64_encode(&bytes[start..end]),
    })
}

fn validate_remote_artwork(bytes: &[u8]) -> bool {
    bytes.len() <= MAX_ARTWORK_BYTES as usize
        && image_mime_from_header(bytes.get(..12).unwrap_or(bytes)) == Some("image/jpeg")
        && jpeg_dimensions(bytes).is_some_and(|(width, height)| {
            width >= 600 && height >= 900 && u64::from(width) * 3 == u64::from(height) * 2
        })
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || !bytes.starts_with(&[0xff, 0xd8]) || !bytes.ends_with(&[0xff, 0xd9]) {
        return None;
    }
    let mut offset = 2;
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xff {
            return None;
        }
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        let marker = *bytes.get(offset)?;
        offset += 1;
        if marker == 0xd9 || marker == 0xda {
            return None;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let length = usize::from(u16::from_be_bytes([
            *bytes.get(offset)?,
            *bytes.get(offset + 1)?,
        ]));
        if length < 2 || offset.checked_add(length)? > bytes.len() {
            return None;
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            if length < 8 {
                return None;
            }
            let height = u32::from(u16::from_be_bytes([
                *bytes.get(offset + 3)?,
                *bytes.get(offset + 4)?,
            ]));
            let width = u32::from(u16::from_be_bytes([
                *bytes.get(offset + 5)?,
                *bytes.get(offset + 6)?,
            ]));
            return (width > 0 && height > 0).then_some((width, height));
        }
        offset += length;
    }
    None
}

fn fetch_remote(app_id: u32) -> Result<Vec<u8>, &'static str> {
    let url = remote_portrait_url(app_id).ok_or("artwork_unavailable")?;
    let response = remote_agent()
        .get(&url)
        .header("accept", "image/jpeg")
        .header("accept-encoding", "identity")
        .call()
        .map_err(|_| "artwork_unavailable")?;
    if response.status() != 200 {
        return Err("artwork_unavailable");
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if content_type != Some("image/jpeg") {
        return Err("unsupported_artwork");
    }
    if response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_ARTWORK_BYTES)
    {
        return Err("unsupported_artwork");
    }

    let mut body = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(MAX_ARTWORK_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|_| "artwork_unavailable")?;
    if !validate_remote_artwork(&body) {
        return Err("unsupported_artwork");
    }
    Ok(body)
}

fn remote_agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let config = Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .timeout_global(Some(REMOTE_FETCH_TIMEOUT))
            .max_response_header_size(8 * 1024)
            .input_buffer_size(8 * 1024)
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build();
        Agent::new_with_config(config)
    })
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

    fn synthetic_jpeg(width: u16, height: u16, total_bytes: usize) -> Vec<u8> {
        let mut bytes = vec![
            0xff,
            0xd8,
            0xff,
            0xc0,
            0x00,
            0x11,
            0x08,
            (height >> 8) as u8,
            height as u8,
            (width >> 8) as u8,
            width as u8,
            0x03,
            0x01,
            0x11,
            0x00,
            0x02,
            0x11,
            0x00,
            0x03,
            0x11,
            0x00,
        ];
        bytes.resize(total_bytes.saturating_sub(2).max(bytes.len()), 0x7f);
        bytes.extend_from_slice(&[0xff, 0xd9]);
        bytes
    }

    #[test]
    fn remote_artwork_revision_and_url_are_stable_and_derived_only_from_app_id() {
        assert_eq!(
            remote_portrait_url(42).as_deref(),
            Some(
                "https://shared.fastly.steamstatic.com/store_item_assets/steam/apps/42/library_600x900_2x.jpg"
            )
        );
        assert_eq!(remote_portrait_url(0), None);
        let initial = remote_revision(42, 123).unwrap();
        assert_eq!(remote_revision(42, 123).as_deref(), Some(initial.as_str()));
        assert_ne!(remote_revision(42, 124).as_deref(), Some(initial.as_str()));
        assert!(initial.len() <= 20);
        assert!(initial.bytes().all(|byte| byte.is_ascii_digit()));
    }

    #[test]
    fn remote_artwork_cache_fetches_once_and_serves_the_same_chunk_contract() {
        let bytes = synthetic_jpeg(600, 900, ARTWORK_CHUNK_BYTES + 19);
        let revision = remote_revision(42, 123).unwrap();
        let mut cache = RemoteArtworkCache::default();
        let now = Instant::now();
        let first = cache
            .read_chunk_with(42, 123, &revision, 0, now, |_| Ok(bytes.clone()))
            .unwrap();
        assert_eq!(first.mime_type, "image/jpeg");
        assert_eq!(first.total_bytes, bytes.len() as u64);
        assert_eq!(first.next_offset, Some(ARTWORK_CHUNK_BYTES as u64));
        assert_eq!(first.data, base64_encode(&bytes[..ARTWORK_CHUNK_BYTES]));

        let last = cache
            .read_chunk_with(42, 123, &revision, ARTWORK_CHUNK_BYTES as u64, now, |_| {
                panic!("cache hit must not fetch again")
            })
            .unwrap();
        assert_eq!(last.next_offset, None);
        assert_eq!(last.data, base64_encode(&bytes[ARTWORK_CHUNK_BYTES..]));
        assert_eq!(last.revision, revision);
    }

    #[test]
    fn stale_revisions_and_invalid_offsets_never_call_the_remote_fetcher() {
        let revision = remote_revision(42, 123).unwrap();
        let mut cache = RemoteArtworkCache::default();
        assert_eq!(
            cache
                .read_chunk_with(42, 123, "1", 0, Instant::now(), |_| {
                    panic!("stale revision must not fetch")
                })
                .unwrap_err(),
            "artwork_changed"
        );
        assert_eq!(
            cache
                .read_chunk_with(42, 123, &revision, 1, Instant::now(), |_| {
                    panic!("unaligned cursor must not fetch")
                })
                .unwrap_err(),
            "invalid_cursor"
        );
    }

    #[test]
    fn failed_remote_artwork_uses_a_bounded_cooldown_then_allows_retry() {
        let revision = remote_revision(42, 123).unwrap();
        let mut cache = RemoteArtworkCache::default();
        let now = Instant::now();
        assert_eq!(
            cache
                .read_chunk_with(42, 123, &revision, 0, now, |_| Err("artwork_unavailable"))
                .unwrap_err(),
            "artwork_unavailable"
        );
        assert_eq!(
            cache
                .read_chunk_with(
                    42,
                    123,
                    &revision,
                    0,
                    now + Duration::from_secs(2),
                    |_| panic!("cooldown must suppress retries"),
                )
                .unwrap_err(),
            "artwork_unavailable"
        );
        let bytes = synthetic_jpeg(600, 900, 64);
        assert!(
            cache
                .read_chunk_with(
                    42,
                    123,
                    &revision,
                    0,
                    now + REMOTE_FAILURE_COOLDOWN + Duration::from_millis(1),
                    |_| Ok(bytes),
                )
                .is_ok()
        );
        assert!(cache.failed_until.len() <= MAX_REMOTE_FAILURES);
    }

    #[test]
    fn remote_jpeg_validation_rejects_wrong_dimensions_and_oversized_data() {
        assert!(validate_remote_artwork(&synthetic_jpeg(600, 900, 64)));
        assert!(!validate_remote_artwork(&synthetic_jpeg(599, 900, 64)));
        assert!(!validate_remote_artwork(&synthetic_jpeg(600, 899, 64)));
        assert!(!validate_remote_artwork(&synthetic_jpeg(
            600,
            900,
            MAX_ARTWORK_BYTES as usize + 1
        )));
    }

    #[test]
    fn artwork_fetch_runs_off_the_calling_thread() {
        let mut worker = RemoteArtworkWorker::default();
        let bytes = synthetic_jpeg(600, 900, 64);
        worker
            .start_with_fetcher(move |_| {
                thread::sleep(Duration::from_millis(250));
                Ok(bytes.clone())
            })
            .unwrap();
        let revision = remote_revision(42, 123).unwrap();
        let started = Instant::now();
        worker
            .enqueue(RemoteArtworkRequest {
                request_id: 7,
                app_id: 42,
                last_updated: 123,
                revision: revision.clone(),
                offset: 0,
                deadline: started + Duration::from_secs(2),
            })
            .unwrap();
        assert!(started.elapsed() < Duration::from_millis(150));

        for _ in 0..100 {
            if let Some(completion) = worker.drain().into_iter().next() {
                assert_eq!(completion.request_id, 7);
                assert_eq!(completion.app_id, 42);
                assert_eq!(completion.revision, revision);
                assert_eq!(completion.result.unwrap().mime_type, "image/jpeg");
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("background artwork fetch did not complete");
    }

    #[test]
    fn artwork_worker_queue_is_bounded_while_a_fetch_is_running() {
        let mut worker = RemoteArtworkWorker::default();
        let bytes = synthetic_jpeg(600, 900, 64);
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let release_receiver = Arc::new(std::sync::Mutex::new(release_receiver));
        let fetch_release = Arc::clone(&release_receiver);
        worker
            .start_with_fetcher(move |_| {
                started_sender.send(()).unwrap();
                fetch_release.lock().unwrap().recv().unwrap();
                Ok(bytes.clone())
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let request = |request_id| {
            let app_id = 41 + u32::try_from(request_id).unwrap();
            RemoteArtworkRequest {
                request_id,
                app_id,
                last_updated: 123,
                revision: remote_revision(app_id, 123).unwrap(),
                offset: 0,
                deadline,
            }
        };

        worker.enqueue(request(1)).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker should start the first fetch");
        worker.enqueue(request(2)).unwrap();
        assert_eq!(worker.enqueue(request(3)).unwrap_err(), "artwork_busy");

        release_sender.send(()).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker should process its one queued request");
        release_sender.send(()).unwrap();
        let mut completed = 0;
        for _ in 0..100 {
            completed += worker.drain().len();
            if completed == 2 {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("bounded artwork jobs did not complete");
    }

    #[test]
    fn expired_artwork_requests_do_not_start_network_fetches() {
        let mut worker = RemoteArtworkWorker::default();
        worker
            .start_with_fetcher(|_| panic!("expired request must not fetch"))
            .unwrap();
        worker
            .enqueue(RemoteArtworkRequest {
                request_id: 9,
                app_id: 42,
                last_updated: 123,
                revision: remote_revision(42, 123).unwrap(),
                offset: 0,
                deadline: Instant::now() - Duration::from_millis(1),
            })
            .unwrap();
        for _ in 0..100 {
            if let Some(completion) = worker.drain().into_iter().next() {
                assert_eq!(completion.request_id, 9);
                assert_eq!(completion.result.unwrap_err(), "request_ended");
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("expired artwork request did not complete");
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
