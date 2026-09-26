use crate::artwork;
use crate::keyvalues::{self, Value};
use crate::metadata::Scanner as MetadataScanner;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, ReadDir};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_LIBRARIES: usize = 64;
const MAX_DIRECTORY_ENTRIES: usize = 100_000;
const MAX_GAMES: usize = 20_000;

#[derive(Clone, Serialize)]
pub struct Game {
    pub stable_id: String,
    pub app_id: u32,
    pub name: String,
    pub install_directory: String,
    pub library_directory: String,
    pub state_flags: u64,
    pub installed: bool,
    pub last_updated: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_played: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playtime_minutes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artwork_revision: Option<String>,
    #[serde(skip)]
    pub artwork_source_path: Option<String>,
    #[serde(skip)]
    pub artwork_is_remote: bool,
    #[serde(skip)]
    pub artwork_candidate_signature: String,
}

#[derive(Default)]
pub struct Scan {
    libraries: VecDeque<PathBuf>,
    seen_libraries: BTreeSet<PathBuf>,
    directory: Option<(PathBuf, ReadDir)>,
    pub games: BTreeMap<u32, Game>,
    pub warnings: Vec<String>,
    pub warning_count: u64,
    pub visited_entries: usize,
    pub finished: bool,
    pub appinfo_complete: bool,
    pub history_complete: bool,
    pub owned_history_complete: bool,
    metadata: Option<MetadataScanner>,
    artwork_roots: Vec<PathBuf>,
}

impl Scan {
    pub fn new(roots: &[PathBuf]) -> Self {
        let mut scan = Self {
            metadata: Some(MetadataScanner::new(roots)),
            artwork_roots: roots.to_vec(),
            ..Self::default()
        };
        if roots.is_empty() {
            scan.warn("no_library_roots");
            return scan;
        }
        for root in roots {
            scan.enqueue(root.clone());
        }
        scan
    }

    fn warn(&mut self, code: &str) {
        self.warning_count += 1;
        if self.warnings.len() < 32 {
            self.warnings.push(code.into());
        }
    }

    fn enqueue(&mut self, root: PathBuf) {
        let root = if root
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("steamapps"))
        {
            root.parent().unwrap_or(&root).to_path_buf()
        } else {
            root
        };
        let root = match root.canonicalize() {
            Ok(path) => path,
            Err(_) => {
                self.warn("library_unavailable");
                return;
            }
        };
        if self.seen_libraries.contains(&root) {
            return;
        }
        if self.seen_libraries.len() >= MAX_LIBRARIES {
            self.warn("library_limit");
            return;
        }
        self.seen_libraries.insert(root.clone());
        self.libraries.push_back(root);
    }

    /// Reads one manifest, opens one library, or advances a bounded sidecar scan
    /// per tick. No scanner thread survives destruction; individual filesystem
    /// calls may still block.
    pub fn tick(&mut self) {
        if self.finished {
            return;
        }
        if self.visited_entries >= MAX_DIRECTORY_ENTRIES {
            self.warn("directory_entry_limit");
            self.finished = true;
            return;
        }
        if let Some((library, directory)) = &mut self.directory {
            let library = library.clone();
            match directory.next() {
                Some(Ok(entry)) => {
                    self.visited_entries += 1;
                    if let Some(app_id) = manifest_id(&entry.file_name().to_string_lossy()) {
                        let artwork_roots = self.artwork_roots.clone();
                        match read_game(&entry.path(), &library, app_id, &artwork_roots) {
                            Ok(game) => self.insert_game(game),
                            Err(_) => self.warn("invalid_or_unavailable_manifest"),
                        }
                    }
                }
                Some(Err(_)) => {
                    self.visited_entries += 1;
                    self.warn("directory_read_failed");
                }
                None => self.directory = None,
            }
        } else if let Some(library) = self.libraries.pop_front() {
            self.open_library(library);
        } else if let Some(metadata) = &mut self.metadata {
            if metadata.tick(&mut self.games) {
                let availability = metadata.availability();
                self.appinfo_complete = availability.appinfo_complete;
                self.history_complete = availability.history_complete;
                self.owned_history_complete = availability.owned_history_complete;
                self.finished = true;
            }
        } else {
            self.finished = true;
        }
    }

    fn open_library(&mut self, library: PathBuf) {
        let steamapps = library.join("steamapps");
        let folders = steamapps.join("libraryfolders.vdf");
        match read_document(&folders) {
            Ok(document) => match library_paths(&document) {
                Ok(paths) => {
                    for path in paths {
                        self.enqueue(path);
                    }
                }
                Err(_) => self.warn("invalid_libraryfolders"),
            },
            Err(_) if !folders.exists() => {}
            Err(_) => self.warn("invalid_libraryfolders"),
        }
        match fs::read_dir(steamapps) {
            Ok(directory) => self.directory = Some((library, directory)),
            Err(_) => self.warn("library_unavailable"),
        }
    }

    fn insert_game(&mut self, game: Game) {
        if !self.games.contains_key(&game.app_id) && self.games.len() >= MAX_GAMES {
            self.warn("game_limit");
            return;
        }
        // Prefer a fully installed copy, then the lexical library path. Discovery
        // order and directory iteration order never change duplicate resolution.
        let replace = self.games.get(&game.app_id).is_none_or(|existing| {
            (game.installed && !existing.installed)
                || (game.installed == existing.installed
                    && game.library_directory < existing.library_directory)
        });
        if replace {
            self.games.insert(game.app_id, game);
        }
    }
}

