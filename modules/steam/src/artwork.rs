//! Bounded local Steam artwork discovery for catalog synchronization.
use image::codecs::png::PngEncoder;
use image::{ImageEncoder, ImageFormat, ImageReader, Limits};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig};

const MAX_ARTWORK_ROOTS: usize = 4;
const MAX_USER_DIRECTORIES: usize = 32;
const MAX_HASH_DIRECTORIES: usize = 32;
const MAX_DIMENSION_HEADER_BYTES: usize = 64 * 1024;
pub const MAX_ARTWORK_BYTES: u64 = 16 * 1024 * 1024;
// Bound the pixel buffer an image decoder would allocate after the compressed
// transfer has passed its byte limit (four bytes per pixel).
const MAX_DECODED_ARTWORK_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARTWORK_DIMENSION: u32 = 8192;
const MAX_REMOTE_CACHE_IMAGES: usize = 4;
const REMOTE_CACHE_DIRECTORY: &str = "amaterasu-steam-artwork-v1";
const MAX_TEMP_NAME_ATTEMPTS: u64 = 8;
const MAX_REMOTE_CACHE_TEMP_FILES: usize = 64;
const MAX_REMOTE_CACHE_DIRECTORY_ENTRIES: usize = 1024;
const REMOTE_TEMP_GRACE: Duration = Duration::from_secs(60 * 60);
static NEXT_REMOTE_TEMP: AtomicU64 = AtomicU64::new(0);
const MAX_REMOTE_FAILURES: usize = 64;
const REMOTE_FETCH_QUEUE_CAPACITY: usize = 1;
const REMOTE_FAILURE_COOLDOWN: Duration = Duration::from_secs(10 * 60);
const REMOTE_FETCH_TIMEOUT: Duration = Duration::from_secs(8);
pub const ARTWORK_CHUNK_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtworkMetadata {
    pub source_path: Option<String>,
    pub revision: Option<String>,
    pub candidate_signature: String,
    pub needs_remote_upgrade: bool,
}

