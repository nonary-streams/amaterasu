//! Bounded, read-only discovery of the current user's installed Lutris games.

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::thread;
use std::time::{Duration, Instant};

const MAX_DATABASE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_GAMES: usize = 10_000;
const MAX_SCAN_BYTES: usize = 24 * 1024 * 1024;
const MAX_SCAN_TIME: Duration = Duration::from_secs(8);
#[cfg(target_os = "linux")]
const FLATPAK_APP_ID: &str = "net.lutris.Lutris";
#[cfg(target_os = "linux")]
const FLATPAK_PROBE_TIME: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchKind {
    Native,
    FlatpakUser,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Game {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub runner: String,
    pub platform: String,
    pub directory: String,
    pub config_path: String,
    pub service: String,
    pub service_id: String,
}

impl Game {
    pub fn steam_backed(&self) -> bool {
        self.runner.eq_ignore_ascii_case("steam") || self.service.eq_ignore_ascii_case("steam")
    }

    pub fn steam_app_id(&self) -> Option<u32> {
        self.steam_backed()
            .then(|| self.service_id.parse::<u32>().ok())
            .flatten()
            .filter(|id| *id > 0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub database_path: PathBuf,
    pub lutris_executable: PathBuf,
    pub launch_kind: LaunchKind,
    pub games: Vec<Game>,
    pub artwork_revisions: BTreeMap<i64, String>,
    pub artwork_scan_incomplete: bool,
    pub artwork_scanned_games: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Availability {
    pub supported_platform: bool,
    pub database_available: bool,
    pub executable_available: bool,
}

pub fn discover() -> Result<Snapshot, String> {
    if !cfg!(target_os = "linux") {
        return Err("unsupported_platform".into());
    }
    let (database_path, lutris_executable, launch_kind) = discover_source()?;
    scan_source(database_path, lutris_executable, launch_kind)
}

fn scan_source(
    database_path: PathBuf,
    lutris_executable: PathBuf,
    launch_kind: LaunchKind,
) -> Result<Snapshot, String> {
    let games = scan_database(&database_path)?;
    let mut snapshot = Snapshot {
        database_path,
        lutris_executable,
        launch_kind,
        games,
        artwork_revisions: BTreeMap::new(),
        artwork_scan_incomplete: false,
        artwork_scanned_games: 0,
    };
    let artwork = crate::artwork::ArtworkStore::revisions(&snapshot);
    snapshot.artwork_revisions = artwork.revisions;
    snapshot.artwork_scan_incomplete = artwork.incomplete;
    snapshot.artwork_scanned_games = artwork.scanned_games;
    Ok(snapshot)
}

pub fn availability() -> Availability {
    let supported_platform = cfg!(target_os = "linux");
    #[cfg(target_os = "linux")]
    let flatpak_database = effective_uid().filter(|uid| *uid != 0).and_then(|uid| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .and_then(|home| flatpak_database_path(&home, uid))
    });
    #[cfg(not(target_os = "linux"))]
    let flatpak_database: Option<PathBuf> = None;
    #[cfg(target_os = "linux")]
    let flatpak_executable = find_flatpak_executable();
    #[cfg(not(target_os = "linux"))]
    let flatpak_executable: Option<PathBuf> = None;
    Availability {
        supported_platform,
        database_available: supported_platform
            && (default_database_path().is_some() || flatpak_database.is_some()),
        executable_available: supported_platform
            && (find_lutris_executable().is_some() || flatpak_executable.is_some()),
    }
}

fn discover_source() -> Result<(PathBuf, PathBuf, LaunchKind), String> {
    let native_client = find_lutris_executable();
    let native_database = default_database_path();
    if native_client.is_some() && native_database.is_some() {
        return select_source(native_database, native_client, None, None, false);
    }
    #[cfg(target_os = "linux")]
    {
        let uid = effective_uid()
            .filter(|uid| *uid != 0)
            .ok_or_else(|| "lutris_executable_unavailable".to_owned())?;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "lutris_database_unavailable".to_owned())?;
        let database = flatpak_database_path(&home, uid)
            .ok_or_else(|| "lutris_database_unavailable".to_owned())?;
        let client =
            find_flatpak_executable().ok_or_else(|| "lutris_executable_unavailable".to_owned())?;
        if !flatpak_user_app_installed(&client) {
            return Err("lutris_flatpak_unavailable".to_owned());
        }
        return select_source(None, None, Some(database), Some(client), true);
    }
    #[cfg(not(target_os = "linux"))]
    Err("lutris_executable_unavailable".to_owned())
}

fn select_source(
    native_database: Option<PathBuf>,
    native_client: Option<PathBuf>,
    flatpak_database: Option<PathBuf>,
    flatpak_client: Option<PathBuf>,
    flatpak_installed: bool,
) -> Result<(PathBuf, PathBuf, LaunchKind), String> {
    if let (Some(database), Some(client)) = (native_database, native_client) {
        return Ok((database, client, LaunchKind::Native));
    }
    if !flatpak_installed {
        return Err("lutris_flatpak_unavailable".to_owned());
    }
    let database = flatpak_database.ok_or_else(|| "lutris_database_unavailable".to_owned())?;
    let client = flatpak_client.ok_or_else(|| "lutris_executable_unavailable".to_owned())?;
    Ok((database, client, LaunchKind::FlatpakUser))
}

#[cfg(target_os = "linux")]
fn flatpak_database_path(home: &Path, uid: u32) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let home = home.is_absolute().then_some(home)?.canonicalize().ok()?;
    if std::fs::metadata(&home).ok()?.uid() != uid {
        return None;
    }
    let data_root = home
        .join(".var/app")
        .join(FLATPAK_APP_ID)
        .join("data/lutris")
        .canonicalize()
        .ok()?;
    if !data_root.starts_with(&home) {
        return None;
    }
    let candidate = data_root.join("pga.db");
    let database = candidate.canonicalize().ok()?;
    if !database.starts_with(&data_root)
        || !is_regular_file(&database)
        || std::fs::metadata(&database).ok()?.uid() != uid
    {
        return None;
    }
    Some(database)
}

#[cfg(target_os = "linux")]
fn find_flatpak_executable() -> Option<PathBuf> {
    select_flatpak_executable(
        &[
            PathBuf::from("/usr/bin/flatpak"),
            PathBuf::from("/usr/local/bin/flatpak"),
        ],
        &verified_system_executable,
    )
}

#[cfg(any(test, target_os = "linux"))]
fn select_flatpak_executable(
    candidates: &[PathBuf],
    verify: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Option<PathBuf> {
    let mut selected = None;
    for candidate in candidates {
        if let Some(executable) = verify(candidate) {
            if selected.as_ref().is_some_and(|prior| prior != &executable) {
                return None;
            }
            selected = Some(executable);
        }
    }
    selected
}

#[cfg(target_os = "linux")]
fn flatpak_user_app_installed(executable: &Path) -> bool {
    // Query the same per-user installation that the projected launch will use.
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::fs::OpenOptionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};
    let nonce = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(time) => time.as_nanos(),
        Err(_) => return false,
    };
    let output_path = std::env::temp_dir().join(format!(
        "amaterasu-lutris-flatpak-probe-{}-{nonce}",
        std::process::id()
    ));
    let mut output_file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&output_path)
    {
        Ok(file) => file,
        Err(_) => return false,
    };
    let child_output = match output_file.try_clone() {
        Ok(file) => file,
        Err(_) => {
            let _ = std::fs::remove_file(&output_path);
            return false;
        }
    };
    let mut child = match Command::new(executable)
        .args(["info", "--user", "--show-ref", FLATPAK_APP_ID])
        .stdout(Stdio::from(child_output))
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            let _ = std::fs::remove_file(&output_path);
            return false;
        }
    };
    // The descriptors survive unlinking; a descendant cannot hold a pipe read open.
    if std::fs::remove_file(&output_path).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    }
    let deadline = Instant::now() + FLATPAK_PROBE_TIME;
    loop {
        if output_file
            .metadata()
            .map_or(true, |metadata| metadata.len() > 256)
        {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return false;
                }
                let mut output = Vec::new();
                return output_file.seek(SeekFrom::Start(0)).is_ok()
                    && output_file.take(257).read_to_end(&mut output).is_ok()
                    && valid_flatpak_ref(&output);
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(any(test, target_os = "linux"))]
fn valid_flatpak_ref(output: &[u8]) -> bool {
    if output.len() > 256 {
        return false;
    }
    let Ok(value) = std::str::from_utf8(output) else {
        return false;
    };
    let Some(reference) = value.strip_suffix('\n') else {
        return false;
    };
    let Some(rest) = reference.strip_prefix("app/net.lutris.Lutris/") else {
        return false;
    };
    let Some((arch, branch)) = rest.split_once('/') else {
        return false;
    };
    arch == std::env::consts::ARCH && branch == "stable"
}