/// Finds a Steam client executable for catalog-backed `-applaunch` records.
/// Configured roots are checked first so secondary library roots never become
/// launch executables by accident; platform defaults and PATH are fallbacks.
pub fn find_steam_executable(roots: &[PathBuf]) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    for configured_root in roots {
        let root = if configured_root
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("steamapps"))
        {
            configured_root.parent().unwrap_or(configured_root)
        } else {
            configured_root
        };
        candidates.extend([
            root.join("steam.exe"),
            root.join("steam.sh"),
            root.join("steam"),
            root.join("steam_osx"),
            root.join("Steam.app/Contents/MacOS/steam_osx"),
            root.join("Contents/MacOS/steam_osx"),
        ]);
    }

    #[cfg(target_os = "windows")]
    candidates.extend(windows_default_client_candidates(
        windows_registered_steam_root(),
        std::env::var_os("ProgramFiles(x86)").map(PathBuf::from),
        std::env::var_os("ProgramFiles").map(PathBuf::from),
        std::env::var_os("LocalAppData").map(PathBuf::from),
    ));

    #[cfg(target_os = "linux")]
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|directory| directory.join("steam")));
    }

    #[cfg(target_os = "macos")]
    {
        candidates.push(PathBuf::from(
            "/Applications/Steam.app/Contents/MacOS/steam_osx",
        ));
        if let Some(home) = std::env::var_os("HOME") {
            candidates
                .push(PathBuf::from(home).join("Applications/Steam.app/Contents/MacOS/steam_osx"));
        }
    }

    resolve_client_candidates(candidates)
}

/// An omitted root setting follows the current user's common Steam installs.
/// Privileged Linux processes and explicitly marked machine hosts do not infer
/// desktop-user paths from the host's HOME.
pub fn default_library_roots() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        existing_library_roots(windows_default_install_roots(
            windows_registered_steam_root(),
            std::env::var_os("ProgramFiles(x86)").map(PathBuf::from),
            std::env::var_os("ProgramFiles").map(PathBuf::from),
            std::env::var_os("LocalAppData").map(PathBuf::from),
        ))
    }

    #[cfg(target_os = "linux")]
    {
        linux_default_library_roots(
            std::env::var_os("HOME").as_deref().map(Path::new),
            std::env::var_os("XDG_DATA_HOME").as_deref().map(Path::new),
            linux_user_session_allowed(),
        )
    }

    #[cfg(target_os = "macos")]
    {
        macos_default_library_roots(std::env::var_os("HOME").as_deref().map(Path::new))
    }

    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    Vec::new()
}

#[cfg(any(windows, target_os = "linux", target_os = "macos", test))]
fn existing_library_roots(candidates: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut seen = BTreeSet::new();
    for candidate in candidates {
        let Ok(root) = candidate.canonicalize() else {
            continue;
        };
        if root.join("steamapps").is_dir() && seen.insert(root.clone()) {
            roots.push(root);
        }
    }
    roots
}

#[cfg(any(target_os = "macos", test))]
fn macos_default_library_roots(home: Option<&Path>) -> Vec<PathBuf> {
    let Some(home) = home.filter(|path| path.is_absolute()) else {
        return Vec::new();
    };
    existing_library_roots(vec![home.join("Library/Application Support/Steam")])
}

