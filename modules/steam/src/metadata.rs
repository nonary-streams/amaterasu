//! Bounded readers for Steam's local app-type and play-history sidecars.
use crate::artwork;
use crate::discovery::Game;
use crate::keyvalues::{self, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const APPINFO_MAGIC: u32 = 0x0756_4429;
const MAX_APPINFO_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_APPINFO_STRING_TABLE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_APPINFO_KEYS: usize = 100_000;
const MAX_APPINFO_KEY_BYTES: usize = 8_192;
const MAX_APPINFO_RECORD_BYTES: u32 = 16 * 1024 * 1024;
const MAX_APPINFO_RECORDS: usize = 2_000_000;
const APPINFO_RECORDS_PER_TICK: usize = 256;
const MAX_LOCALCONFIG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_METADATA_ROOTS: usize = 32;
const MAX_USER_ACCOUNTS: usize = 64;
const MAX_LOCALCONFIG_TASKS: usize = 256;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Availability {
    pub appinfo_complete: bool,
    pub history_complete: bool,
    pub owned_history_complete: bool,
}

enum Task {
    AppInfo(AppInfoScan),
    LocalConfig { path: PathBuf, root: PathBuf },
}

pub struct Scanner {
    roots: VecDeque<PathBuf>,
    tasks: VecDeque<Task>,
    appinfo_scans: VecDeque<AppInfoScan>,
    seen_roots: BTreeSet<PathBuf>,
    appinfo_files: usize,
    history_roots: usize,
    localconfig_tasks: usize,
    appinfo_ok: bool,
    candidate_appinfo_ok: bool,
    history_ok: bool,
    owned_history_ok: bool,
    appinfo_found: BTreeSet<u32>,
    appinfo_typed: BTreeSet<u32>,
    appinfo_named: BTreeSet<u32>,
    artwork_roots: Vec<PathBuf>,
    pending_artwork: VecDeque<u32>,
    artwork_queued: bool,
    complete: bool,
    availability: Availability,
}

impl Scanner {
    pub fn new(roots: &[PathBuf]) -> Self {
        let roots_truncated = roots.len() > MAX_METADATA_ROOTS;
        let roots: VecDeque<PathBuf> = roots
            .iter()
            .take(MAX_METADATA_ROOTS)
            .map(|root| {
                if root
                    .file_name()
                    .is_some_and(|name| name.eq_ignore_ascii_case("steamapps"))
                {
                    root.parent().unwrap_or(root).to_path_buf()
                } else {
                    root.clone()
                }
            })
            .collect();
        let artwork_roots = roots.iter().cloned().collect();
        Self {
            roots,
            tasks: VecDeque::new(),
            appinfo_scans: VecDeque::new(),
            seen_roots: BTreeSet::new(),
            appinfo_files: 0,
            history_roots: 0,
            localconfig_tasks: 0,
            appinfo_ok: !roots_truncated,
            candidate_appinfo_ok: !roots_truncated,
            history_ok: !roots_truncated,
            owned_history_ok: !roots_truncated,
            appinfo_found: BTreeSet::new(),
            appinfo_typed: BTreeSet::new(),
            appinfo_named: BTreeSet::new(),
            artwork_roots,
            pending_artwork: VecDeque::new(),
            artwork_queued: false,
            complete: false,
            availability: Availability::default(),
        }
    }

    /// Advances one root or sidecar. Appinfo is streamed in bounded record
    /// batches so large caches do not monopolize a module tick.
    pub fn tick(&mut self, games: &mut BTreeMap<u32, Game>) -> bool {
        if self.complete {
            return true;
        }
        if matches!(self.tasks.front(), Some(Task::AppInfo(_))) {
            let Some(Task::AppInfo(mut scan)) = self.tasks.pop_front() else {
                unreachable!();
            };
            match scan.tick(games) {
                Ok(true) => {
                    self.appinfo_found.extend(scan.found_records);
                    self.appinfo_typed.extend(scan.found_types);
                    self.appinfo_named.extend(scan.found_names);
                    self.candidate_appinfo_ok &= scan.candidate_ok;
                }
                Ok(false) => self.tasks.push_front(Task::AppInfo(scan)),
                Err(_) => self.appinfo_ok = false,
            }
            return false;
        }
        if let Some(Task::LocalConfig { path, root }) = self.tasks.pop_front() {
            match read_localconfig(&path, games) {
                Ok(history) => {
                    self.owned_history_ok &= history.candidates_complete;
                    for (app_id, state) in history.apps {
                        if let Some(game) = games.get_mut(&app_id) {
                            game.last_played =
                                Some(game.last_played.unwrap_or_default().max(state.last_played));
                            game.playtime_minutes = Some(
                                game.playtime_minutes
                                    .unwrap_or_default()
                                    .max(state.playtime),
                            );
                        } else if state.last_played > 0 || state.playtime > 0 {
                            if games.len() >= 20_000 {
                                self.history_ok = false;
                                self.owned_history_ok = false;
                                continue;
                            }
                            let candidate = uninstalled_history_game(app_id, &root, state);
                            if serde_json::to_vec(&candidate)
                                .map_or(true, |bytes| bytes.len() > 8192)
                            {
                                self.owned_history_ok = false;
                                continue;
                            }
                            games.insert(app_id, candidate);
                        }
                    }
                }
                Err(_) => {
                    self.history_ok = false;
                    self.owned_history_ok = false;
                }
            }
            return false;
        }
        if let Some(root) = self.roots.pop_front() {
            self.process_root(root);
            return false;
        }
        if let Some(scan) = self.appinfo_scans.pop_front() {
            self.tasks.push_front(Task::AppInfo(scan));
            return false;
        }

        if !self.artwork_queued {
            self.pending_artwork = games
                .values()
                .filter(|game| !game.installed && !game.name.is_empty())
                .map(|game| game.app_id)
                .collect();
            self.artwork_queued = true;
        }
        if let Some(app_id) = self.pending_artwork.pop_front() {
            if let Some(game) = games.get_mut(&app_id) {
                let library = Path::new(&game.library_directory);
                let art = artwork::discover(app_id, library, &self.artwork_roots);
                game.artwork_is_remote = art.revision.is_none();
                game.artwork_revision =
                    art.revision.or_else(|| artwork::remote_revision(app_id, 0));
                game.artwork_source_path = art.source_path;
                game.artwork_candidate_signature = art.candidate_signature;
            }
            return false;
        }

        let installed_apps = games
            .values()
            .filter(|game| game.installed)
            .map(|game| game.app_id)
            .collect::<BTreeSet<_>>();
        let uninstalled_apps = games
            .values()
            .filter(|game| !game.installed)
            .map(|game| game.app_id)
            .collect::<BTreeSet<_>>();
        self.availability = Availability {
            appinfo_complete: self.appinfo_files > 0
                && self.appinfo_ok
                && installed_apps.is_subset(&self.appinfo_found)
                && installed_apps.is_subset(&self.appinfo_typed),
            history_complete: self.history_roots > 0 && self.history_ok,
            owned_history_complete: self.history_roots > 0
                && self.history_ok
                && self.owned_history_ok
                && self.appinfo_ok
                && self.candidate_appinfo_ok
                && uninstalled_apps.is_subset(&self.appinfo_found)
                && uninstalled_apps.is_subset(&self.appinfo_typed)
                && uninstalled_apps.is_subset(&self.appinfo_named),
        };
        games.retain(|_, game| game.installed || !game.name.is_empty());
        self.complete = true;
        true
    }

    pub fn availability(&self) -> Availability {
        self.availability
    }

    fn process_root(&mut self, root: PathBuf) {
        let Ok(root) = root.canonicalize() else {
            return;
        };
        if !self.seen_roots.insert(root.clone()) {
            return;
        }

        let appinfo_path = root.join("appcache/appinfo.vdf");
        let userdata_path = root.join("userdata");
        let appinfo_metadata = fs::symlink_metadata(&appinfo_path).ok();
        let userdata_metadata = fs::symlink_metadata(&userdata_path).ok();
        let has_appinfo_path = appinfo_metadata.is_some();
        let has_userdata_path = userdata_metadata.is_some();
        let steam_client_present = ["steam.exe", "steam.sh", "steam", "steam_osx"]
            .iter()
            .any(|name| regular_file(&root.join(name)).is_some());

        if !has_appinfo_path && !has_userdata_path && !steam_client_present {
            return;
        }

        let appinfo = if let Some(metadata) = appinfo_metadata {
            self.appinfo_files += 1;
            match regular_metadata(&appinfo_path, metadata) {
                Ok(()) => match AppInfoScan::open(&appinfo_path) {
                    Ok(scan) => Some(scan),
                    Err(_) => {
                        self.appinfo_ok = false;
                        None
                    }
                },
                Err(_) => {
                    self.appinfo_ok = false;
                    None
                }
            }
        } else {
            self.appinfo_ok = false;
            None
        };

        match userdata_metadata {
            Some(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                self.history_roots += 1;
                self.enqueue_user_configs(&userdata_path, &root);
            }
            _ => {
                self.history_ok = false;
                self.owned_history_ok = false;
            }
        }
        if let Some(appinfo) = appinfo {
            self.appinfo_scans.push_back(appinfo);
        }
    }

    fn enqueue_user_configs(&mut self, userdata: &Path, root: &Path) {
        let Ok(entries) = fs::read_dir(userdata) else {
            self.history_ok = false;
            self.owned_history_ok = false;
            return;
        };
        let mut users = Vec::new();
        let mut accounts_seen = 0;
        for entry in entries {
            let Ok(entry) = entry else {
                self.history_ok = false;
                self.owned_history_ok = false;
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.parse::<u32>().is_err() {
                continue;
            }
            accounts_seen += 1;
            if accounts_seen > MAX_USER_ACCOUNTS {
                self.history_ok = false;
                self.owned_history_ok = false;
                break;
            }
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                self.history_ok = false;
                self.owned_history_ok = false;
                continue;
            };
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                users.push(entry.path());
            } else {
                self.history_ok = false;
                self.owned_history_ok = false;
            }
        }
        users.sort();
        for user in users {
            if self.localconfig_tasks >= MAX_LOCALCONFIG_TASKS {
                self.history_ok = false;
                self.owned_history_ok = false;
                break;
            }
            let path = user.join("config/localconfig.vdf");
            self.localconfig_tasks += 1;
            self.tasks.push_back(Task::LocalConfig {
                path,
                root: root.to_path_buf(),
            });
        }
    }
}