pub fn default_database_path() -> Option<PathBuf> {
    let xdg_data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    resolve_database_path(xdg_data_home.as_deref(), home.as_deref())
}

fn resolve_database_path(xdg_data_home: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let xdg_candidate = xdg_data_home
        .filter(|path| path.is_absolute())
        .map(|path| path.join("lutris").join("pga.db"));
    if let Some(path) = xdg_candidate.filter(|path| is_regular_file(path)) {
        return path.canonicalize().ok();
    }
    let home_candidate = home.filter(|path| path.is_absolute()).map(|path| {
        path.join(".local")
            .join("share")
            .join("lutris")
            .join("pga.db")
    });
    home_candidate
        .filter(|path| is_regular_file(path))
        .and_then(|path| path.canonicalize().ok())
}

pub fn scan_database(path: &Path) -> Result<Vec<Game>, String> {
    let metadata = std::fs::metadata(path).map_err(|_| "lutris_database_unavailable".to_owned())?;
    if !metadata.is_file() || metadata.len() > MAX_DATABASE_BYTES {
        return Err("lutris_database_exceeds_bounds".into());
    }
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "lutris_database_unreadable".to_owned())?;
    connection
        .busy_timeout(Duration::from_secs(1))
        .map_err(|_| "lutris_database_unreadable".to_owned())?;
    connection
        .execute_batch("PRAGMA query_only = ON;")
        .map_err(|_| "lutris_database_unreadable".to_owned())?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(|_| "lutris_database_unreadable".to_owned())?;
    let mut statement = transaction
        .prepare(
            "SELECT id, name, slug, runner, platform, directory, configpath, service, service_id \
             FROM games WHERE installed = 1 AND configpath IS NOT NULL AND configpath <> '' \
             ORDER BY id",
        )
        .map_err(|_| "lutris_database_schema_unsupported".to_owned())?;
    let started = Instant::now();
    let mut rows = statement
        .query(params![])
        .map_err(|_| "lutris_database_query_failed".to_owned())?;
    let mut games = Vec::new();
    let mut seen_ids = BTreeSet::new();
    let mut total_bytes = 0_usize;
    loop {
        let row = rows
            .next()
            .map_err(|_| "lutris_database_query_failed".to_owned())?;
        let Some(row) = row else {
            break;
        };
        if started.elapsed() > MAX_SCAN_TIME || games.len() >= MAX_GAMES {
            return Err("lutris_scan_exceeds_bounds".into());
        }
        let game = Game {
            id: row
                .get(0)
                .map_err(|_| "lutris_database_record_invalid".to_owned())?,
            name: row
                .get::<_, Option<String>>(1)
                .map_err(|_| "lutris_database_record_invalid".to_owned())?
                .unwrap_or_default(),
            slug: row
                .get::<_, Option<String>>(2)
                .map_err(|_| "lutris_database_record_invalid".to_owned())?
                .unwrap_or_default(),
            runner: row
                .get::<_, Option<String>>(3)
                .map_err(|_| "lutris_database_record_invalid".to_owned())?
                .unwrap_or_default(),
            platform: row
                .get::<_, Option<String>>(4)
                .map_err(|_| "lutris_database_record_invalid".to_owned())?
                .unwrap_or_default(),
            directory: row
                .get::<_, Option<String>>(5)
                .map_err(|_| "lutris_database_record_invalid".to_owned())?
                .unwrap_or_default(),
            config_path: row
                .get::<_, Option<String>>(6)
                .map_err(|_| "lutris_database_record_invalid".to_owned())?
                .unwrap_or_default(),
            service: row
                .get::<_, Option<String>>(7)
                .map_err(|_| "lutris_database_record_invalid".to_owned())?
                .unwrap_or_default(),
            service_id: row
                .get::<_, Option<String>>(8)
                .map_err(|_| "lutris_database_record_invalid".to_owned())?
                .unwrap_or_default(),
        };
        validate_game(&game)?;
        if !seen_ids.insert(game.id) {
            return Err("lutris_database_has_duplicate_game_ids".into());
        }
        total_bytes = total_bytes
            .checked_add(game.name.len())
            .and_then(|bytes| bytes.checked_add(game.slug.len()))
            .and_then(|bytes| bytes.checked_add(game.runner.len()))
            .and_then(|bytes| bytes.checked_add(game.platform.len()))
            .and_then(|bytes| bytes.checked_add(game.directory.len()))
            .and_then(|bytes| bytes.checked_add(game.config_path.len()))
            .and_then(|bytes| bytes.checked_add(game.service.len()))
            .and_then(|bytes| bytes.checked_add(game.service_id.len()))
            .ok_or_else(|| "lutris_scan_exceeds_bounds".to_owned())?;
        if total_bytes > MAX_SCAN_BYTES {
            return Err("lutris_scan_exceeds_bounds".into());
        }
        games.push(game);
    }
    drop(rows);
    drop(statement);
    transaction
        .commit()
        .map_err(|_| "lutris_database_query_failed".to_owned())?;
    Ok(games)
}

