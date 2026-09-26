//! Bounded readers for Steam's local app-type and play-history sidecars.
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
}

enum Task {
    AppInfo(AppInfoScan),
    LocalConfig(PathBuf),
}

pub struct Scanner {
    roots: VecDeque<PathBuf>,
    tasks: VecDeque<Task>,
    seen_roots: BTreeSet<PathBuf>,
    appinfo_files: usize,
    history_roots: usize,
    localconfig_tasks: usize,
    appinfo_ok: bool,
    history_ok: bool,
    appinfo_found: BTreeSet<u32>,
    appinfo_typed: BTreeSet<u32>,
    complete: bool,
    availability: Availability,
}

impl Scanner {
    pub fn new(roots: &[PathBuf]) -> Self {
        let roots_truncated = roots.len() > MAX_METADATA_ROOTS;
        let roots = roots
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
        Self {
            roots,
            tasks: VecDeque::new(),
            seen_roots: BTreeSet::new(),
            appinfo_files: 0,
            history_roots: 0,
            localconfig_tasks: 0,
            appinfo_ok: !roots_truncated,
            history_ok: !roots_truncated,
            appinfo_found: BTreeSet::new(),
            appinfo_typed: BTreeSet::new(),
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
                }
                Ok(false) => self.tasks.push_front(Task::AppInfo(scan)),
                Err(_) => self.appinfo_ok = false,
            }
            return false;
        }
        if let Some(Task::LocalConfig(path)) = self.tasks.pop_front() {
            match read_localconfig(&path, games) {
                Ok(last_played) => {
                    for (app_id, timestamp) in last_played {
                        if let Some(game) = games.get_mut(&app_id) {
                            game.last_played =
                                Some(game.last_played.unwrap_or_default().max(timestamp));
                        }
                    }
                }
                Err(_) => self.history_ok = false,
            }
            return false;
        }
        if let Some(root) = self.roots.pop_front() {
            self.process_root(root);
            return false;
        }

        let required_apps = games
            .values()
            .filter(|game| game.installed)
            .map(|game| game.app_id)
            .collect::<BTreeSet<_>>();
        self.availability = Availability {
            appinfo_complete: self.appinfo_files > 0
                && self.appinfo_ok
                && required_apps.is_subset(&self.appinfo_found)
                && required_apps.is_subset(&self.appinfo_typed),
            history_complete: self.history_roots > 0 && self.history_ok,
        };
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

        if let Some(metadata) = appinfo_metadata {
            self.appinfo_files += 1;
            match regular_metadata(&appinfo_path, metadata) {
                Ok(()) => match AppInfoScan::open(&appinfo_path) {
                    Ok(scan) => self.tasks.push_back(Task::AppInfo(scan)),
                    Err(_) => self.appinfo_ok = false,
                },
                Err(_) => self.appinfo_ok = false,
            }
        } else {
            self.appinfo_ok = false;
        }

        match userdata_metadata {
            Some(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                self.history_roots += 1;
                self.enqueue_user_configs(&userdata_path);
            }
            _ => self.history_ok = false,
        }
    }

    fn enqueue_user_configs(&mut self, userdata: &Path) {
        let Ok(entries) = fs::read_dir(userdata) else {
            self.history_ok = false;
            return;
        };
        let mut users = Vec::new();
        let mut accounts_seen = 0;
        for entry in entries {
            let Ok(entry) = entry else {
                self.history_ok = false;
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
                break;
            }
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                self.history_ok = false;
                continue;
            };
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                users.push(entry.path());
            } else {
                self.history_ok = false;
            }
        }
        users.sort();
        for user in users {
            if self.localconfig_tasks >= MAX_LOCALCONFIG_TASKS {
                self.history_ok = false;
                break;
            }
            let path = user.join("config/localconfig.vdf");
            self.localconfig_tasks += 1;
            self.tasks.push_back(Task::LocalConfig(path));
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

fn read_localconfig(
    path: &Path,
    games: &BTreeMap<u32, Game>,
) -> Result<BTreeMap<u32, u64>, String> {
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
    let mut last_played = BTreeMap::new();
    for (raw_app_id, app) in apps {
        let Ok(app_id) = raw_app_id.parse::<u32>() else {
            continue;
        };
        if app_id == 0 || app_id.to_string() != *raw_app_id {
            continue;
        }
        // Other apps can have malformed local history without affecting this
        // scan's installed games. Steam's localconfig is shared by all apps.
        if !games.get(&app_id).is_some_and(|game| game.installed) {
            continue;
        }
        let Value::Object(fields) = app else {
            return Err("Steam localconfig app data must be an object".into());
        };
        let timestamp = match fields.get("lastplayed") {
            Some(Value::Text(value)) => value
                .parse::<u64>()
                .map_err(|_| "Steam localconfig has an invalid LastPlayed timestamp")?,
            Some(_) => return Err("Steam localconfig LastPlayed must be text".into()),
            None => 0,
        };
        last_played.insert(app_id, timestamp);
    }
    Ok(last_played)
}

struct AppInfoScan {
    file: File,
    string_table_offset: u64,
    offset: u64,
    keys: Vec<String>,
    found_records: BTreeSet<u32>,
    found_types: BTreeSet<u32>,
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
            if let Some(game) = games.get_mut(&app_id).filter(|game| game.installed) {
                if !self.found_records.insert(app_id) {
                    return Err("Steam appinfo contains a duplicate app ID".into());
                }
                let value_bytes = entry_size - 60;
                if value_bytes > MAX_APPINFO_RECORD_BYTES {
                    return Err("Steam appinfo record exceeds its byte limit".into());
                }
                self.file
                    .seek(SeekFrom::Current(60))
                    .map_err(|error| error.to_string())?;
                let mut data = vec![0u8; value_bytes as usize];
                self.file
                    .read_exact(&mut data)
                    .map_err(|error| error.to_string())?;
                let mut cursor = 0;
                let mut nodes = 0;
                let root =
                    parse_binary_object(&data, &mut cursor, data.len(), &self.keys, 0, &mut nodes)?;
                if cursor != data.len() {
                    return Err("Steam appinfo record has trailing binary data".into());
                }
                let metadata = root.field("appinfo").unwrap_or(&root);
                let common = metadata.field("common")?;
                let app_type = common.field("type")?.text()?.trim();
                if app_type.is_empty()
                    || app_type.len() > 64
                    || app_type.chars().any(char::is_control)
                {
                    return Err("Steam appinfo type is invalid".into());
                }
                game.app_type = Some(app_type.to_owned());
                self.found_types.insert(app_id);
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
            let mut records = Vec::new();
            for (app_id, app_type) in entries {
                let mut object = vec![0x00];
                push_u32(&mut object, 0); // appinfo
                object.push(0x00);
                push_u32(&mut object, 1); // common
                object.push(0x01);
                push_u32(&mut object, 2); // type
                object.extend_from_slice(app_type.as_bytes());
                object.push(0);
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
            push_u32(&mut bytes, 3);
            for key in ["appinfo", "common", "type"] {
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