#[cfg(any(target_os = "linux", test))]
fn linux_default_library_roots(
    home: Option<&Path>,
    xdg_data_home: Option<&Path>,
    user_session: bool,
) -> Vec<PathBuf> {
    if !user_session {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    if let Some(home) = home.filter(|path| path.is_absolute()) {
        for relative in [
            ".local/share/Steam",
            ".steam/steam",
            ".steam/root",
            ".steam/debian-installation",
            ".var/app/com.valvesoftware.Steam/data/Steam",
            ".var/app/com.valvesoftware.Steam/.local/share/Steam",
            "snap/steam/common/.local/share/Steam",
        ] {
            candidates.push(home.join(relative));
        }
    }
    if let Some(xdg_data_home) = xdg_data_home.filter(|path| path.is_absolute()) {
        candidates.push(xdg_data_home.join("Steam"));
    }
    existing_library_roots(candidates)
}

#[cfg(target_os = "linux")]
fn linux_user_session_allowed() -> bool {
    let machine_host = ["AMATERASU_MACHINE_HOST", "VIBESHINE_MACHINE_HOST"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    std::fs::read_to_string("/proc/self/status")
        .is_ok_and(|status| linux_status_allows_user_roots(&status, machine_host))
}

#[cfg(any(target_os = "linux", test))]
fn linux_status_allows_user_roots(status: &str, machine_host: bool) -> bool {
    if machine_host {
        return false;
    }
    let effective_uid = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|uids| uids.split_whitespace().nth(1))
        .and_then(|uid| uid.parse::<u32>().ok());
    let effective_capabilities = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .and_then(|capabilities| u64::from_str_radix(capabilities.trim(), 16).ok());
    matches!((effective_uid, effective_capabilities), (Some(uid), Some(caps))
        if uid != 0 && caps & (1_u64 << 21) == 0)
}

fn resolve_client_candidates(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find_map(|candidate| {
        let canonical = candidate.canonicalize().ok()?;
        canonical.is_file().then_some(canonical)
    })
}

#[cfg(windows)]
fn windows_default_client_candidates(
    registered_root: Option<PathBuf>,
    program_files_x86: Option<PathBuf>,
    program_files: Option<PathBuf>,
    local_app_data: Option<PathBuf>,
) -> Vec<PathBuf> {
    windows_default_install_roots(
        registered_root,
        program_files_x86,
        program_files,
        local_app_data,
    )
    .into_iter()
    .map(|root| root.join("steam.exe"))
    .collect()
}

#[cfg(windows)]
fn windows_default_install_roots(
    registered_root: Option<PathBuf>,
    program_files_x86: Option<PathBuf>,
    program_files: Option<PathBuf>,
    local_app_data: Option<PathBuf>,
) -> Vec<PathBuf> {
    registered_root
        .into_iter()
        .chain(program_files_x86.map(|path| path.join("Steam")))
        .chain(program_files.map(|path| path.join("Steam")))
        .chain(local_app_data.map(|path| path.join("Steam")))
        .collect()
}

#[cfg(windows)]
fn windows_registered_steam_root() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;

    const HKEY_CURRENT_USER: isize = -2_147_483_647;
    const RRF_RT_REG_SZ: u32 = 2;
    const ERROR_SUCCESS: u32 = 0;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            key: isize,
            subkey: *const u16,
            value: *const u16,
            flags: u32,
            value_type: *mut u32,
            data: *mut std::ffi::c_void,
            data_bytes: *mut u32,
        ) -> u32;
    }

    let subkey = "Software\\Valve\\Steam"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let value = "SteamPath"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut byte_count = 0_u32;
    // SAFETY: Both name pointers refer to terminated UTF-16 buffers that live
    // through the call. The output size pointer is valid; no data is requested.
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut byte_count,
        )
    };
    if result != ERROR_SUCCESS
        || !(4..=32 * 1024).contains(&byte_count)
        || !byte_count.is_multiple_of(2)
    {
        return None;
    }
    let mut path = vec![0_u16; byte_count as usize / 2];
    // SAFETY: `path` owns `byte_count` writable bytes and remains allocated
    // through the call. RegGetValueW receives its exact capacity and a valid
    // mutable size pointer; the registry string is read as REG_SZ only.
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            path.as_mut_ptr().cast(),
            &mut byte_count,
        )
    };
    if result != ERROR_SUCCESS
        || byte_count as usize > path.len() * 2
        || !byte_count.is_multiple_of(2)
    {
        return None;
    }
    let text = &path[..byte_count as usize / 2];
    let terminator = text.iter().position(|unit| *unit == 0)?;
    if terminator == 0 {
        return None;
    }
    Some(std::ffi::OsString::from_wide(&text[..terminator]).into())
}