fn validate_game(game: &Game) -> Result<(), String> {
    if game.id <= 0
        || game.name.trim().is_empty()
        || !bounded_field(&game.name, 160)
        || !bounded_field(&game.slug, 256)
        || !bounded_field(&game.runner, 128)
        || !bounded_field(&game.platform, 128)
        || !bounded_field(&game.directory, 2048)
        || game.config_path.is_empty()
        || !bounded_field(&game.config_path, 2048)
        || !bounded_field(&game.service, 128)
        || !bounded_field(&game.service_id, 128)
    {
        return Err("lutris_database_record_invalid".into());
    }
    Ok(())
}

fn bounded_field(value: &str, bytes: usize) -> bool {
    value.len() <= bytes && !value.chars().any(char::is_control)
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

#[cfg(target_os = "linux")]
fn find_lutris_executable() -> Option<PathBuf> {
    let effective_uid = effective_uid()?;
    // A machine service has no trusted logged-in desktop identity. The session
    // scanner/launcher that Vibeshine uses is not part of this service yet.
    if effective_uid == 0 {
        return None;
    }
    let path_directories = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    let system_candidates = [
        PathBuf::from("/usr/bin/lutris"),
        PathBuf::from("/usr/local/bin/lutris"),
    ];
    select_lutris_executable(
        &path_directories,
        &system_candidates,
        &|path| verified_user_executable(path, effective_uid),
        &verified_system_executable,
    )
}

#[cfg(any(test, target_os = "linux"))]
pub(crate) fn select_lutris_executable(
    path_directories: &[PathBuf],
    system_candidates: &[PathBuf],
    verify_path: &dyn Fn(&Path) -> Option<PathBuf>,
    verify_system: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Option<PathBuf> {
    for directory in path_directories.iter().filter(|path| path.is_absolute()) {
        if let Some(executable) = verify_path(&directory.join("lutris")) {
            return Some(executable);
        }
    }
    let mut fallback = None;
    for candidate in system_candidates {
        if let Some(executable) = verify_system(candidate) {
            match &fallback {
                Some(previous) if previous != &executable => return None,
                None => fallback = Some(executable),
                _ => {}
            }
        }
    }
    fallback
}

#[cfg(target_os = "linux")]
fn verified_user_executable(candidate: &Path, effective_uid: u32) -> Option<PathBuf> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let path = candidate.canonicalize().ok()?;
    let metadata = std::fs::metadata(&path).ok()?;
    let mode = metadata.permissions().mode();
    (metadata.is_file()
        && mode & 0o022 == 0
        && (metadata.uid() == effective_uid || metadata.uid() == 0)
        && ((metadata.uid() == effective_uid && mode & 0o100 != 0) || mode & 0o001 != 0))
        .then_some(path)
}

#[cfg(target_os = "linux")]
fn verified_system_executable(candidate: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let path = candidate.canonicalize().ok()?;
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o022 != 0
        || metadata.permissions().mode() & 0o001 == 0
    {
        return None;
    }
    for parent in [candidate.parent()?, path.parent()?] {
        for ancestor in parent.ancestors() {
            let metadata = std::fs::metadata(ancestor).ok()?;
            if !metadata.is_dir()
                || metadata.uid() != 0
                || metadata.permissions().mode() & 0o022 != 0
            {
                return None;
            }
        }
    }
    Some(path)
}

#[cfg(target_os = "linux")]
fn effective_uid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(not(target_os = "linux"))]
fn find_lutris_executable() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_database_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("amaterasu-lutris-{nonce}.db"))
    }

    fn create_database(path: &Path) -> Connection {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE games (
                    id INTEGER,
                    name TEXT,
                    slug TEXT,
                    runner TEXT,
                    platform TEXT,
                    directory TEXT,
                    configpath TEXT,
                    service TEXT,
                    service_id TEXT,
                    installed INTEGER
                );",
            )
            .unwrap();
        connection
    }

    #[test]
    fn resolves_only_the_calling_users_supported_database_locations() {
        let home = std::env::temp_dir().join("lutris-home-test");
        let xdg = std::env::temp_dir().join("lutris-xdg-test");
        let home_db = home.join(".local/share/lutris/pga.db");
        let xdg_db = xdg.join("lutris/pga.db");
        std::fs::create_dir_all(home_db.parent().unwrap()).unwrap();
        std::fs::create_dir_all(xdg_db.parent().unwrap()).unwrap();
        std::fs::write(&home_db, []).unwrap();
        std::fs::write(&xdg_db, []).unwrap();
        assert_eq!(
            resolve_database_path(Some(&xdg), Some(&home)),
            xdg_db.canonicalize().ok()
        );
        std::fs::remove_file(&xdg_db).unwrap();
        assert_eq!(
            resolve_database_path(Some(&xdg), Some(&home)),
            home_db.canonicalize().ok()
        );
        assert_eq!(
            resolve_database_path(Some(Path::new("relative")), None),
            None
        );
        std::fs::remove_dir_all(home).unwrap();
        std::fs::remove_dir_all(xdg).unwrap();
    }

    #[test]
    fn native_source_precedes_flatpak_and_failed_flatpak_identity_cannot_import() {
        let native_database = PathBuf::from("/native/pga.db");
        let flatpak_database = PathBuf::from("/flatpak/pga.db");
        let native_client = PathBuf::from("/usr/bin/lutris");
        let flatpak_client = PathBuf::from("/usr/bin/flatpak");
        assert_eq!(
            select_source(
                Some(native_database.clone()),
                Some(native_client.clone()),
                Some(flatpak_database.clone()),
                Some(flatpak_client.clone()),
                true
            ),
            Ok((
                native_database.clone(),
                native_client.clone(),
                LaunchKind::Native
            ))
        );
        assert_eq!(
            select_source(
                None,
                None,
                Some(flatpak_database.clone()),
                Some(flatpak_client.clone()),
                true
            ),
            Ok((
                flatpak_database.clone(),
                flatpak_client.clone(),
                LaunchKind::FlatpakUser
            ))
        );
        assert_eq!(
            select_source(
                None,
                Some(native_client.clone()),
                Some(flatpak_database.clone()),
                Some(flatpak_client.clone()),
                true
            ),
            Ok((
                flatpak_database.clone(),
                flatpak_client.clone(),
                LaunchKind::FlatpakUser
            ))
        );
        assert_eq!(
            select_source(
                None,
                None,
                Some(flatpak_database.clone()),
                Some(flatpak_client.clone()),
                false
            ),
            Err("lutris_flatpak_unavailable".into())
        );
        assert_eq!(
            select_source(None, None, None, Some(flatpak_client), true),
            Err("lutris_database_unavailable".into())
        );
        assert_eq!(
            select_source(
                Some(native_database),
                Some(PathBuf::from("/usr/bin/lutris")),
                Some(flatpak_database),
                None,
                true
            )
            .unwrap()
            .2,
            LaunchKind::Native
        );
        assert_eq!(
            select_flatpak_executable(&[PathBuf::from("/one"), PathBuf::from("/two")], &|path| {
                Some(path.to_path_buf())
            }),
            None
        );
        assert!(valid_flatpak_ref(
            format!("app/net.lutris.Lutris/{}/stable\n", std::env::consts::ARCH).as_bytes()
        ));
        for bad in [
            b"app/net.lutris.Lutris/x86_64/beta\n".as_slice(),
            b"app/other/x86_64/stable\n",
            b"app/net.lutris.Lutris/x86_64/stable\nother\n",
            b"app/net.lutris.Lutris/x86_64/stable",
        ] {
            assert!(!valid_flatpak_ref(bad));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn flatpak_source_requires_current_user_database_and_bounded_installed_app_probe() {
        use std::os::unix::fs::PermissionsExt;
        let uid = effective_uid().unwrap();
        let root = std::env::temp_dir().join(format!(
            "amaterasu-lutris-flatpak-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = root.join("home");
        let database = home.join(".var/app/net.lutris.Lutris/data/lutris/pga.db");
        std::fs::create_dir_all(database.parent().unwrap()).unwrap();
        let connection = create_database(&database);
        connection.execute("INSERT INTO games (id,name,slug,runner,platform,directory,configpath,service,service_id,installed) VALUES (7,'Example','example','wine','linux','','game.yml','','',1)", []).unwrap();
        drop(connection);
        let resolved = flatpak_database_path(&home, uid).unwrap();
        let launcher = root.join("flatpak");
        std::fs::write(
            &launcher,
            "#!/bin/sh\nprintf 'app/net.lutris.Lutris/x86_64/stable\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(flatpak_user_app_installed(&launcher));
        let source =
            scan_source(resolved.clone(), launcher.clone(), LaunchKind::FlatpakUser).unwrap();
        assert_eq!(source.games[0].id, 7);
        assert_eq!(source.launch_kind, LaunchKind::FlatpakUser);
        assert!(flatpak_database_path(&home, uid.saturating_add(1)).is_none());
        std::fs::write(&launcher, "#!/bin/sh\nsleep 5\n").unwrap();
        let started = Instant::now();
        assert!(!flatpak_user_app_installed(&launcher));
        assert!(started.elapsed() < Duration::from_secs(4));
        std::fs::write(
            &launcher,
            "#!/bin/sh\nprintf 'app/other/x86_64/stable\\n'\n",
        )
        .unwrap();
        assert!(!flatpak_user_app_installed(&launcher));
        std::fs::write(&launcher, "#!/bin/sh\nexit 1\n").unwrap();
        assert!(!flatpak_user_app_installed(&launcher));
        std::fs::write(&launcher, "#!/bin/sh\nprintf '%0300d' 0\n").unwrap();
        assert!(!flatpak_user_app_installed(&launcher));
        let outside = root.join("outside.db");
        std::fs::rename(&database, &outside).unwrap();
        std::os::unix::fs::symlink(&outside, &database).unwrap();
        assert!(flatpak_database_path(&home, uid).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scans_only_installed_configured_rows_from_a_read_only_snapshot() {
        let path = temp_database_path();
        let connection = create_database(&path);
        connection
            .execute(
                "INSERT INTO games (id,name,slug,runner,platform,directory,configpath,service,service_id,installed)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![7, "Example", "example", "wine", "linux", "/games/example", "/games/example.yml", "", "", 1],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO games (id,name,slug,runner,platform,directory,configpath,service,service_id,installed)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![8, "Not installed", "missing", "wine", "linux", "", "/games/missing.yml", "", "", 0],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO games (id,name,slug,runner,platform,directory,configpath,service,service_id,installed)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![9, "No config", "no-config", "wine", "linux", "", "", "", "", 1],
            )
            .unwrap();
        drop(connection);
        let games = scan_database(&path).unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].id, 7);
        assert_eq!(games[0].name, "Example");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn refuses_missing_schema_duplicate_ids_and_invalid_catalog_names() {
        let missing_schema = temp_database_path();
        drop(Connection::open(&missing_schema).unwrap());
        assert_eq!(
            scan_database(&missing_schema),
            Err("lutris_database_schema_unsupported".into())
        );
        std::fs::remove_file(missing_schema).unwrap();

        let path = temp_database_path();
        let connection = create_database(&path);
        for (id, name) in [(7_i64, "Game"), (7_i64, "Duplicate")] {
            connection
                .execute(
                    "INSERT INTO games (id,name,slug,runner,platform,directory,configpath,service,service_id,installed)
                     VALUES (?1,?2,'slug','wine','linux','/games','/games/game.yml','','',1)",
                    params![id, name],
                )
                .unwrap();
        }
        drop(connection);
        assert_eq!(
            scan_database(&path),
            Err("lutris_database_has_duplicate_game_ids".into())
        );
        std::fs::remove_file(path).unwrap();

        let path = temp_database_path();
        let connection = create_database(&path);
        connection
            .execute(
                "INSERT INTO games (id,name,slug,runner,platform,directory,configpath,service,service_id,installed)
                 VALUES (7,?1,'slug','wine','linux','/games','/games/game.yml','','',1)",
                params!["x".repeat(161)],
            )
            .unwrap();
        drop(connection);
        assert_eq!(
            scan_database(&path),
            Err("lutris_database_record_invalid".into())
        );
        std::fs::remove_file(path).unwrap();
    }
}