struct LocalArtworkCandidate {
    metadata: ArtworkMetadata,
    canonical_path: PathBuf,
    byte_length: u64,
    modified_nanos: u128,
    role_priority: u8,
    pixel_area: u64,
    width: u32,
    height: u32,
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

struct CachedLocalArtwork {
    source_path: PathBuf,
    revision: String,
    bytes: Vec<u8>,
}

#[derive(Default)]
pub struct LocalArtworkCache {
    images: VecDeque<CachedLocalArtwork>,
}

#[derive(Default)]
pub struct RemoteArtworkCache {
    images: VecDeque<CachedRemoteArtwork>,
    failed_until: BTreeMap<u32, Instant>,
    disk_root: Option<PathBuf>,
}

pub struct RemoteArtworkRequest {
    pub prefetch: bool,
    pub generation: u64,
    pub request_id: u64,
    pub app_id: u32,
    pub last_updated: u64,
    pub revision: String,
    pub offset: u64,
    pub deadline: Instant,
}

pub struct RemoteArtworkCompletion {
    pub prefetch: bool,
    pub generation: u64,
    pub request_id: u64,
    pub app_id: u32,
    pub revision: String,
    pub offset: u64,
    pub result: Result<ArtworkChunk, &'static str>,
}

pub struct LocalArtworkTarget {
    pub app_id: u32,
    pub library: PathBuf,
}

pub enum LocalArtworkRequest {
    Validate {
        generation: u64,
        roots: Vec<PathBuf>,
        targets: Vec<LocalArtworkTarget>,
    },
    Read {
        generation: u64,
        request_id: u64,
        app_id: u32,
        source_path: String,
        revision: String,
        allowed_roots: Vec<PathBuf>,
        offset: u64,
        deadline: Instant,
    },
}

pub enum LocalArtworkCompletion {
    Validated {
        generation: u64,
        artwork: BTreeMap<u32, ArtworkMetadata>,
        complete: bool,
    },
    Read {
        generation: u64,
        request_id: u64,
        app_id: u32,
        revision: String,
        offset: u64,
        result: Result<ArtworkChunk, &'static str>,
    },
}

#[derive(Default)]
pub struct LocalArtworkWorker {
    requests: Option<mpsc::SyncSender<LocalArtworkRequest>>,
    completions: Option<mpsc::Receiver<LocalArtworkCompletion>>,
    stopped: Arc<AtomicBool>,
    current_generation: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl LocalArtworkWorker {
    pub fn set_generation(&self, generation: u64) {
        self.current_generation.store(generation, Ordering::Release);
    }

    pub fn enqueue(&mut self, request: LocalArtworkRequest) -> Result<(), &'static str> {
        self.start_if_needed()?;
        self.requests
            .as_ref()
            .ok_or("artwork_unavailable")?
            .try_send(request)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "artwork_busy",
                mpsc::TrySendError::Disconnected(_) => "artwork_unavailable",
            })
    }

    pub fn drain(&mut self) -> Vec<LocalArtworkCompletion> {
        self.completions
            .as_ref()
            .map_or_else(Vec::new, |receiver| receiver.try_iter().collect())
    }

    fn start_if_needed(&mut self) -> Result<(), &'static str> {
        if self.requests.is_some() {
            return Ok(());
        }
        let (request_sender, request_receiver) = mpsc::sync_channel(1);
        let (completion_sender, completion_receiver) = mpsc::sync_channel(2);
        let stopped = Arc::clone(&self.stopped);
        let current_generation = Arc::clone(&self.current_generation);
        let worker_thread = thread::Builder::new()
            .name("steam-artwork-local".into())
            .spawn(move || {
                let mut cache = LocalArtworkCache::default();
                while let Ok(request) = request_receiver.recv() {
                    if stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let completion = match request {
                        LocalArtworkRequest::Validate {
                            generation,
                            roots,
                            targets,
                        } => {
                            let mut artwork = BTreeMap::new();
                            let deadline = Instant::now() + Duration::from_secs(15);
                            let target_count = targets.len();
                            for target in targets {
                                if stopped.load(Ordering::Acquire)
                                    || current_generation.load(Ordering::Acquire) != generation
                                    || Instant::now() >= deadline
                                {
                                    break;
                                }
                                artwork.insert(
                                    target.app_id,
                                    discover_with_validation(
                                        target.app_id,
                                        &target.library,
                                        &roots,
                                        Some(&mut cache),
                                        Some(&|| {
                                            stopped.load(Ordering::Acquire)
                                                || current_generation.load(Ordering::Acquire)
                                                    != generation
                                                || Instant::now() >= deadline
                                        }),
                                    ),
                                );
                            }
                            LocalArtworkCompletion::Validated {
                                generation,
                                complete: artwork.len() == target_count
                                    && Instant::now() < deadline,
                                artwork,
                            }
                        }
                        LocalArtworkRequest::Read {
                            generation,
                            request_id,
                            app_id,
                            source_path,
                            revision,
                            allowed_roots,
                            offset,
                            deadline,
                        } => {
                            let result = if Instant::now() >= deadline
                                || current_generation.load(Ordering::Acquire) != generation
                            {
                                Err("request_ended")
                            } else {
                                read_chunk_cached(
                                    &mut cache,
                                    &source_path,
                                    &revision,
                                    &allowed_roots,
                                    offset,
                                )
                            };
                            let result = if Instant::now() >= deadline {
                                Err("request_ended")
                            } else {
                                result
                            };
                            LocalArtworkCompletion::Read {
                                generation,
                                request_id,
                                app_id,
                                revision,
                                offset,
                                result,
                            }
                        }
                    };
                    if completion_sender.send(completion).is_err() {
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

impl Drop for LocalArtworkWorker {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.completions.take();
        self.requests.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
pub struct RemoteArtworkWorker {
    requests: Option<mpsc::SyncSender<RemoteArtworkRequest>>,
    completions: Option<mpsc::Receiver<RemoteArtworkCompletion>>,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    cache_root: Option<PathBuf>,
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
    fn with_disk_root(root: Option<PathBuf>) -> Self {
        Self {
            disk_root: root.and_then(|root| {
                if !regular_directory(&root) {
                    return None;
                }
                let parent = root.canonicalize().ok()?;
                let child = parent.join(REMOTE_CACHE_DIRECTORY);
                if !child.exists() && fs::create_dir(&child).is_err() {
                    return None;
                }
                (regular_directory(&child) && child.canonicalize().ok().as_ref() == Some(&child))
                    .then_some(child)
            }),
            ..Self::default()
        }
    }

    fn disk_directory_valid(&self) -> bool {
        self.disk_root.as_ref().is_some_and(|root| {
            regular_directory(root)
                && root.canonicalize().ok().as_ref() == Some(root)
                && root.parent().is_some_and(regular_directory)
        })
    }

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

        if let Some(bytes) = self.read_disk(app_id, &revision) {
            self.failed_until.remove(&app_id);
            return self.remember_image(app_id, revision, bytes, offset);
        }

        if self
            .failed_until
            .get(&app_id)
            .is_some_and(|failed_until| now < *failed_until)
        {
            return Err("artwork_unavailable");
        }
        self.failed_until.remove(&app_id);

        let bytes = match fetcher(app_id).and_then(|bytes| convert_remote_jpeg(&bytes)) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.remember_failure(app_id, now);
                return Err(error);
            }
        };
        self.write_disk(app_id, &revision, &bytes);
        self.failed_until.remove(&app_id);
        self.remember_image(app_id, revision, bytes, offset)
    }

    fn remember_image(
        &mut self,
        app_id: u32,
        revision: String,
        bytes: Vec<u8>,
        offset: u64,
    ) -> Result<ArtworkChunk, &'static str> {
        if self.images.len() == MAX_REMOTE_CACHE_IMAGES {
            self.images.pop_front();
        }
        self.images.push_back(CachedRemoteArtwork {
            app_id,
            revision,
            bytes,
        });
        let image = self.images.back().expect("new cache item exists");
        chunk_from_bytes(&image.bytes, &image.revision, offset)
    }

    fn disk_path(&self, app_id: u32, revision: &str) -> Option<PathBuf> {
        if !self.disk_directory_valid()
            || app_id == 0
            || revision.is_empty()
            || revision.len() > 20
            || !revision.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        Some(
            self.disk_root
                .as_ref()?
                .join(format!("{app_id}-{revision}.png")),
        )
    }

    fn read_disk(&self, app_id: u32, revision: &str) -> Option<Vec<u8>> {
        let path = self.disk_path(app_id, revision)?;
        if path.canonicalize().ok()? != path {
            return None;
        }
        let metadata = fs::symlink_metadata(&path).ok()?;
        if !metadata.is_file()
            || is_link_or_reparse(&metadata)
            || metadata.len() > MAX_ARTWORK_BYTES
        {
            return None;
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        let file = File::open(&path).ok()?;
        let opened = file.metadata().ok()?;
        if !opened.is_file()
            || opened.len() != metadata.len()
            || opened.modified().ok() != metadata.modified().ok()
        {
            return None;
        }
        file.take(MAX_ARTWORK_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() as u64 == opened.len()
            && path.canonicalize().ok().as_ref() == Some(&path)
            && self.disk_directory_valid()
            && validate_cached_png(&bytes))
        .then_some(bytes)
    }

    fn write_disk(&self, app_id: u32, revision: &str, bytes: &[u8]) {
        let Some(path) = self.disk_path(app_id, revision) else {
            return;
        };
        if !validate_cached_png(bytes) {
            return;
        }
        if !self.reclaim_or_bound_temporary_files() {
            return;
        }
        // Keep each attempt private to this provider-owned child. A crashed
        // writer's old temporary name cannot permanently block future writes.
        let Some((temporary, mut file)) = self.create_temporary(&path) else {
            return;
        };
        let written = file.write_all(bytes).and_then(|_| file.sync_all()).is_ok();
        drop(file);
        if written && self.can_replace_entry(&path) {
            let _ = fs::rename(&temporary, &path);
        }
        let _ = fs::remove_file(temporary);
    }

    fn create_temporary(&self, entry: &Path) -> Option<(PathBuf, File)> {
        let clock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos();
        for _ in 0..MAX_TEMP_NAME_ATTEMPTS {
            let serial = NEXT_REMOTE_TEMP.fetch_add(1, Ordering::Relaxed);
            let temporary =
                entry.with_extension(format!("{}.{}.{}.tmp", std::process::id(), clock, serial));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Ok(file) => return Some((temporary, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return None,
            }
        }
        None
    }

    fn reclaim_or_bound_temporary_files(&self) -> bool {
        let Some(root) = &self.disk_root else {
            return false;
        };
        if !self.disk_directory_valid() {
            return false;
        }
        let Ok(entries) = fs::read_dir(root) else {
            return false;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut temporary_count = 0;
        for (index, entry) in entries.enumerate() {
            if index >= MAX_REMOTE_CACHE_DIRECTORY_ENTRIES {
                return false;
            }
            let Ok(entry) = entry else {
                return false;
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.ends_with(".tmp") {
                continue;
            }
            let Some(created_at) = owned_temporary_timestamp(name) else {
                // Unknown temporary files are not ours to delete, but they
                // count against the bound on writes into this child.
                temporary_count += 1;
                continue;
            };
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                return false;
            };
            if metadata.is_file()
                && !is_link_or_reparse(&metadata)
                && path.canonicalize().ok().as_ref() == Some(&path)
                && now.saturating_sub(created_at) >= REMOTE_TEMP_GRACE.as_nanos()
            {
                if !self.disk_directory_valid() || fs::remove_file(&path).is_err() {
                    return false;
                }
            } else {
                temporary_count += 1;
            }
        }
        temporary_count < MAX_REMOTE_CACHE_TEMP_FILES
    }

    fn can_replace_entry(&self, path: &Path) -> bool {
        if !self.disk_directory_valid() {
            return false;
        }
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                // Only an ordinary entry inside our private child can be
                // replaced. A linked/reparse entry is left untouched.
                metadata.is_file()
                    && !is_link_or_reparse(&metadata)
                    && path.canonicalize().ok().as_deref() == Some(path)
                    && self.read_disk_entry_is_invalid(path)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self.make_disk_room(),
            Err(_) => false,
        }
    }

    fn read_disk_entry_is_invalid(&self, path: &Path) -> bool {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            return false;
        };
        if !metadata.is_file() || is_link_or_reparse(&metadata) {
            return false;
        }
        if metadata.len() > MAX_ARTWORK_BYTES {
            return true;
        }
        let Ok(file) = File::open(path) else {
            return false;
        };
        let mut bytes = Vec::new();
        if file
            .take(MAX_ARTWORK_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return false;
        }
        !validate_cached_png(&bytes)
    }

    fn make_disk_room(&self) -> bool {
        let Some(root) = &self.disk_root else {
            return false;
        };
        let Ok(entries) = fs::read_dir(root) else {
            return false;
        };
        let mut images = Vec::new();
        for (index, entry) in entries.enumerate() {
            if index >= MAX_REMOTE_CACHE_DIRECTORY_ENTRIES {
                return false;
            }
            let Ok(entry) = entry else {
                return false;
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            // Older releases stored the same opaque key as JPEG. Count those
            // entries during eviction so conversion does not grow this child.
            let Some(stem) = name
                .strip_suffix(".png")
                .or_else(|| name.strip_suffix(".jpg"))
            else {
                continue;
            };
            let Some((app_id, revision)) = stem.split_once('-') else {
                continue;
            };
            if app_id.parse::<u32>().ok().filter(|id| *id != 0).is_none()
                || revision.is_empty()
                || revision.len() > 20
                || !revision.bytes().all(|byte| byte.is_ascii_digit())
            {
                continue;
            }
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                return false;
            };
            if metadata.is_file()
                && !is_link_or_reparse(&metadata)
                && entry.path().canonicalize().ok().as_ref() == Some(&entry.path())
            {
                images.push((metadata.modified().unwrap_or(UNIX_EPOCH), entry.path()));
            }
        }
        images.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        while images.len() >= MAX_REMOTE_CACHE_IMAGES {
            if !self.disk_directory_valid() {
                return false;
            }
            if fs::remove_file(images.remove(0).1).is_err() {
                return false;
            }
        }
        true
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

fn owned_temporary_timestamp(name: &str) -> Option<u128> {
    let stem = name.strip_suffix(".tmp")?;
    let mut parts = stem.split('.');
    let (app_id, revision) = parts.next()?.split_once('-')?;
    let pid = parts.next()?;
    let clock = parts.next()?;
    let serial = parts.next()?;
    if parts.next().is_some()
        || app_id
            .parse::<u32>()
            .ok()
            .filter(|id| *id != 0)?
            .to_string()
            != app_id
        || revision.is_empty()
        || revision.len() > 20
        || !revision.bytes().all(|byte| byte.is_ascii_digit())
        || pid.parse::<u32>().ok()?.to_string() != pid
        || serial.parse::<u64>().ok()?.to_string() != serial
    {
        return None;
    }
    let timestamp = clock.parse::<u128>().ok()?;
    (timestamp.to_string() == clock).then_some(timestamp)
}

fn regular_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_dir() && !is_link_or_reparse(&metadata))
}

fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

impl RemoteArtworkWorker {
    pub fn set_cache_root(&mut self, root: Option<PathBuf>) {
        self.cache_root = root;
    }

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

    pub(crate) fn start_with_fetcher(
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
        let cache_root = self.cache_root.clone();
        let worker_thread = thread::Builder::new()
            .name("steam-artwork-fetch".into())
            .spawn(move || {
                let mut cache = RemoteArtworkCache::with_disk_root(cache_root);
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
                            prefetch: request.prefetch,
                            generation: request.generation,
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
            width >= 600
                && height >= 900
                && u64::from(width) * 3 == u64::from(height) * 2
                && decoded_size_is_bounded(width, height)
        })
}

fn convert_remote_jpeg(bytes: &[u8]) -> Result<Vec<u8>, &'static str> {
    if !validate_remote_artwork(bytes) {
        return Err("unsupported_artwork");
    }
    convert_to_png(bytes, ImageFormat::Jpeg)
}

fn validate_cached_png(bytes: &[u8]) -> bool {
    if bytes.is_empty()
        || bytes.len() as u64 > MAX_ARTWORK_BYTES
        || image_mime_from_header(bytes) != Some("image/png")
    {
        return false;
    }
    let Some((width, height)) = png_dimensions(bytes) else {
        return false;
    };
    width >= 600
        && height >= 900
        && u64::from(width) * 3 == u64::from(height) * 2
        && decoded_size_is_bounded(width, height)
        && decode_image(bytes, ImageFormat::Png).is_ok()
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    (bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") && &bytes[12..16] == b"IHDR")
        .then(|| {
            (
                u32::from_be_bytes(bytes[16..20].try_into().expect("four bytes")),
                u32::from_be_bytes(bytes[20..24].try_into().expect("four bytes")),
            )
        })
}

fn decode_image(bytes: &[u8], format: ImageFormat) -> Result<image::DynamicImage, &'static str> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODED_ARTWORK_BYTES);
    limits.max_image_width = Some(MAX_ARTWORK_DIMENSION);
    limits.max_image_height = Some(MAX_ARTWORK_DIMENSION);
    reader.limits(limits);
    let image = reader.decode().map_err(|_| "unsupported_artwork")?;
    if !decoded_size_is_bounded(image.width(), image.height()) {
        return Err("unsupported_artwork");
    }
    Ok(image)
}

fn convert_to_png(bytes: &[u8], format: ImageFormat) -> Result<Vec<u8>, &'static str> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_ARTWORK_BYTES {
        return Err("unsupported_artwork");
    }
    let image = decode_image(bytes, format)?;
    let pixels = image.to_rgba8();
    let mut encoded = BoundedArtworkWriter::default();
    PngEncoder::new(&mut encoded)
        .write_image(
            pixels.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|_| "unsupported_artwork")?;
    Ok(encoded.bytes)
}