fn read_document(path: &Path) -> Result<Value, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > 1_048_576 {
        return Err("manifest must be a regular file of at most 1 MiB".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| error.to_string())?
        .take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "manifest is not UTF-8")?;
    keyvalues::parse(text)
}

fn manifest_id(filename: &str) -> Option<u32> {
    let digits = filename
        .strip_prefix("appmanifest_")?
        .strip_suffix(".acf")?;
    let id = digits.parse::<u32>().ok()?;
    (id != 0 && id.to_string() == digits).then_some(id)
}

fn library_paths(document: &Value) -> Result<Vec<PathBuf>, String> {
    let Value::Object(folders) = document.field("libraryfolders")? else {
        return Err("libraryfolders must be an object".into());
    };
    let mut paths = Vec::new();
    for (index, entry) in folders {
        if index.parse::<u32>().is_err() {
            continue;
        }
        let path = match entry {
            Value::Text(path) => path.as_str(),
            Value::Object(_) => entry.field("path")?.text()?,
        };
        if path.len() > 4096 || !Path::new(path).is_absolute() || path.contains('\0') {
            return Err("library path must be absolute and bounded".into());
        }
        paths.push(PathBuf::from(path));
    }
    Ok(paths)
}

fn unsigned_field(document: &Value, name: &str, required: bool) -> Result<u64, String> {
    match document.field(name) {
        Ok(value) => value.text()?.parse().map_err(|_| format!("invalid {name}")),
        Err(_) if !required => Ok(0),
        Err(error) => Err(error),
    }
}

fn read_game(
    path: &Path,
    library: &Path,
    expected_id: u32,
    artwork_roots: &[PathBuf],
) -> Result<Game, String> {
    let document = read_document(path)?;
    let app = document.field("appstate")?;
    if unsigned_field(app, "appid", true)? != u64::from(expected_id) {
        return Err("manifest ID does not match filename".into());
    }
    let name = app.field("name")?.text()?;
    let directory = app.field("installdir")?.text()?;
    if name.trim().is_empty()
        || name.len() > 512
        || name.chars().any(char::is_control)
        || directory.is_empty()
        || directory.len() > 255
        || directory == "."
        || directory == ".."
        || directory
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\' | ':'))
    {
        return Err("invalid game name or installation directory".into());
    }
    let state_flags = unsigned_field(app, "stateflags", true)?;
    let install_directory = library.join("steamapps").join("common").join(directory);
    let installed = state_flags & 4 != 0 && install_directory.is_dir();
    let last_updated = unsigned_field(app, "lastupdated", false)?;
    let artwork = artwork::discover(expected_id, library, artwork_roots);
    let artwork_is_remote = artwork.revision.is_none();
    let artwork_revision = artwork
        .revision
        .or_else(|| artwork::remote_revision(expected_id, last_updated));
    let game = Game {
        stable_id: format!("steam:{expected_id}"),
        app_id: expected_id,
        name: name.into(),
        install_directory: install_directory
            .to_str()
            .ok_or("non-UTF8 install path")?
            .into(),
        library_directory: library.to_str().ok_or("non-UTF8 library path")?.into(),
        state_flags,
        installed,
        last_updated,
        app_type: match app.field("type") {
            Ok(value) => Some(value.text()?.to_owned()),
            Err(_) => None,
        },
        last_played: None,
        playtime_minutes: None,
        artwork_revision,
        artwork_source_path: artwork.source_path,
        artwork_is_remote,
        artwork_candidate_signature: artwork.candidate_signature,
    };
    if serde_json::to_vec(&game)
        .map_err(|error| error.to_string())?
        .len()
        > 8192
    {
        return Err("game metadata exceeds transport limit".into());
    }
    Ok(game)
}

#[cfg(test)]
mod tests;