fn regular_file(path: &Path) -> Option<Metadata> {
    let metadata = fs::symlink_metadata(path).ok()?;
    (metadata.is_file() && !metadata.file_type().is_symlink()).then_some(metadata)
}

fn regular_metadata(path: &Path, metadata: Metadata) -> Result<(), String> {
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_APPINFO_BYTES
    {
        return Err("Steam appinfo cache is not a bounded regular file".into());
    }
    // Opening the file here catches inaccessible caches before the reader is
    // queued; the reader reopens it on its own bounded state machine.
    File::open(path).map_err(|error| error.to_string())?;
    Ok(())
}

#[derive(Clone, Copy)]
struct LocalAppState {
    last_played: u64,
    playtime: u64,
}

struct LocalConfigRead {
    apps: BTreeMap<u32, LocalAppState>,
    candidates_complete: bool,
}

fn uninstalled_history_game(app_id: u32, root: &Path, state: LocalAppState) -> Game {
    Game {
        stable_id: format!("steam:{app_id}"),
        app_id,
        name: String::new(),
        install_directory: String::new(),
        library_directory: root.to_string_lossy().into_owned(),
        state_flags: 0,
        installed: false,
        last_updated: 0,
        app_type: None,
        last_played: (state.last_played > 0).then_some(state.last_played),
        playtime_minutes: (state.playtime > 0).then_some(state.playtime),
        artwork_revision: None,
        artwork_source_path: None,
        artwork_is_remote: false,
        artwork_candidate_signature: String::new(),
    }
}