#[derive(Default)]
struct BoundedArtworkWriter {
    bytes: Vec<u8>,
}

impl Write for BoundedArtworkWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > MAX_ARTWORK_BYTES as usize {
            return Err(std::io::Error::other("converted artwork exceeds limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || !bytes.starts_with(&[0xff, 0xd8]) || !bytes.ends_with(&[0xff, 0xd9]) {
        return None;
    }
    jpeg_dimensions_from_prefix(bytes)
}

fn jpeg_dimensions_from_prefix(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || !bytes.starts_with(&[0xff, 0xd8]) {
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
    discover_with_validation(app_id, library, configured_roots, None, None)
}

fn discover_with_validation(
    app_id: u32,
    library: &Path,
    configured_roots: &[PathBuf],
    mut cache: Option<&mut LocalArtworkCache>,
    cancelled: Option<&dyn Fn() -> bool>,
) -> ArtworkMetadata {
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

    let mut candidates = Vec::new();
    for root in roots {
        candidates.extend(local_candidates(app_id, &root));
    }

    let candidate_signature = local_candidate_signature(&candidates);
    candidates.sort_by(|left, right| {
        (
            right.role_priority,
            right.pixel_area,
            right.width,
            right.height,
        )
            .cmp(&(left.role_priority, left.pixel_area, left.width, left.height))
            .then_with(|| left.canonical_path.cmp(&right.canonical_path))
    });
    let best_candidate = candidates.into_iter().find(|candidate| {
        if cancelled.is_some_and(|cancelled| cancelled()) {
            return false;
        }
        cache
            .as_deref_mut()
            .is_none_or(|cache| validate_local_candidate(candidate, cache))
    });

    match best_candidate {
        Some(candidate) => ArtworkMetadata {
            candidate_signature,
            needs_remote_upgrade: candidate.role_priority != 3
                || candidate.width < 600
                || candidate.height < 900
                || u64::from(candidate.width) * 3 != u64::from(candidate.height) * 2,
            ..candidate.metadata
        },
        None => ArtworkMetadata {
            candidate_signature,
            ..ArtworkMetadata::default()
        },
    }
}

fn validate_local_candidate(
    candidate: &LocalArtworkCandidate,
    cache: &mut LocalArtworkCache,
) -> bool {
    let source_path = candidate.canonical_path.to_string_lossy();
    let Some(revision) = candidate.metadata.revision.as_deref() else {
        return false;
    };
    if read_chunk_cached(
        cache,
        &source_path,
        revision,
        &[candidate
            .canonical_path
            .parent()
            .unwrap_or(Path::new("/"))
            .to_path_buf()],
        0,
    )
    .is_err()
    {
        return false;
    }
    if image_mime(&candidate.canonical_path) == Some("image/png") {
        let Ok(file) = File::open(&candidate.canonical_path) else {
            return false;
        };
        let mut bytes = Vec::new();
        if file
            .take(MAX_ARTWORK_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 != candidate.byte_length
        {
            return false;
        }
        return decode_image(&bytes, ImageFormat::Png).is_ok()
            && candidate
                .canonical_path
                .metadata()
                .ok()
                .is_some_and(|metadata| {
                    revision_for_metadata(&candidate.canonical_path, &metadata) == revision
                });
    }
    true
}

fn local_candidate_signature(candidates: &[LocalArtworkCandidate]) -> String {
    if candidates.is_empty() {
        return String::new();
    }

    let mut candidates_in_signature_order = candidates.iter().collect::<Vec<_>>();
    candidates_in_signature_order.sort_by(|left, right| {
        left.canonical_path
            .cmp(&right.canonical_path)
            .then_with(|| left.role_priority.cmp(&right.role_priority))
            .then_with(|| left.byte_length.cmp(&right.byte_length))
            .then_with(|| left.modified_nanos.cmp(&right.modified_nanos))
            .then_with(|| left.width.cmp(&right.width))
            .then_with(|| left.height.cmp(&right.height))
    });

    let mut signature = CandidateSignatureBuilder::new();
    signature.write_u64(candidates_in_signature_order.len() as u64);
    for candidate in candidates_in_signature_order {
        signature.write_bytes(&canonical_path_signature_bytes(&candidate.canonical_path));
        signature.write_u64(candidate.byte_length);
        signature.write_u128(candidate.modified_nanos);
        signature.write_u64(u64::from(candidate.role_priority));
        signature.write_u64(u64::from(candidate.width));
        signature.write_u64(u64::from(candidate.height));
    }
    format!("{:016x}", signature.finish())
}

struct CandidateSignatureBuilder(u64);

impl CandidateSignatureBuilder {
    fn new() -> Self {
        Self(1_469_598_103_934_665_603)
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        self.write_u64(bytes.len() as u64);
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(1_099_511_628_211);
        }
    }

    fn write_u64(&mut self, value: u64) {
        for byte in value.to_le_bytes() {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(1_099_511_628_211);
        }
    }

    fn write_u128(&mut self, value: u128) {
        for byte in value.to_le_bytes() {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(1_099_511_628_211);
        }
    }

    fn finish(self) -> u64 {
        self.0
    }
}

#[cfg(unix)]
fn canonical_path_signature_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().to_vec()
}

#[cfg(windows)]
fn canonical_path_signature_bytes(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(not(any(unix, windows)))]
fn canonical_path_signature_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}

