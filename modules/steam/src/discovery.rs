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
    metadata: Option<MetadataScanner>,
}

impl Scan {
    pub fn new(roots: &[PathBuf]) -> Self {
        let mut scan = Self {
            metadata: Some(MetadataScanner::new(roots)),
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
                        match read_game(&entry.path(), &library, app_id) {
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
    if let Some(program_files) = std::env::var_os("ProgramFiles(x86)") {
        candidates.push(PathBuf::from(program_files).join("Steam/steam.exe"));
    }

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

    candidates.into_iter().find_map(|candidate| {
        let canonical = candidate.canonicalize().ok()?;
        canonical.is_file().then_some(canonical)
    })
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

fn read_game(path: &Path, library: &Path, expected_id: u32) -> Result<Game, String> {
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
        last_updated: unsigned_field(app, "lastupdated", false)?,
        app_type: match app.field("type") {
            Ok(value) => Some(value.text()?.to_owned()),
            Err(_) => None,
        },
        last_played: None,
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