fn read_localconfig(path: &Path, games: &BTreeMap<u32, Game>) -> Result<LocalConfigRead, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_LOCALCONFIG_BYTES
    {
        return Err("Steam localconfig is not a bounded regular file".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)
        .map_err(|error| error.to_string())?
        .take(MAX_LOCALCONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 != metadata.len() {
        return Err("Steam localconfig changed while it was read".into());
    }
    let source = std::str::from_utf8(&bytes).map_err(|_| "Steam localconfig is not UTF-8")?;
    let document = keyvalues::parse_bounded(source, MAX_LOCALCONFIG_BYTES as usize)?;
    let apps = document
        .field("UserLocalConfigStore")?
        .field("Software")?
        .field("Valve")?
        .field("Steam")?
        .field("apps")?;
    let Value::Object(apps) = apps else {
        return Err("Steam localconfig apps must be an object".into());
    };
    let mut history = BTreeMap::new();
    let mut candidates_complete = true;
    for (raw_app_id, app) in apps {
        let Ok(app_id) = raw_app_id.parse::<u32>() else {
            continue;
        };
        if app_id == 0 || app_id.to_string() != *raw_app_id {
            continue;
        }
        // Other apps can have malformed local history without affecting this
        // scan's installed games. Steam's localconfig is shared by all apps.
        let installed = games.get(&app_id).is_some_and(|game| game.installed);
        let Value::Object(fields) = app else {
            if installed {
                return Err("Steam localconfig app data must be an object".into());
            }
            candidates_complete = false;
            continue;
        };
        let last_played = match fields.get("lastplayed") {
            Some(Value::Text(value)) => value
                .parse::<u64>()
                .map_err(|_| "Steam localconfig has an invalid LastPlayed timestamp"),
            Some(_) => Err("Steam localconfig LastPlayed must be text"),
            None => Ok(0),
        };
        let playtime = match fields.get("playtime") {
            Some(Value::Text(value)) => value
                .parse::<u64>()
                .map_err(|_| "Steam localconfig has invalid Playtime"),
            Some(_) => Err("Steam localconfig Playtime must be text"),
            None => Ok(0),
        };
        let (last_played, playtime) = match (last_played, playtime) {
            (Ok(last_played), Ok(playtime)) => (last_played, playtime),
            (Err(error), _) | (_, Err(error)) if installed => return Err(error.into()),
            _ => {
                candidates_complete = false;
                continue;
            }
        };
        if installed || last_played > 0 || playtime > 0 {
            history.insert(
                app_id,
                LocalAppState {
                    last_played,
                    playtime,
                },
            );
        }
    }
    Ok(LocalConfigRead {
        apps: history,
        candidates_complete,
    })
}

struct AppInfoScan {
    file: File,
    string_table_offset: u64,
    offset: u64,
    keys: Vec<String>,
    found_records: BTreeSet<u32>,
    found_types: BTreeSet<u32>,
    found_names: BTreeSet<u32>,
    candidate_ok: bool,
    records_seen: usize,
    finished: bool,
}

impl AppInfoScan {
    fn open(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|error| error.to_string())?;
        let length = file.metadata().map_err(|error| error.to_string())?.len();
        if !(20..=MAX_APPINFO_BYTES).contains(&length) {
            return Err("Steam appinfo cache has invalid size".into());
        }
        let mut header = [0u8; 16];
        file.read_exact(&mut header)
            .map_err(|error| error.to_string())?;
        if read_u32(&header, 0)? != APPINFO_MAGIC {
            return Err("Steam appinfo cache magic is unsupported".into());
        }
        let string_table_offset = read_u64(&header, 8)?;
        if string_table_offset < 16
            || string_table_offset > length - 4
            || length - string_table_offset > MAX_APPINFO_STRING_TABLE_BYTES
        {
            return Err("Steam appinfo string table offset is invalid".into());
        }
        file.seek(SeekFrom::Start(string_table_offset))
            .map_err(|error| error.to_string())?;
        let mut table = vec![0u8; (length - string_table_offset) as usize];
        file.read_exact(&mut table)
            .map_err(|error| error.to_string())?;
        let (keys, table_end) = parse_string_table(&table)?;
        if table[table_end..].iter().any(|byte| *byte != 0) {
            return Err("Steam appinfo string table has invalid trailing bytes".into());
        }
        file.seek(SeekFrom::Start(16))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            file,
            string_table_offset,
            offset: 16,
            keys,
            found_records: BTreeSet::new(),
            found_types: BTreeSet::new(),
            found_names: BTreeSet::new(),
            candidate_ok: true,
            records_seen: 0,
            finished: false,
        })
    }

    fn tick(&mut self, games: &mut BTreeMap<u32, Game>) -> Result<bool, String> {
        if self.finished {
            return Ok(true);
        }
        for _ in 0..APPINFO_RECORDS_PER_TICK {
            if self.offset == self.string_table_offset {
                self.finished = true;
                return Ok(true);
            }
            if self.offset > self.string_table_offset || self.string_table_offset - self.offset < 4
            {
                return Err("truncated Steam appinfo record index".into());
            }
            let mut bytes = [0u8; 4];
            self.file
                .read_exact(&mut bytes)
                .map_err(|error| error.to_string())?;
            let app_id = read_u32(&bytes, 0)?;
            self.offset += 4;
            if app_id == 0 {
                validate_zero_padding(&mut self.file, self.offset, self.string_table_offset)?;
                self.offset = self.string_table_offset;
                self.finished = true;
                return Ok(true);
            }
            if self.offset + 4 > self.string_table_offset {
                return Err("truncated Steam appinfo record".into());
            }
            self.file
                .read_exact(&mut bytes)
                .map_err(|error| error.to_string())?;
            let entry_size = read_u32(&bytes, 0)?;
            self.offset += 4;
            self.records_seen += 1;
            if self.records_seen > MAX_APPINFO_RECORDS || entry_size < 60 {
                return Err("Steam appinfo record count or size is invalid".into());
            }
            let entry_end = self
                .offset
                .checked_add(u64::from(entry_size))
                .filter(|end| *end <= self.string_table_offset)
                .ok_or("Steam appinfo record exceeds its string table boundary")?;
            if let Some(game) = games.get_mut(&app_id) {
                if !self.found_records.insert(app_id) {
                    if game.installed {
                        return Err("Steam appinfo contains a duplicate app ID".into());
                    }
                    self.candidate_ok = false;
                    self.file
                        .seek(SeekFrom::Start(entry_end))
                        .map_err(|error| error.to_string())?;
                    self.offset = entry_end;
                    continue;
                }
                let value_bytes = entry_size - 60;
                if value_bytes > MAX_APPINFO_RECORD_BYTES {
                    if game.installed {
                        return Err("Steam appinfo record exceeds its byte limit".into());
                    }
                    self.candidate_ok = false;
                    self.file
                        .seek(SeekFrom::Start(entry_end))
                        .map_err(|error| error.to_string())?;
                    self.offset = entry_end;
                    continue;
                }
                self.file
                    .seek(SeekFrom::Current(60))
                    .map_err(|error| error.to_string())?;
                let mut data = vec![0u8; value_bytes as usize];
                self.file
                    .read_exact(&mut data)
                    .map_err(|error| error.to_string())?;
                match appinfo_fields(&data, &self.keys) {
                    Ok((app_type, name)) => {
                        game.app_type = Some(app_type);
                        self.found_types.insert(app_id);
                        if !game.installed
                            && let Some(name) = name
                        {
                            game.name = name;
                            self.found_names.insert(app_id);
                        }
                    }
                    Err(error) if game.installed => return Err(error),
                    Err(_) => self.candidate_ok = false,
                }
            } else {
                self.file
                    .seek(SeekFrom::Start(entry_end))
                    .map_err(|error| error.to_string())?;
            }
            self.offset = entry_end;
        }
        Ok(false)
    }
}