fn local_candidates(app_id: u32, root: &Path) -> Vec<LocalArtworkCandidate> {
    let mut candidates = Vec::new();
    let cache_directories = [
        root.join("appcache/librarycache"),
        root.join("librarycache"),
    ];
    let file_suffixes = [
        ("_library_600x900_2x", 3),
        ("_library_600x900", 3),
        ("_header", 2),
        ("_icon", 1),
    ];
    let extensions = ["jpg", "png", "webp"];

    for cache in &cache_directories {
        for (suffix, role_priority) in file_suffixes {
            for extension in extensions {
                let candidate = cache.join(format!("{app_id}{suffix}.{extension}"));
                if let Some(candidate) = inspect_candidate(&candidate, root, role_priority) {
                    candidates.push(candidate);
                }
            }
        }
        candidates.extend(from_hashed_library_cache(app_id, cache, root));
    }
    candidates.extend(from_user_grid(app_id, root));
    candidates
}

fn from_hashed_library_cache(
    app_id: u32,
    cache: &Path,
    canonical_root: &Path,
) -> Vec<LocalArtworkCandidate> {
    let app_directory = cache.join(app_id.to_string());
    let Some(mut hash_directories) = bounded_directories(&app_directory, MAX_HASH_DIRECTORIES)
    else {
        return Vec::new();
    };
    hash_directories.sort();
    let mut candidates = Vec::new();
    for hash_directory in hash_directories {
        for (name, role_priority) in [
            ("library_600x900_2x.jpg", 3),
            ("library_600x900_2x.png", 3),
            ("library_600x900_2x.webp", 3),
            ("library_600x900.jpg", 3),
            ("library_600x900.png", 3),
            ("library_600x900.webp", 3),
            ("library_capsule.jpg", 3),
            ("library_capsule.png", 3),
            ("library_capsule.webp", 3),
            ("library_header.jpg", 2),
            ("library_header.png", 2),
            ("library_header.webp", 2),
        ] {
            if let Some(candidate) =
                inspect_candidate(&hash_directory.join(name), canonical_root, role_priority)
            {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

fn from_user_grid(app_id: u32, root: &Path) -> Vec<LocalArtworkCandidate> {
    let Some(mut users) = bounded_directories(&root.join("userdata"), MAX_USER_DIRECTORIES) else {
        return Vec::new();
    };
    users.sort();
    let suffixes = [
        ("p.png", 3),
        ("p.jpg", 3),
        ("_p.png", 3),
        ("_p.jpg", 3),
        ("_hero.png", 2),
        ("_hero.jpg", 2),
        ("_icon.png", 1),
        ("_icon.jpg", 1),
        (".png", 1),
    ];
    let mut candidates = Vec::new();
    for user in users {
        let grid = user.join("config/grid");
        for (suffix, role_priority) in suffixes {
            if let Some(candidate) =
                inspect_candidate(&grid.join(format!("{app_id}{suffix}")), root, role_priority)
            {
                candidates.push(candidate);
            }
        }
    }
    candidates
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

fn inspect_candidate(
    candidate: &Path,
    canonical_root: &Path,
    role_priority: u8,
) -> Option<LocalArtworkCandidate> {
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
    let (width, height) = image_dimensions(&canonical_path)?;
    if !decoded_size_is_bounded(width, height) {
        return None;
    }
    // A mislabeled wide library image is a header even when its filename says
    // 600x900. It must not displace a real portrait in another Steam root.
    let role_priority = if role_priority == 3 && width > height {
        2
    } else {
        role_priority
    };
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |modified| modified.as_nanos());
    let revision = revision(&canonical_path, metadata.len(), modified_nanos);
    Some(LocalArtworkCandidate {
        metadata: ArtworkMetadata {
            source_path: canonical_path.to_str().map(str::to_owned),
            revision: Some(revision),
            candidate_signature: String::new(),
            needs_remote_upgrade: false,
        },
        canonical_path,
        byte_length: metadata.len(),
        modified_nanos,
        role_priority,
        pixel_area: u64::from(width) * u64::from(height),
        width,
        height,
    })
}

fn image_dimensions(path: &Path) -> Option<(u32, u32)> {
    let file = File::open(path).ok()?;
    let mut header = Vec::with_capacity(MAX_DIMENSION_HEADER_BYTES);
    file.take(MAX_DIMENSION_HEADER_BYTES as u64)
        .read_to_end(&mut header)
        .ok()?;
    if header.starts_with(b"\x89PNG\r\n\x1a\n") && header.len() >= 24 && &header[12..16] == b"IHDR"
    {
        let width = u32::from_be_bytes(header[16..20].try_into().ok()?);
        let height = u32::from_be_bytes(header[20..24].try_into().ok()?);
        return (width > 0 && height > 0).then_some((width, height));
    }
    if header.starts_with(&[0xff, 0xd8]) {
        return jpeg_dimensions_from_prefix(&header);
    }
    webp_dimensions(&header)
}

fn decoded_size_is_bounded(width: u32, height: u32) -> bool {
    width <= MAX_ARTWORK_DIMENSION
        && height <= MAX_ARTWORK_DIMENSION
        && u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .is_some_and(|bytes| bytes <= MAX_DECODED_ARTWORK_BYTES)
}

fn webp_dimensions(header: &[u8]) -> Option<(u32, u32)> {
    if header.len() < 30 || &header[..4] != b"RIFF" || &header[8..12] != b"WEBP" {
        return None;
    }
    match &header[12..16] {
        b"VP8X" if header.len() >= 30 => {
            let width = 1 + u32::from_le_bytes([header[24], header[25], header[26], 0]);
            let height = 1 + u32::from_le_bytes([header[27], header[28], header[29], 0]);
            Some((width, height))
        }
        b"VP8 " if header[23..26] == [0x9d, 0x01, 0x2a] => {
            let width = u32::from(u16::from_le_bytes([header[26], header[27]]) & 0x3fff);
            let height = u32::from(u16::from_le_bytes([header[28], header[29]]) & 0x3fff);
            (width > 0 && height > 0).then_some((width, height))
        }
        b"VP8L" if header[20] == 0x2f => {
            let width = 1 + u32::from(header[21]) + (u32::from(header[22] & 0x3f) << 8);
            let height = 1
                + (u32::from(header[22] & 0xc0) >> 6)
                + (u32::from(header[23]) << 2)
                + (u32::from(header[24] & 0x0f) << 10);
            Some((width, height))
        }
        _ => None,
    }
}

#[cfg(test)]
pub fn read_chunk(
    source_path: &str,
    expected_revision: &str,
    allowed_roots: &[PathBuf],
    offset: u64,
) -> Result<ArtworkChunk, &'static str> {
    read_chunk_cached(
        &mut LocalArtworkCache::default(),
        source_path,
        expected_revision,
        allowed_roots,
        offset,
    )
}

pub fn read_chunk_cached(
    cache: &mut LocalArtworkCache,
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
        cache
            .images
            .retain(|image| image.source_path != canonical_source);
        return Err("artwork_changed");
    }

    let mut header = [0_u8; 12];
    let header_bytes = file.read(&mut header).map_err(|_| "artwork_unavailable")?;
    let mime_type = image_mime_from_header(&header[..header_bytes]).ok_or("unsupported_artwork")?;
    if mime_type != "image/png" {
        if let Some(index) = cache.images.iter().position(|image| {
            image.source_path == canonical_source && image.revision == expected_revision
        }) {
            if revision_for_metadata(
                &canonical_source,
                &file.metadata().map_err(|_| "artwork_changed")?,
            ) != expected_revision
                || source_path.canonicalize().map_err(|_| "artwork_changed")? != canonical_source
            {
                cache
                    .images
                    .retain(|image| image.source_path != canonical_source);
                return Err("artwork_changed");
            }
            let image = cache.images.remove(index).expect("located image exists");
            let result = chunk_from_bytes(&image.bytes, expected_revision, offset);
            cache.images.push_back(image);
            return result;
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|_| "artwork_unavailable")?;
        let mut source_bytes = Vec::with_capacity(file_metadata.len() as usize);
        (&mut file)
            .take(file_metadata.len() + 1)
            .read_to_end(&mut source_bytes)
            .map_err(|_| "artwork_changed")?;
        if source_bytes.len() as u64 != file_metadata.len()
            || revision_for_metadata(
                &canonical_source,
                &file.metadata().map_err(|_| "artwork_changed")?,
            ) != expected_revision
            || source_path.canonicalize().map_err(|_| "artwork_changed")? != canonical_source
        {
            return Err("artwork_changed");
        }
        let format = if mime_type == "image/jpeg" {
            ImageFormat::Jpeg
        } else {
            ImageFormat::WebP
        };
        let converted = convert_to_png(&source_bytes, format)?;
        if revision_for_metadata(
            &canonical_source,
            &file.metadata().map_err(|_| "artwork_changed")?,
        ) != expected_revision
            || source_path.canonicalize().map_err(|_| "artwork_changed")? != canonical_source
        {
            return Err("artwork_changed");
        }
        let result = chunk_from_bytes(&converted, expected_revision, offset);
        cache
            .images
            .retain(|image| image.source_path != canonical_source);
        if cache.images.len() == MAX_REMOTE_CACHE_IMAGES {
            cache.images.pop_front();
        }
        cache.images.push_back(CachedLocalArtwork {
            source_path: canonical_source,
            revision: expected_revision.to_owned(),
            bytes: converted,
        });
        return result;
    }
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
        fs::write(&portrait, synthetic_png(300, 450)).unwrap();
        fs::write(&header, synthetic_jpeg(800, 450, 64)).unwrap();

        let first = discover(42, &root.0, &[]);
        assert_eq!(first.source_path.as_deref(), portrait.to_str());
        assert!(first.revision.is_some());
        assert!(first.needs_remote_upgrade);

        fs::write(&portrait, synthetic_png(301, 451)).unwrap();
        let second = discover(42, &root.0, &[]);
        assert_eq!(second.source_path.as_deref(), portrait.to_str());
        assert_ne!(second.revision, first.revision);
    }

    #[test]
    fn candidate_signature_tracks_non_selected_artwork_candidates() {
        let root = TemporaryRoot::new();
        let cache = root.0.join("appcache/librarycache");
        fs::create_dir_all(&cache).unwrap();
        let portrait = cache.join("42_library_600x900_2x.png");
        let header = cache.join("42_header.png");
        let icon = cache.join("42_icon.png");
        fs::write(&portrait, synthetic_png(600, 900)).unwrap();
        fs::write(&header, synthetic_png(1280, 720)).unwrap();

        let first = discover(42, &root.0, &[]);
        assert_eq!(first.source_path.as_deref(), portrait.to_str());
        assert!(!first.candidate_signature.is_empty());

        let mut updated_header = synthetic_png(1280, 720);
        updated_header.push(1);
        fs::write(&header, updated_header).unwrap();
        let after_mutation = discover(42, &root.0, &[]);
        assert_eq!(after_mutation.source_path.as_deref(), portrait.to_str());
        assert_ne!(
            after_mutation.candidate_signature,
            first.candidate_signature
        );

        fs::write(&icon, synthetic_png(64, 64)).unwrap();
        let after_addition = discover(42, &root.0, &[]);
        assert_eq!(after_addition.source_path.as_deref(), portrait.to_str());
        assert_ne!(
            after_addition.candidate_signature,
            after_mutation.candidate_signature
        );
    }

    #[test]
    fn candidate_signature_is_independent_of_root_enumeration_order() {
        let first_root = TemporaryRoot::new();
        let second_root = TemporaryRoot::new();
        let first_cache = first_root.0.join("appcache/librarycache");
        let second_cache = second_root.0.join("appcache/librarycache");
        fs::create_dir_all(&first_cache).unwrap();
        fs::create_dir_all(&second_cache).unwrap();
        fs::write(
            first_cache.join("42_library_600x900_2x.png"),
            synthetic_png(600, 900),
        )
        .unwrap();
        fs::write(second_cache.join("42_icon.png"), synthetic_png(64, 64)).unwrap();

        let forward = discover(42, &first_root.0, std::slice::from_ref(&second_root.0));
        let reverse = discover(42, &second_root.0, std::slice::from_ref(&first_root.0));

        assert_eq!(forward.candidate_signature, reverse.candidate_signature);
    }

    #[test]
    fn selects_the_largest_local_portrait_across_roots_before_a_larger_header() {
        let primary_root = TemporaryRoot::new();
        let secondary_root = TemporaryRoot::new();
        let primary_cache = primary_root.0.join("appcache/librarycache");
        let secondary_cache = secondary_root.0.join("appcache/librarycache");
        fs::create_dir_all(&primary_cache).unwrap();
        fs::create_dir_all(&secondary_cache).unwrap();

        let low_resolution_portrait = primary_cache.join("42_library_600x900_2x.png");
        let large_header = primary_cache.join("42_header.png");
        let high_resolution_portrait = secondary_cache.join("42_library_600x900.png");
        fs::write(&low_resolution_portrait, synthetic_png(300, 450)).unwrap();
        fs::write(&large_header, synthetic_png(3840, 2160)).unwrap();
        fs::write(&high_resolution_portrait, synthetic_png(1200, 1800)).unwrap();

        let artwork = discover(42, &primary_root.0, std::slice::from_ref(&secondary_root.0));

        assert_eq!(
            artwork.source_path.as_deref(),
            high_resolution_portrait.to_str()
        );
        assert!(!artwork.needs_remote_upgrade);
    }

    #[test]
    fn wide_library_assets_do_not_displace_a_low_resolution_portrait() {
        let root = TemporaryRoot::new();
        let cache = root.0.join("appcache/librarycache");
        let hashed = cache.join("42/hash");
        fs::create_dir_all(&hashed).unwrap();
        let portrait = cache.join("42_library_600x900.png");
        fs::write(&portrait, synthetic_png(300, 450)).unwrap();
        fs::write(
            cache.join("42_library_600x900_2x.png"),
            synthetic_png(1920, 1080),
        )
        .unwrap();
        fs::write(
            hashed.join("library_capsule.png"),
            synthetic_png(2048, 1024),
        )
        .unwrap();

        let artwork = discover(42, &root.0, &[]);
        assert_eq!(artwork.source_path.as_deref(), portrait.to_str());
        assert!(artwork.needs_remote_upgrade);
    }

    #[test]
    fn full_local_portrait_stays_selected_over_wide_library_assets() {
        let root = TemporaryRoot::new();
        let cache = root.0.join("appcache/librarycache");
        fs::create_dir_all(&cache).unwrap();
        let portrait = cache.join("42_library_600x900.png");
        fs::write(&portrait, synthetic_png(600, 900)).unwrap();
        fs::write(
            cache.join("42_library_600x900_2x.png"),
            synthetic_png(1920, 1080),
        )
        .unwrap();

        let artwork = discover(42, &root.0, &[]);
        assert_eq!(artwork.source_path.as_deref(), portrait.to_str());
        assert!(!artwork.needs_remote_upgrade);
    }

    #[test]
    fn full_hashed_capsule_portrait_beats_a_low_resolution_local_portrait() {
        let root = TemporaryRoot::new();
        let cache = root.0.join("appcache/librarycache");
        let hashed = cache.join("42/hash");
        fs::create_dir_all(&hashed).unwrap();
        fs::write(
            cache.join("42_library_600x900.png"),
            synthetic_png(300, 450),
        )
        .unwrap();
        let capsule = hashed.join("library_capsule.png");
        fs::write(&capsule, synthetic_png(600, 900)).unwrap();

        let artwork = discover(42, &root.0, &[]);
        assert_eq!(artwork.source_path.as_deref(), capsule.to_str());
        assert!(!artwork.needs_remote_upgrade);
    }

    #[test]
    fn reads_bounded_dimensions_from_supported_local_image_headers() {
        let root = TemporaryRoot::new();
        let jpeg_path = root.0.join("portrait.jpg");
        let png_path = root.0.join("portrait.png");
        fs::write(&jpeg_path, synthetic_jpeg(600, 900, 64)).unwrap();
        fs::write(&png_path, synthetic_png(1200, 1800)).unwrap();

        assert_eq!(image_dimensions(&jpeg_path), Some((600, 900)));
        assert_eq!(image_dimensions(&png_path), Some((1200, 1800)));
    }

    #[test]
    fn oversized_or_malformed_local_headers_yield_to_a_valid_portrait() {
        let primary = TemporaryRoot::new();
        let fallback = TemporaryRoot::new();
        let primary_cache = primary.0.join("appcache/librarycache");
        let fallback_cache = fallback.0.join("appcache/librarycache");
        fs::create_dir_all(&primary_cache).unwrap();
        fs::create_dir_all(&fallback_cache).unwrap();
        fs::write(
            primary_cache.join("42_library_600x900_2x.png"),
            synthetic_png(u32::MAX, u32::MAX),
        )
        .unwrap();
        fs::write(
            primary_cache.join("42_library_600x900_2x.jpg"),
            synthetic_jpeg(6000, 9000, 64),
        )
        .unwrap();
        let mut webp = vec![0; 30];
        webp[..4].copy_from_slice(b"RIFF");
        webp[8..12].copy_from_slice(b"WEBP");
        webp[12..16].copy_from_slice(b"VP8X");
        webp[24..27].copy_from_slice(&[0xff, 0xff, 0]);
        webp[27..30].copy_from_slice(&[0xff, 0xff, 0]);
        fs::write(primary_cache.join("42_library_600x900_2x.webp"), webp).unwrap();
        let mut malformed_png = synthetic_png(1200, 1800);
        malformed_png[12..16].copy_from_slice(b"JUNK");
        fs::write(primary_cache.join("42_library_600x900.png"), malformed_png).unwrap();
        let portrait = fallback_cache.join("42_library_600x900.png");
        fs::write(&portrait, synthetic_png(600, 900)).unwrap();

        let artwork = discover(42, &primary.0, std::slice::from_ref(&fallback.0));
        assert_eq!(artwork.source_path.as_deref(), portrait.to_str());
        assert!(!artwork.needs_remote_upgrade);
    }

    fn synthetic_png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0; 24];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes[12..16].copy_from_slice(b"IHDR");
        bytes[16..20].copy_from_slice(&width.to_be_bytes());
        bytes[20..24].copy_from_slice(&height.to_be_bytes());
        bytes
    }

    fn valid_png(width: u32, height: u32) -> Vec<u8> {
        let pixels = vec![32_u8; width as usize * height as usize * 4];
        let mut encoded = Vec::new();
        PngEncoder::new(&mut encoded)
            .write_image(&pixels, width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        encoded
    }

    #[test]
    fn validation_skips_corrupt_full_portrait_before_publishing_fallback() {
        let root = TemporaryRoot::new();
        let directory = root.0.join("appcache/librarycache");
        fs::create_dir_all(&directory).unwrap();
        let corrupt = directory.join("42_library_600x900_2x.jpg");
        let fallback = directory.join("42_library_600x900.png");
        fs::write(&corrupt, synthetic_jpeg(600, 900, 64)).unwrap();
        fs::write(&fallback, valid_png(300, 450)).unwrap();
        let mut cache = LocalArtworkCache::default();
        let artwork = discover_with_validation(42, &root.0, &[], Some(&mut cache), None);
        assert_eq!(artwork.source_path.as_deref(), fallback.to_str());
        assert!(artwork.needs_remote_upgrade);

        fs::write(&fallback, synthetic_png(600, 900)).unwrap();
        let artwork = discover_with_validation(42, &root.0, &[], Some(&mut cache), None);
        assert!(artwork.source_path.is_none());
        assert!(artwork.revision.is_none());
    }

    #[test]
    fn falls_back_to_user_grid_and_ignores_unbounded_or_empty_sources() {
        let root = TemporaryRoot::new();
        let grid = root.0.join("userdata/100/config/grid");
        fs::create_dir_all(&grid).unwrap();
        fs::write(grid.join("42p.png"), synthetic_png(600, 900)).unwrap();

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
        let bytes = valid_jpeg();
        let converted = convert_to_png(&bytes, ImageFormat::Jpeg).unwrap();
        fs::write(&image, &bytes).unwrap();
        let discovered = discover(42, &root.0, &[]);
        let source_path = discovered.source_path.unwrap();
        let revision = discovered.revision.unwrap();

        let first = read_chunk(&source_path, &revision, std::slice::from_ref(&root.0), 0).unwrap();
        assert_eq!(first.mime_type, "image/png");
        assert!(first.data.starts_with("iVBOR"));
        assert_eq!(first.total_bytes, converted.len() as u64);
        assert_eq!(first.next_offset, Some(ARTWORK_CHUNK_BYTES as u64));
        assert_eq!(first.data, base64_encode(&converted[..ARTWORK_CHUNK_BYTES]));

        let last = read_chunk(
            &source_path,
            &revision,
            std::slice::from_ref(&root.0),
            ((converted.len() - 1) / ARTWORK_CHUNK_BYTES * ARTWORK_CHUNK_BYTES) as u64,
        )
        .unwrap();
        assert_eq!(last.next_offset, None);
        let last_offset = (converted.len() - 1) / ARTWORK_CHUNK_BYTES * ARTWORK_CHUNK_BYTES;
        assert_eq!(last.data, base64_encode(&converted[last_offset..]));
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
    fn local_webp_conversion_is_cached_and_stale_revision_is_rejected() {
        use image::codecs::webp::WebPEncoder;
        let root = TemporaryRoot::new();
        let cache_dir = root.0.join("appcache/librarycache");
        fs::create_dir_all(&cache_dir).unwrap();
        let image_path = cache_dir.join("42_library_600x900_2x.webp");
        let pixels = vec![64_u8; 300 * 450 * 4];
        let mut webp = Vec::new();
        WebPEncoder::new_lossless(&mut webp)
            .write_image(&pixels, 300, 450, image::ExtendedColorType::Rgba8)
            .unwrap();
        fs::write(&image_path, &webp).unwrap();
        let discovered = discover(42, &root.0, &[]);
        let revision = discovered.revision.unwrap();
        let mut cache = LocalArtworkCache::default();
        let first = read_chunk_cached(
            &mut cache,
            image_path.to_str().unwrap(),
            &revision,
            std::slice::from_ref(&root.0),
            0,
        )
        .unwrap();
        assert_eq!(first.mime_type, "image/png");
        assert_eq!(cache.images.len(), 1);
        let again = read_chunk_cached(
            &mut cache,
            image_path.to_str().unwrap(),
            &revision,
            std::slice::from_ref(&root.0),
            0,
        )
        .unwrap();
        assert_eq!(again.data, first.data);
        fs::write(&image_path, b"changed").unwrap();
        assert_eq!(
            read_chunk_cached(
                &mut cache,
                image_path.to_str().unwrap(),
                &revision,
                std::slice::from_ref(&root.0),
                0,
            )
            .unwrap_err(),
            "artwork_changed"
        );
        assert!(cache.images.is_empty());
    }

    #[test]
    fn remote_decode_failure_does_not_enter_cache_or_publish_png() {
        let revision = remote_revision(42, 123).unwrap();
        let mut cache = RemoteArtworkCache::default();
        let malformed = synthetic_jpeg(600, 900, 64);
        assert!(validate_remote_artwork(&malformed));
        assert_eq!(
            cache
                .read_chunk_with(42, 123, &revision, 0, Instant::now(), |_| Ok(malformed))
                .unwrap_err(),
            "unsupported_artwork"
        );
        assert!(cache.images.is_empty());
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

    fn valid_jpeg() -> Vec<u8> {
        use image::codecs::jpeg::JpegEncoder;
        static JPEG: OnceLock<Vec<u8>> = OnceLock::new();
        JPEG.get_or_init(|| {
            let mut pixels = Vec::with_capacity(600 * 900 * 3);
            for y in 0..900_u32 {
                for x in 0..600_u32 {
                    pixels.extend_from_slice(&[
                        (x.wrapping_mul(37) ^ y.wrapping_mul(19)) as u8,
                        (x.wrapping_mul(11) ^ y.wrapping_mul(43)) as u8,
                        (x.wrapping_mul(53) ^ y.wrapping_mul(7)) as u8,
                    ]);
                }
            }
            let mut encoded = Vec::new();
            JpegEncoder::new_with_quality(&mut encoded, 75)
                .write_image(&pixels, 600, 900, image::ExtendedColorType::Rgb8)
                .unwrap();
            encoded
        })
        .clone()
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
        let source = valid_jpeg();
        let bytes = convert_remote_jpeg(&source).unwrap();
        let revision = remote_revision(42, 123).unwrap();
        let mut cache = RemoteArtworkCache::default();
        let now = Instant::now();
        let first = cache
            .read_chunk_with(42, 123, &revision, 0, now, |_| Ok(source.clone()))
            .unwrap();
        assert_eq!(first.mime_type, "image/png");
        assert_eq!(first.total_bytes, bytes.len() as u64);
        assert_eq!(first.next_offset, Some(ARTWORK_CHUNK_BYTES as u64));
        assert_eq!(first.data, base64_encode(&bytes[..ARTWORK_CHUNK_BYTES]));

        let last_offset = ((bytes.len() - 1) / ARTWORK_CHUNK_BYTES * ARTWORK_CHUNK_BYTES) as u64;
        let last = cache
            .read_chunk_with(42, 123, &revision, last_offset, now, |_| {
                panic!("cache hit must not fetch again")
            })
            .unwrap();
        assert_eq!(last.next_offset, None);
        assert_eq!(last.data, base64_encode(&bytes[last_offset as usize..]));
        assert_eq!(last.revision, revision);
    }

    #[test]
    fn remote_artwork_survives_cache_restart_and_rejects_corrupt_disk_bytes() {
        let root = TemporaryRoot::new();
        let revision = remote_revision(42, 123).unwrap();
        let source = valid_jpeg();
        let bytes = convert_remote_jpeg(&source).unwrap();
        let now = Instant::now();
        let mut first = RemoteArtworkCache::with_disk_root(Some(root.0.clone()));
        first
            .read_chunk_with(42, 123, &revision, 0, now, |_| Ok(source.clone()))
            .unwrap();
        let path = root
            .0
            .join(REMOTE_CACHE_DIRECTORY)
            .join(format!("42-{revision}.png"));
        assert_eq!(fs::read(&path).unwrap(), bytes);

        let mut restarted = RemoteArtworkCache::with_disk_root(Some(root.0.clone()));
        let from_disk = restarted
            .read_chunk_with(42, 123, &revision, ARTWORK_CHUNK_BYTES as u64, now, |_| {
                panic!("valid persistent portrait must not fetch")
            })
            .unwrap();
        assert_eq!(
            from_disk.data,
            base64_encode(&bytes[ARTWORK_CHUNK_BYTES..2 * ARTWORK_CHUNK_BYTES])
        );

        fs::write(&path, b"corrupt").unwrap();
        let mut recovered = RemoteArtworkCache::with_disk_root(Some(root.0.clone()));
        recovered
            .read_chunk_with(42, 123, &revision, 0, now, |_| Ok(source.clone()))
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(recovered.images.len(), 1);
        let mut after_recovery = RemoteArtworkCache::with_disk_root(Some(root.0.clone()));
        after_recovery
            .read_chunk_with(42, 123, &revision, 0, now, |_| {
                panic!("recovered portrait must survive another restart")
            })
            .unwrap();
    }

    #[test]
    fn abandoned_temporary_file_does_not_block_a_new_portrait() {
        let root = TemporaryRoot::new();
        let mut cache = RemoteArtworkCache::with_disk_root(Some(root.0.clone()));
        let revision = remote_revision(42, 123).unwrap();
        let child = root.0.join(REMOTE_CACHE_DIRECTORY);
        let abandoned = child.join(format!("42-{revision}.{}.tmp", std::process::id()));
        fs::write(&abandoned, b"abandoned previous process write").unwrap();
        let source = valid_jpeg();
        let bytes = convert_remote_jpeg(&source).unwrap();
        cache
            .read_chunk_with(
                42,
                123,
                &revision,
                0,
                Instant::now(),
                |_| Ok(source.clone()),
            )
            .unwrap();
        assert_eq!(
            fs::read(child.join(format!("42-{revision}.png"))).unwrap(),
            bytes
        );
        assert_eq!(
            fs::read(abandoned).unwrap(),
            b"abandoned previous process write"
        );
    }

    #[test]
    fn aged_temporary_files_are_reclaimed_before_corrupt_entry_replacement() {
        let root = TemporaryRoot::new();
        let child = root.0.join(REMOTE_CACHE_DIRECTORY);
        let revision = remote_revision(42, 123).unwrap();
        let entry = child.join(format!("42-{revision}.png"));
        let cache = RemoteArtworkCache::with_disk_root(Some(root.0.clone()));
        fs::write(&entry, b"corrupt").unwrap();
        let active_clock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let active = child.join(format!("42-{revision}.999.{active_clock}.0.tmp"));
        fs::write(&active, b"active writer").unwrap();
        for serial in 0..100 {
            fs::write(
                child.join(format!("42-{revision}.999.1.{serial}.tmp")),
                b"orphan",
            )
            .unwrap();
        }
        let unrelated = root.0.join(format!("42-{revision}.999.1.0.tmp"));
        fs::write(&unrelated, b"unrelated parent").unwrap();
        let mut cache = cache;
        let source = valid_jpeg();
        let bytes = convert_remote_jpeg(&source).unwrap();
        cache
            .read_chunk_with(
                42,
                123,
                &revision,
                0,
                Instant::now(),
                |_| Ok(source.clone()),
            )
            .unwrap();
        assert_eq!(fs::read(entry).unwrap(), bytes);
        assert_eq!(fs::read(active).unwrap(), b"active writer");
        assert_eq!(fs::read(unrelated).unwrap(), b"unrelated parent");
        assert_eq!(fs::read_dir(&child).unwrap().count(), 2);
    }

    #[test]
    fn current_temporary_file_cap_prevents_unbounded_disk_writes() {
        let root = TemporaryRoot::new();
        let child = root.0.join(REMOTE_CACHE_DIRECTORY);
        let mut cache = RemoteArtworkCache::with_disk_root(Some(root.0.clone()));
        let revision = remote_revision(42, 123).unwrap();
        let clock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        for serial in 0..MAX_REMOTE_CACHE_TEMP_FILES {
            fs::write(
                child.join(format!("42-{revision}.999.{clock}.{serial}.tmp")),
                b"active",
            )
            .unwrap();
        }
        let bytes = valid_jpeg();
        assert!(
            cache
                .read_chunk_with(42, 123, &revision, 0, Instant::now(), |_| Ok(bytes))
                .is_ok()
        );
        assert!(!child.join(format!("42-{revision}.png")).exists());
        assert_eq!(
            fs::read_dir(child).unwrap().count(),
            MAX_REMOTE_CACHE_TEMP_FILES
        );
    }

    #[test]
    fn disk_cache_evicts_old_portraits_and_requires_an_existing_root() {
        let root = TemporaryRoot::new();
        let unrelated = root
            .0
            .join(format!("1-{}.jpg", remote_revision(1, 123).unwrap()));
        fs::write(&unrelated, b"unrelated parent file").unwrap();
        let mut cache = RemoteArtworkCache::with_disk_root(Some(root.0.clone()));
        for app_id in 1..=6 {
            let revision = remote_revision(app_id, 123).unwrap();
            cache
                .read_chunk_with(app_id, 123, &revision, 0, Instant::now(), |_| {
                    Ok(valid_jpeg())
                })
                .unwrap();
        }
        assert_eq!(
            fs::read_dir(root.0.join(REMOTE_CACHE_DIRECTORY))
                .unwrap()
                .count(),
            MAX_REMOTE_CACHE_IMAGES
        );
        assert_eq!(fs::read(unrelated).unwrap(), b"unrelated parent file");
        assert!(
            RemoteArtworkCache::with_disk_root(Some(root.0.join("missing")))
                .disk_root
                .is_none()
        );
    }

    #[test]
    fn disk_cache_rejects_linked_cache_directory_and_entry() {
        let root = TemporaryRoot::new();
        let other = TemporaryRoot::new();
        let child = root.0.join(REMOTE_CACHE_DIRECTORY);
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&other.0, &child);
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&other.0, &child);
        if linked.is_err() {
            return;
        } // Windows may deny symlink creation to this test user.
        assert!(
            RemoteArtworkCache::with_disk_root(Some(root.0.clone()))
                .disk_root
                .is_none()
        );
        fs::remove_dir(&child).unwrap();

        let mut cache = RemoteArtworkCache::with_disk_root(Some(root.0.clone()));
        let revision = remote_revision(42, 123).unwrap();
        let target = other.0.join("portrait.png");
        fs::write(&target, synthetic_jpeg(600, 900, 64)).unwrap();
        let entry = child.join(format!("42-{revision}.png"));
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &entry).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &entry).unwrap();
        cache
            .read_chunk_with(42, 123, &revision, 0, Instant::now(), |_| Ok(valid_jpeg()))
            .unwrap();
        assert!(
            fs::symlink_metadata(&entry)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(target).unwrap(), synthetic_jpeg(600, 900, 64));
    }

    #[test]
    fn remote_artwork_accepts_the_reference_limit_and_rejects_bytes_above_it() {
        let revision = remote_revision(42, 123).unwrap();
        let mut cache = RemoteArtworkCache::default();
        let valid = valid_jpeg();
        let bytes = convert_remote_jpeg(&valid).unwrap();
        let source = [
            &valid[..valid.len() - 2],
            &vec![0_u8; MAX_ARTWORK_BYTES as usize - valid.len()][..],
            &[0xff, 0xd9],
        ]
        .concat();
        assert!(validate_remote_artwork(&source));
        assert_eq!(source.len() as u64, MAX_ARTWORK_BYTES);
        let first = cache
            .read_chunk_with(42, 123, &revision, 0, Instant::now(), |_| Ok(valid))
            .unwrap();
        assert_eq!(first.total_bytes, bytes.len() as u64);
        assert_eq!(first.next_offset, Some(ARTWORK_CHUNK_BYTES as u64));

        let last_offset = ((bytes.len() - 1) / ARTWORK_CHUNK_BYTES * ARTWORK_CHUNK_BYTES) as u64;
        let last = cache
            .read_chunk_with(42, 123, &revision, last_offset, Instant::now(), |_| {
                panic!("cached full-size artwork must not be fetched again")
            })
            .unwrap();
        assert_eq!(last.total_bytes, bytes.len() as u64);
        assert_eq!(last.next_offset, None);
        assert_eq!(last.data, base64_encode(&bytes[last_offset as usize..]));

        let oversized = synthetic_jpeg(600, 900, MAX_ARTWORK_BYTES as usize + 1);
        assert!(!validate_remote_artwork(&oversized));
        assert_eq!(
            RemoteArtworkCache::default()
                .read_chunk_with(
                    43,
                    123,
                    &remote_revision(43, 123).unwrap(),
                    0,
                    Instant::now(),
                    |_| { Ok(oversized) }
                )
                .unwrap_err(),
            "unsupported_artwork"
        );
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
        let bytes = valid_jpeg();
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
        assert!(!validate_remote_artwork(&synthetic_jpeg(6000, 9000, 64)));
    }

    #[test]
    fn artwork_fetch_runs_off_the_calling_thread() {
        let mut worker = RemoteArtworkWorker::default();
        let bytes = valid_jpeg();
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
                prefetch: false,
                generation: 0,
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
                assert_eq!(completion.result.unwrap().mime_type, "image/png");
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("background artwork fetch did not complete");
    }

    #[test]
    fn artwork_worker_queue_is_bounded_while_a_fetch_is_running() {
        let mut worker = RemoteArtworkWorker::default();
        let bytes = valid_jpeg();
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
                prefetch: false,
                generation: 0,
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
                prefetch: false,
                generation: 0,
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
