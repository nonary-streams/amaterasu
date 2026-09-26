//! Bounded, read-only discovery of the current user's installed Lutris games.

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_DATABASE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_GAMES: usize = 10_000;
const MAX_SCAN_BYTES: usize = 24 * 1024 * 1024;
const MAX_SCAN_TIME: Duration = Duration::from_secs(8);

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
    let database_path =
        default_database_path().ok_or_else(|| "lutris_database_unavailable".to_owned())?;
    let lutris_executable =
        find_lutris_executable().ok_or_else(|| "lutris_executable_unavailable".to_owned())?;
    let games = scan_database(&database_path)?;
    let mut snapshot = Snapshot {
        database_path,
        lutris_executable,
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
    Availability {
        supported_platform,
        database_available: supported_platform && default_database_path().is_some(),
        executable_available: supported_platform && find_lutris_executable().is_some(),
    }
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