fn appinfo_fields(data: &[u8], keys: &[String]) -> Result<(String, Option<String>), String> {
    let mut cursor = 0;
    let mut nodes = 0;
    let root = parse_binary_object(data, &mut cursor, data.len(), keys, 0, &mut nodes)?;
    if cursor != data.len() {
        return Err("Steam appinfo record has trailing binary data".into());
    }
    let metadata = root.field("appinfo").unwrap_or(&root);
    let common = metadata.field("common")?;
    let app_type = common.field("type")?.text()?.trim();
    if app_type.is_empty() || app_type.len() > 64 || app_type.chars().any(char::is_control) {
        return Err("Steam appinfo type is invalid".into());
    }
    let name = common
        .field("name")
        .ok()
        .and_then(|value| value.text().ok());
    let name = name
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= 160 && !name.chars().any(char::is_control))
        .map(str::to_owned);
    Ok((app_type.to_owned(), name))
}

fn parse_string_table(bytes: &[u8]) -> Result<(Vec<String>, usize), String> {
    let count = read_u32(bytes, 0)? as usize;
    if count > MAX_APPINFO_KEYS {
        return Err("Steam appinfo has too many string keys".into());
    }
    let mut offset = 4;
    let mut keys = Vec::with_capacity(count);
    for _ in 0..count {
        let key = read_cstring(bytes, &mut offset, bytes.len(), MAX_APPINFO_KEY_BYTES)?;
        keys.push(key.to_ascii_lowercase());
    }
    Ok((keys, offset))
}

fn parse_binary_object(
    bytes: &[u8],
    offset: &mut usize,
    end: usize,
    keys: &[String],
    depth: usize,
    nodes: &mut usize,
) -> Result<Value, String> {
    if depth > 16 {
        return Err("Steam appinfo nesting exceeds 16".into());
    }
    let mut values = BTreeMap::new();
    loop {
        let kind = *bytes
            .get(*offset)
            .filter(|_| *offset < end)
            .ok_or("unterminated Steam appinfo object")?;
        *offset += 1;
        if kind == 0x08 {
            return Ok(Value::Object(values));
        }
        *nodes += 1;
        if *nodes > 32_768 {
            return Err("Steam appinfo record has too many values".into());
        }
        let key_index = read_u32(bytes, *offset)? as usize;
        *offset += 4;
        let key = keys
            .get(key_index)
            .ok_or("Steam appinfo references an unknown key")?
            .clone();
        let value = match kind {
            0x00 => parse_binary_object(bytes, offset, end, keys, depth + 1, nodes)?,
            0x01 => Value::Text(read_cstring(bytes, offset, end, MAX_APPINFO_KEY_BYTES)?),
            0x02 | 0x03 | 0x04 | 0x06 => {
                let value = read_u32(bytes, *offset)?;
                *offset += 4;
                Value::Text(value.to_string())
            }
            0x05 => Value::Text(read_wide_string(bytes, offset, end)?),
            0x07 | 0x0a => {
                let value = read_u64(bytes, *offset)?;
                *offset += 8;
                Value::Text(value.to_string())
            }
            0x09 => {
                let raw = read_u64(bytes, *offset)?;
                *offset += 8;
                Value::Text((raw as i64).to_string())
            }
            _ => return Err("Steam appinfo uses an unsupported binary value type".into()),
        };
        if values.insert(key, value).is_some() {
            return Err("Steam appinfo record has duplicate keys".into());
        }
    }
}

fn read_cstring(
    bytes: &[u8],
    offset: &mut usize,
    end: usize,
    max_bytes: usize,
) -> Result<String, String> {
    let begin = *offset;
    while *offset < end && bytes[*offset] != 0 {
        *offset += 1;
        if *offset - begin > max_bytes {
            return Err("Steam appinfo string exceeds its byte limit".into());
        }
    }
    if *offset >= end {
        return Err("unterminated Steam appinfo string".into());
    }
    let value = std::str::from_utf8(&bytes[begin..*offset])
        .map_err(|_| "Steam appinfo string is not UTF-8")?
        .to_owned();
    *offset += 1;
    Ok(value)
}

fn read_wide_string(bytes: &[u8], offset: &mut usize, end: usize) -> Result<String, String> {
    let mut units = Vec::new();
    while offset.checked_add(1).is_some_and(|next| next < end) {
        let unit = u16::from_le_bytes([bytes[*offset], bytes[*offset + 1]]);
        *offset += 2;
        if unit == 0 {
            return Ok(String::from_utf16_lossy(&units));
        }
        units.push(unit);
        if units.len() > MAX_APPINFO_KEY_BYTES {
            return Err("Steam appinfo wide string exceeds its byte limit".into());
        }
    }
    Err("unterminated Steam appinfo wide string".into())
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or("truncated Steam appinfo u32")?;
    Ok(u32::from_le_bytes(
        value.try_into().expect("four-byte slice"),
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, String> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or("truncated Steam appinfo u64")?;
    Ok(u64::from_le_bytes(
        value.try_into().expect("eight-byte slice"),
    ))
}

fn validate_zero_padding(file: &mut File, mut start: u64, end: u64) -> Result<(), String> {
    let mut buffer = [0u8; 4096];
    while start < end {
        let length = (end - start).min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..length])
            .map_err(|error| error.to_string())?;
        if buffer[..length].iter().any(|byte| *byte != 0) {
            return Err("Steam appinfo has data after its record terminator".into());
        }
        start += length as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "amaterasu-steam-metadata-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }

        fn steam_root(&self) -> &Path {
            &self.0
        }

        fn installed_manifest(&self, app_id: u32) {
            let directory = self.0.join(format!("steamapps/common/Game-{app_id}"));
            fs::create_dir_all(&directory).unwrap();
            fs::write(self.0.join("steam.exe"), "client").unwrap();
            fs::write(
                self.0
                    .join(format!("steamapps/appmanifest_{app_id}.acf")),
                format!(
                    "AppState {{ appid {app_id} name Game-{app_id} installdir Game-{app_id} StateFlags 4 }}"
                ),
            )
            .unwrap();
        }

        fn appinfo(&self, entries: &[(u32, &str)]) {
            let named = entries
                .iter()
                .map(|(id, kind)| (*id, *kind, None))
                .collect::<Vec<_>>();
            self.appinfo_with_names(&named);
        }

        fn appinfo_with_names(&self, entries: &[(u32, &str, Option<&str>)]) {
            let mut records = Vec::new();
            for (app_id, app_type, name) in entries {
                let mut object = vec![0x00];
                push_u32(&mut object, 0); // appinfo
                object.push(0x00);
                push_u32(&mut object, 1); // common
                object.push(0x01);
                push_u32(&mut object, 2); // type
                object.extend_from_slice(app_type.as_bytes());
                object.push(0);
                if let Some(name) = name {
                    object.push(0x01);
                    push_u32(&mut object, 3); // name
                    object.extend_from_slice(name.as_bytes());
                    object.push(0);
                }
                object.push(0x08); // common end
                object.push(0x08); // appinfo end
                object.push(0x08); // root end

                push_u32(&mut records, *app_id);
                push_u32(&mut records, 60 + object.len() as u32);
                records.extend_from_slice(&[0; 60]);
                records.extend_from_slice(&object);
            }
            let string_table_offset = 16 + records.len() as u64;
            let mut bytes = Vec::new();
            push_u32(&mut bytes, APPINFO_MAGIC);
            push_u32(&mut bytes, 31);
            bytes.extend_from_slice(&string_table_offset.to_le_bytes());
            bytes.extend_from_slice(&records);
            push_u32(&mut bytes, 4);
            for key in ["appinfo", "common", "type", "name"] {
                bytes.extend_from_slice(key.as_bytes());
                bytes.push(0);
            }
            let path = self.0.join("appcache/appinfo.vdf");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }

        fn localconfig(&self, account: &str, apps: &[(u32, u64)]) {
            let mut rows = String::new();
            for (app_id, last_played) in apps {
                rows.push_str(&format!(
                    "\"{app_id}\" {{ \"LastPlayed\" \"{last_played}\" }} "
                ));
            }
            let path = self
                .0
                .join(format!("userdata/{account}/config/localconfig.vdf"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                path,
                format!(
                    "UserLocalConfigStore {{ Software {{ Valve {{ Steam {{ apps {{ {rows} }} }} }} }} }}"
                ),
            )
            .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn push_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn scan_to_completion(root: &Path) -> crate::discovery::Scan {
        let mut scan = crate::discovery::Scan::new(&[root.to_path_buf()]);
        for _ in 0..20_000 {
            scan.tick();
            if scan.finished {
                return scan;
            }
        }
        panic!("Steam metadata scan did not finish");
    }

    #[test]
    fn reads_app_types_and_merges_last_played_across_local_users() {
        let fixture = Fixture::new();
        fixture.installed_manifest(42);
        fixture.appinfo(&[(42, "game")]);
        fixture.localconfig("100", &[(42, 1_700_000_000)]);
        fixture.localconfig("200", &[(42, 1_800_000_000)]);

        let scan = scan_to_completion(fixture.steam_root());
        let game = &scan.games[&42];
        assert_eq!(game.app_type.as_deref(), Some("game"));
        assert_eq!(game.last_played, Some(1_800_000_000));
        assert!(scan.appinfo_complete);
        assert!(scan.history_complete);
    }

    #[test]
    fn previously_played_uninstalled_games_recover_without_duplicating_installs() {
        let fixture = Fixture::new();
        fixture.installed_manifest(42);
        fixture.appinfo_with_names(&[
            (42, "game", Some("Installed cache name")),
            (77, "game", Some("Previously played")),
            (88, "tool", Some("Old tool")),
        ]);
        fixture.localconfig("100", &[(42, 1_700_000_000), (77, 1_800_000_000)]);
        let localconfig = fixture.0.join("userdata/100/config/localconfig.vdf");
        fs::write(
            &localconfig,
            "UserLocalConfigStore { Software { Valve { Steam { apps { 42 { LastPlayed 1700000000 } 77 { LastPlayed 1800000000 } 88 { Playtime 15 } } } } } }",
        ).unwrap();

        let first = scan_to_completion(fixture.steam_root());
        assert!(first.appinfo_complete && first.history_complete && first.owned_history_complete);
        assert_eq!(first.games.len(), 3);
        assert!(first.games[&42].installed);
        assert_eq!(first.games[&42].name, "Game-42");
        assert!(!first.games[&77].installed);
        assert_eq!(first.games[&77].name, "Previously played");
        assert_eq!(first.games[&77].last_played, Some(1_800_000_000));
        assert_eq!(first.games[&77].stable_id, "steam:77");
        assert!(first.games[&77].artwork_revision.is_some());
        assert_eq!(first.games[&88].playtime_minutes, Some(15));
        assert_eq!(first.games[&88].last_played, None);
        assert_eq!(
            serde_json::to_value(&first.games[&88]).unwrap()["playtime_minutes"],
            15
        );

        fixture.localconfig("100", &[(42, 1_700_000_000)]);
        let removed = scan_to_completion(fixture.steam_root());
        assert!(removed.appinfo_complete && removed.owned_history_complete);
        assert!(!removed.games.contains_key(&77));
        fixture.localconfig("100", &[(42, 1_700_000_000), (77, 1_800_000_000)]);
        let recovered = scan_to_completion(fixture.steam_root());
        assert_eq!(recovered.games[&77].name, "Previously played");
    }

    #[test]
    fn missing_candidate_name_or_malformed_history_cannot_authorize_cleanup() {
        let fixture = Fixture::new();
        fixture.installed_manifest(42);
        fixture.appinfo(&[(42, "game"), (77, "game")]);
        fixture.localconfig("100", &[(42, 1_700_000_000), (77, 1_800_000_000)]);
        let missing_name = scan_to_completion(fixture.steam_root());
        assert!(missing_name.appinfo_complete);
        assert!(!missing_name.owned_history_complete);
        assert!(!missing_name.games.contains_key(&77));
        assert!(missing_name.games[&42].installed);

        fixture.appinfo_with_names(&[(42, "game", None), (77, "", Some("Invalid type"))]);
        let malformed_appinfo = scan_to_completion(fixture.steam_root());
        assert!(malformed_appinfo.appinfo_complete);
        assert!(!malformed_appinfo.owned_history_complete);
        assert!(malformed_appinfo.games[&42].installed);
        assert!(!malformed_appinfo.games.contains_key(&77));

        fixture.appinfo_with_names(&[(42, "game", None), (77, "game", Some("Old game"))]);
        let prior = scan_to_completion(fixture.steam_root());
        assert_eq!(prior.games[&77].name, "Old game");
        fs::write(
            fixture.0.join("userdata/100/config/localconfig.vdf"),
            "UserLocalConfigStore { Software { Valve { Steam { apps { 42 { LastPlayed 1700000000 } 77 { LastPlayed broken } } } } } }",
        ).unwrap();
        let malformed = scan_to_completion(fixture.steam_root());
        assert!(malformed.history_complete);
        assert!(!malformed.owned_history_complete);
        assert!(malformed.games[&42].installed);
        assert!(!malformed.games.contains_key(&77));
        fixture.localconfig("100", &[(42, 1_700_000_000), (77, 1_800_000_000)]);
        let recovered = scan_to_completion(fixture.steam_root());
        assert!(recovered.owned_history_complete);
        assert_eq!(recovered.games[&77].name, "Old game");
    }

    #[test]
    fn each_uninstalled_artwork_lookup_gets_its_own_scanner_tick() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("steam.exe"), "client").unwrap();
        fixture.appinfo_with_names(&[
            (77, "game", Some("First old game")),
            (88, "game", Some("Second old game")),
        ]);
        fixture.localconfig("100", &[(77, 1_800_000_000), (88, 1_700_000_000)]);
        let mut scanner = Scanner::new(std::slice::from_ref(&fixture.0));
        let mut games = BTreeMap::new();
        for _ in 0..100 {
            scanner.tick(&mut games);
            if scanner.artwork_queued {
                break;
            }
        }
        assert!(scanner.artwork_queued);
        assert_eq!(scanner.pending_artwork.len(), 1);
        assert_eq!(
            games
                .values()
                .filter(|game| game.artwork_revision.is_some())
                .count(),
            1
        );
        scanner.tick(&mut games);
        assert!(scanner.pending_artwork.is_empty());
        assert_eq!(
            games
                .values()
                .filter(|game| game.artwork_revision.is_some())
                .count(),
            2
        );
    }

    #[test]
    fn later_root_history_can_use_earlier_root_appinfo() {
        let first = Fixture::new();
        let second = Fixture::new();
        first.installed_manifest(42);
        first.appinfo_with_names(&[
            (42, "game", None),
            (77, "game", Some("First root cache name")),
        ]);
        first.localconfig("100", &[(42, 1_700_000_000)]);
        fs::create_dir_all(second.0.join("steamapps")).unwrap();
        fs::write(second.0.join("steam.exe"), "client").unwrap();
        second.appinfo(&[(42, "game")]);
        second.localconfig("200", &[(77, 1_800_000_000)]);
        let mut scan = crate::discovery::Scan::new(&[first.0.clone(), second.0.clone()]);
        for _ in 0..20_000 {
            scan.tick();
            if scan.finished {
                break;
            }
        }
        assert!(scan.finished);
        assert!(scan.appinfo_complete && scan.owned_history_complete);
        assert!(scan.games[&42].installed);
        assert_eq!(scan.games[&77].name, "First root cache name");
        assert!(!scan.games[&77].installed);
    }

    #[test]
    fn one_missing_account_config_makes_history_incomplete() {
        let fixture = Fixture::new();
        fixture.installed_manifest(42);
        fixture.appinfo(&[(42, "game")]);
        fixture.localconfig("100", &[(42, 1_800_000_000)]);
        fs::create_dir_all(fixture.0.join("userdata/200/config")).unwrap();

        let scan = scan_to_completion(fixture.steam_root());
        assert!(scan.appinfo_complete);
        assert!(!scan.history_complete);
        assert_eq!(scan.games[&42].last_played, Some(1_800_000_000));
    }

    #[test]
    fn unrelated_corrupt_history_does_not_block_installed_game_reconciliation() {
        let fixture = Fixture::new();
        fixture.installed_manifest(42);
        fixture.appinfo(&[(42, "game")]);
        let localconfig = fixture
            .steam_root()
            .join("userdata/100/config/localconfig.vdf");
        fs::create_dir_all(localconfig.parent().unwrap()).unwrap();
        fs::write(
            &localconfig,
            "UserLocalConfigStore { Software { Valve { Steam { apps { 42 { LastPlayed 1800000000 } 99 { LastPlayed broken } } } } } }",
        )
        .unwrap();

        let scan = scan_to_completion(fixture.steam_root());
        assert!(scan.history_complete);
        assert!(scan.appinfo_complete);
        assert_eq!(scan.games[&42].last_played, Some(1_800_000_000));
    }

    #[test]
    fn corrupt_installed_game_history_still_blocks_catalog_cleanup() {
        let fixture = Fixture::new();
        fixture.installed_manifest(42);
        fixture.appinfo(&[(42, "game")]);
        let localconfig = fixture
            .steam_root()
            .join("userdata/100/config/localconfig.vdf");
        fs::create_dir_all(localconfig.parent().unwrap()).unwrap();
        fs::write(
            localconfig,
            "UserLocalConfigStore { Software { Valve { Steam { apps { 42 { LastPlayed broken } } } } } }",
        )
        .unwrap();

        let scan = scan_to_completion(fixture.steam_root());
        assert!(!scan.history_complete);
        assert!(scan.appinfo_complete);
    }

    #[test]
    fn missing_appinfo_and_userdata_do_not_become_authoritative_empty_metadata() {
        let fixture = Fixture::new();
        fixture.installed_manifest(42);
        let scan = scan_to_completion(fixture.steam_root());
        assert!(!scan.appinfo_complete);
        assert!(!scan.history_complete);
    }
}
