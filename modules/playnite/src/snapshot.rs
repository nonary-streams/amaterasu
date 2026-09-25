use crate::settings::ProductSettings;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_GAMES: usize = 50_000;
pub const MAX_CATEGORIES: usize = 4_096;
pub const MAX_PLUGINS: usize = 4_096;
pub const MAX_LINE_BYTES: usize = 1_048_576;
pub const SOURCE_RECENT: u32 = 1 << 0;
pub const SOURCE_CATEGORY: u32 = 1 << 1;
pub const SOURCE_PLUGIN: u32 = 1 << 2;
pub const SOURCE_INSTALLED: u32 = 1 << 3;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedGame {
    pub game: Game,
    pub source_flags: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Game {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub exe: String,
    #[serde(default)]
    pub args: String,
    #[serde(default, rename = "workingDir")]
    pub working_directory: String,
    #[serde(default, rename = "installDir")]
    pub install_directory: String,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default, rename = "pluginId")]
    pub plugin_id: String,
    #[serde(default, rename = "pluginName")]
    pub plugin_name: String,
    #[serde(default, rename = "lastPlayed")]
    pub last_played: String,
    #[serde(default)]
    pub installed: Option<bool>,
    #[serde(default, rename = "isInstalled")]
    pub is_installed: Option<bool>,
}

impl Game {
    pub fn is_installed(&self) -> Option<bool> {
        match (self.installed, self.is_installed) {
            (Some(installed), Some(is_installed)) => Some(installed || is_installed),
            (Some(installed), None) => Some(installed),
            (None, Some(is_installed)) => Some(is_installed),
            (None, None) => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Category {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Plugin {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub games: Vec<Game>,
    pub categories: Vec<Category>,
    pub plugins: Vec<Plugin>,
}

#[derive(Default)]
pub struct SnapshotAssembler {
    current: Option<IncompleteSnapshot>,
}

#[derive(Default)]
struct IncompleteSnapshot {
    games: Vec<Game>,
    ids: BTreeSet<String>,
    categories: Option<Vec<Category>>,
    plugins: Option<Vec<Plugin>>,
    invalid: bool,
}

impl SnapshotAssembler {
    /// Consumes one newline-delimited extension message. A snapshot is exposed
    /// only when both lookup lists and the declared game count are complete.
    pub fn receive_line(&mut self, line: &[u8]) -> Option<Result<Snapshot, &'static str>> {
        if line.is_empty() || line.len() > MAX_LINE_BYTES {
            if let Some(current) = &mut self.current {
                current.invalid = true;
            }
            return None;
        }
        let Ok(message) = serde_json::from_slice::<serde_json::Value>(line) else {
            if let Some(current) = &mut self.current {
                current.invalid = true;
            }
            return None;
        };
        let Some(kind) = message.get("type").and_then(serde_json::Value::as_str) else {
            if let Some(current) = &mut self.current {
                current.invalid = true;
            }
            return None;
        };
        match kind {
            "snapshotStart" => {
                self.current = Some(IncompleteSnapshot::default());
                None
            }
            "plugins" => self.receive_plugins(message.get("payload")),
            "categories" => self.receive_categories(message.get("payload")),
            "games" => self.receive_games(message.get("payload")),
            "snapshotComplete" => self.finish(message.get("games")),
            "status" | "commandResult" | "hello" => None,
            _ => {
                if let Some(current) = &mut self.current {
                    current.invalid = true;
                }
                None
            }
        }
    }

    fn receive_plugins(
        &mut self,
        payload: Option<&serde_json::Value>,
    ) -> Option<Result<Snapshot, &'static str>> {
        let Some(current) = &mut self.current else {
            return None;
        };
        let Some(plugins) =
            payload.and_then(|payload| serde_json::from_value::<Vec<Plugin>>(payload.clone()).ok())
        else {
            current.invalid = true;
            return None;
        };
        if plugins.len() > MAX_PLUGINS
            || plugins.iter().any(|plugin| {
                (plugin.id.is_empty() && plugin.name.is_empty())
                    || plugin.id.len() > 512
                    || plugin.name.len() > 512
                    || plugin.id.chars().any(char::is_control)
                    || plugin.name.chars().any(char::is_control)
            })
            || current.plugins.replace(plugins).is_some()
        {
            current.invalid = true;
        }
        None
    }

    fn receive_categories(
        &mut self,
        payload: Option<&serde_json::Value>,
    ) -> Option<Result<Snapshot, &'static str>> {
        let Some(current) = &mut self.current else {
            return None;
        };
        let Some(categories) = payload
            .and_then(|payload| serde_json::from_value::<Vec<Category>>(payload.clone()).ok())
        else {
            current.invalid = true;
            return None;
        };
        if categories.len() > MAX_CATEGORIES
            || categories.iter().any(|category| {
                (category.id.is_empty() && category.name.is_empty())
                    || category.id.len() > 512
                    || category.name.len() > 512
                    || category.id.chars().any(char::is_control)
                    || category.name.chars().any(char::is_control)
            })
            || current.categories.replace(categories).is_some()
        {
            current.invalid = true;
        }
        None
    }

    fn receive_games(
        &mut self,
        payload: Option<&serde_json::Value>,
    ) -> Option<Result<Snapshot, &'static str>> {
        let Some(current) = &mut self.current else {
            return None;
        };
        let Some(games) =
            payload.and_then(|payload| serde_json::from_value::<Vec<Game>>(payload.clone()).ok())
        else {
            current.invalid = true;
            return None;
        };
        if current.games.len().saturating_add(games.len()) > MAX_GAMES {
            current.invalid = true;
            return None;
        }
        for game in games {
            let id = game.id.to_ascii_lowercase();
            if game.id.trim().is_empty()
                || game.id.len() > 512
                || game.id.chars().any(char::is_control)
                || game.name.len() > 1_024
                || game.name.chars().any(char::is_control)
                || game.is_installed().is_none()
                || game.categories.len() > MAX_CATEGORIES
                || game
                    .categories
                    .iter()
                    .any(|category| category.len() > 512 || category.chars().any(char::is_control))
                || !current.ids.insert(id)
            {
                current.invalid = true;
                continue;
            }
            current.games.push(game);
        }
        None
    }

    fn finish(
        &mut self,
        declared_count: Option<&serde_json::Value>,
    ) -> Option<Result<Snapshot, &'static str>> {
        let current = self.current.take()?;
        let Some(expected_count) = declared_count.and_then(serde_json::Value::as_u64) else {
            return Some(Err("snapshot completion omitted a valid game count"));
        };
        if current.invalid
            || current.games.len() as u64 != expected_count
            || current.categories.is_none()
            || current.plugins.is_none()
        {
            return Some(Err("Playnite snapshot was incomplete or malformed"));
        }
        Some(Ok(Snapshot {
            games: current.games,
            categories: current.categories.unwrap_or_default(),
            plugins: current.plugins.unwrap_or_default(),
        }))
    }
}

#[cfg(test)]
pub fn select_games(snapshot: &Snapshot, settings: &ProductSettings, now_unix: i64) -> Vec<Game> {
    select_game_sources(snapshot, settings, now_unix)
        .into_iter()
        .map(|selected| selected.game)
        .collect()
}

pub fn select_game_sources(
    snapshot: &Snapshot,
    settings: &ProductSettings,
    now_unix: i64,
) -> Vec<SelectedGame> {
    let mut installed: Vec<&Game> = snapshot
        .games
        .iter()
        .filter(|game| game.is_installed() == Some(true))
        .collect();
    installed.sort_by(|left, right| right.last_played.cmp(&left.last_played));

    let included_categories = ascii_lower_set(&settings.sync_categories);
    let excluded_categories = ascii_lower_set(&settings.exclude_categories);
    let included_plugins = ascii_lower_set(&settings.sync_plugins);
    let excluded_plugins = ascii_lower_set(&settings.exclude_plugins);
    let excluded_games = ascii_lower_set(&settings.exclude_games);
    let mut selected = BTreeMap::<String, (&Game, u32)>::new();

    if settings.recent_games > 0 {
        let cutoff = i64::from(settings.recent_max_age_days)
            .checked_mul(86_400)
            .and_then(|age| now_unix.checked_sub(age));
        let mut recent_count = 0;
        for game in &installed {
            if is_excluded(
                game,
                &excluded_games,
                &excluded_categories,
                &excluded_plugins,
            ) {
                continue;
            }
            if settings.recent_max_age_days > 0
                && !parse_iso8601_unix(&game.last_played)
                    .zip(cutoff)
                    .is_some_and(|(last_played, cutoff)| last_played >= cutoff)
            {
                continue;
            }
            selected
                .entry(game.id.to_ascii_lowercase())
                .and_modify(|(_, source_flags)| *source_flags |= SOURCE_RECENT)
                .or_insert((game, SOURCE_RECENT));
            recent_count += 1;
            if recent_count == settings.recent_games as usize {
                break;
            }
        }
    }

    for game in &installed {
        if is_excluded(
            game,
            &excluded_games,
            &excluded_categories,
            &excluded_plugins,
        ) {
            continue;
        }
        let mut source_flags = 0;
        if settings.sync_all_installed {
            source_flags |= SOURCE_INSTALLED;
        }
        if game
            .categories
            .iter()
            .any(|category| included_categories.contains(&category.to_ascii_lowercase()))
        {
            source_flags |= SOURCE_CATEGORY;
        }
        if included_plugins.contains(&game.plugin_id.to_ascii_lowercase()) {
            source_flags |= SOURCE_PLUGIN;
        }
        if source_flags != 0 {
            selected
                .entry(game.id.to_ascii_lowercase())
                .and_modify(|(_, selected_flags)| *selected_flags |= source_flags)
                .or_insert((game, source_flags));
        }
    }

    selected
        .into_values()
        .map(|(game, source_flags)| SelectedGame {
            game: game.clone(),
            source_flags,
        })
        .collect()
}

fn is_excluded(
    game: &Game,
    excluded_games: &BTreeSet<String>,
    excluded_categories: &BTreeSet<String>,
    excluded_plugins: &BTreeSet<String>,
) -> bool {
    excluded_games.contains(&game.id.to_ascii_lowercase())
        || excluded_plugins.contains(&game.plugin_id.to_ascii_lowercase())
        || game
            .categories
            .iter()
            .any(|category| excluded_categories.contains(&category.to_ascii_lowercase()))
}

fn ascii_lower_set(values: &[String]) -> BTreeSet<String> {
    values
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

pub fn parse_iso8601_unix(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 19
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !matches!(bytes.get(10), Some(b'T' | b't' | b' '))
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }
    let year = parse_decimal(&bytes[0..4])? as i64;
    let month = parse_decimal(&bytes[5..7])? as i64;
    let day = parse_decimal(&bytes[8..10])? as i64;
    let hour = parse_decimal(&bytes[11..13])? as i64;
    let minute = parse_decimal(&bytes[14..16])? as i64;
    let second = parse_decimal(&bytes[17..19])? as i64;
    if !(1..=12).contains(&month)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
        || day == 0
        || day > days_in_month(year, month)
    {
        return None;
    }
    let mut position = 19;
    if bytes.get(position) == Some(&b'.') {
        position += 1;
        let start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if position == start {
            return None;
        }
    }
    let mut offset_seconds = 0_i64;
    match bytes.get(position) {
        None => {}
        Some(b'Z' | b'z') if position + 1 == bytes.len() => {}
        Some(sign @ (b'+' | b'-')) => {
            if bytes.len() != position + 6 || bytes.get(position + 3) != Some(&b':') {
                return None;
            }
            let hours = parse_decimal(&bytes[position + 1..position + 3])? as i64;
            let minutes = parse_decimal(&bytes[position + 4..position + 6])? as i64;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let direction = if *sign == b'+' { 1 } else { -1 };
            offset_seconds = direction * (hours * 3_600 + minutes * 60);
        }
        _ => return None,
    }

    let days = days_from_civil(year, month, day)?;
    days.checked_mul(86_400)?
        .checked_add(hour * 3_600 + minute * 60 + second)?
        .checked_sub(offset_seconds)
}

fn parse_decimal(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    bytes.iter().try_fold(0_u32, |value, byte| {
        value.checked_mul(10)?.checked_add(u32::from(*byte - b'0'))
    })
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    let year = year.checked_sub(i64::from(month <= 2))?;
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

pub fn valid_playnite_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

pub fn playnite_launch(game: &Game) -> Option<serde_json::Value> {
    (game.name.trim().len() <= 160 && !game.name.trim().is_empty() && valid_playnite_id(&game.id))
        .then(|| {
            serde_json::json!({
                "kind":"playnite",
                "game_id":game.id.to_ascii_lowercase(),
                "exit_timeout_seconds":10
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(id: &str, categories: &[&str], plugin: &str, last_played: &str) -> Game {
        Game {
            id: id.into(),
            name: format!("Game {id}"),
            exe: format!(r"C:\Games\{id}\game.exe"),
            args: String::new(),
            working_directory: String::new(),
            install_directory: format!(r"C:\Games\{id}"),
            categories: categories.iter().map(|value| (*value).into()).collect(),
            plugin_id: plugin.into(),
            plugin_name: String::new(),
            last_played: last_played.into(),
            installed: Some(true),
            is_installed: None,
        }
    }

    #[test]
    fn only_a_bracketed_count_matched_snapshot_is_published() {
        let mut assembler = SnapshotAssembler::default();
        assembler.receive_line(br#"{"type":"plugins","payload":[]}"#);
        assembler.receive_line(br#"{"type":"snapshotStart"}"#);
        assembler.receive_line(br#"{"type":"plugins","payload":[]}"#);
        assembler.receive_line(br#"{"type":"categories","payload":[]}"#);
        assembler.receive_line(
            br#"{"type":"games","payload":[{"id":"a","name":"A","installed":true}]}"#,
        );
        let Some(Ok(snapshot)) =
            assembler.receive_line(br#"{"type":"snapshotComplete","games":1}"#)
        else {
            panic!("complete source snapshot should be published");
        };
        assert_eq!(snapshot.games.len(), 1);

        assembler.receive_line(br#"{"type":"snapshotStart"}"#);
        assembler.receive_line(br#"{"type":"plugins","payload":[]}"#);
        assembler.receive_line(br#"{"type":"categories","payload":[]}"#);
        assert_eq!(
            assembler.receive_line(br#"{"type":"snapshotComplete","games":2}"#),
            Some(Err("Playnite snapshot was incomplete or malformed"))
        );
    }

    #[test]
    fn malformed_or_ambiguous_games_poison_the_whole_snapshot() {
        let mut assembler = SnapshotAssembler::default();
        assembler.receive_line(br#"{"type":"snapshotStart"}"#);
        assembler.receive_line(br#"{"type":"plugins","payload":[]}"#);
        assembler.receive_line(br#"{"type":"categories","payload":[]}"#);
        assembler.receive_line(br#"{"type":"games","payload":[{"id":"a","name":"A"}]}"#);
        assert!(matches!(
            assembler.receive_line(br#"{"type":"snapshotComplete","games":1}"#),
            Some(Err(_))
        ));

        assembler.receive_line(br#"{"type":"snapshotStart"}"#);
        assembler.receive_line(br#"{"type":"plugins","payload":[]}"#);
        assembler.receive_line(br#"{"type":"categories","payload":[]}"#);
        assembler.receive_line(br#"{"type":"games","payload":[{"id":"a","name":"A","installed":true},{"id":"A","name":"A copy","installed":true}]}"#);
        assert!(matches!(
            assembler.receive_line(br#"{"type":"snapshotComplete","games":2}"#),
            Some(Err(_))
        ));
    }

    #[test]
    fn full_scan_filters_union_and_recent_age_match_product_policy() {
        let snapshot = Snapshot {
            games: vec![
                game("recent", &[], "p1", "2026-09-20T00:00:00Z"),
                game("category", &["Arcade"], "p2", "2025-01-01T00:00:00Z"),
                game("plugin", &[], "P3", ""),
                Game {
                    installed: Some(false),
                    ..game("uninstalled", &["Arcade"], "p1", "2026-09-21T00:00:00Z")
                },
                game("excluded", &["Arcade"], "p1", "2026-09-21T00:00:00Z"),
            ],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let settings = ProductSettings {
            recent_games: 10,
            recent_max_age_days: 30,
            sync_categories: vec!["arcade".into()],
            sync_plugins: vec!["p3".into()],
            exclude_games: vec!["EXCLUDED".into()],
            ..ProductSettings::default()
        };
        let selected = select_games(
            &snapshot,
            &settings,
            parse_iso8601_unix("2026-09-24T00:00:00Z").unwrap(),
        );
        let ids: BTreeSet<_> = selected.iter().map(|game| game.id.as_str()).collect();
        assert_eq!(ids, BTreeSet::from(["category", "plugin", "recent"]));
    }

    #[test]
    fn recent_max_age_cutoff_includes_boundary_and_excludes_one_second_older() {
        let snapshot = Snapshot {
            games: vec![
                game("boundary", &[], "p1", "2026-08-25T00:00:00Z"),
                game("too-old", &[], "p1", "2026-08-24T23:59:59Z"),
            ],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let settings = ProductSettings {
            recent_games: 10,
            recent_max_age_days: 30,
            ..ProductSettings::default()
        };
        let now = parse_iso8601_unix("2026-09-24T00:00:00Z").unwrap();

        let selected = select_games(&snapshot, &settings, now);
        let selected_ids: BTreeSet<_> = selected.iter().map(|game| game.id.as_str()).collect();

        assert_eq!(selected_ids, BTreeSet::from(["boundary"]));
    }

    #[test]
    fn selected_games_retain_reference_source_membership_bits() {
        let snapshot = Snapshot {
            games: vec![
                game("all", &["Arcade"], "p1", "2026-09-23T00:00:00Z"),
                game("recent", &[], "p2", "2026-09-22T00:00:00Z"),
            ],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let settings = ProductSettings {
            sync_categories: vec!["arcade".into()],
            sync_plugins: vec!["p1".into()],
            ..ProductSettings::default()
        };
        let selected = select_game_sources(
            &snapshot,
            &settings,
            parse_iso8601_unix("2026-09-24T00:00:00Z").unwrap(),
        );
        let flags: std::collections::BTreeMap<_, _> = selected
            .iter()
            .map(|item| (item.game.id.as_str(), item.source_flags))
            .collect();
        assert_eq!(
            flags.get("all"),
            Some(&(SOURCE_RECENT | SOURCE_CATEGORY | SOURCE_PLUGIN))
        );
        assert_eq!(flags.get("recent"), Some(&SOURCE_RECENT));

        let installed = select_game_sources(
            &snapshot,
            &ProductSettings {
                recent_games: 0,
                sync_all_installed: true,
                ..ProductSettings::default()
            },
            0,
        );
        assert!(
            installed
                .iter()
                .all(|item| item.source_flags == SOURCE_INSTALLED)
        );
    }

    #[test]
    fn category_exclusions_use_ascii_case_insensitive_matching() {
        let snapshot = Snapshot {
            games: vec![
                game("one", &["ARCADE"], "plugin", ""),
                game("two", &["Strategy"], "other", ""),
                game("three", &["K"], "other", ""),
            ],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let settings = ProductSettings {
            recent_games: 0,
            sync_all_installed: true,
            exclude_categories: vec!["arcade".into(), "k".into()],
            exclude_games: vec!["TWO".into()],
            ..ProductSettings::default()
        };
        let selected = select_games(&snapshot, &settings, 0);

        assert_eq!(
            selected
                .iter()
                .map(|game| game.id.as_str())
                .collect::<Vec<_>>(),
            vec!["three"]
        );
    }

    #[test]
    fn snapshot_ids_do_not_unicode_case_fold() {
        let mut assembler = SnapshotAssembler::default();
        assembler.receive_line(br#"{"type":"snapshotStart"}"#);
        assembler.receive_line(br#"{"type":"plugins","payload":[]}"#);
        assembler.receive_line(br#"{"type":"categories","payload":[]}"#);
        assembler.receive_line(
            br#"{"type":"games","payload":[{"id":"k","name":"ASCII","installed":true},{"id":"\u212a","name":"Kelvin","installed":true}]}"#,
        );

        assert!(matches!(
            assembler.receive_line(br#"{"type":"snapshotComplete","games":2}"#),
            Some(Ok(snapshot)) if snapshot.games.len() == 2
        ));
    }

    #[test]
    fn included_categories_use_ascii_case_insensitive_matching() {
        let snapshot = Snapshot {
            games: vec![
                game("ascii-match", &["ARCADE"], "plugin", ""),
                game("unicode-distinct", &["K"], "other", ""),
            ],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let settings = ProductSettings {
            recent_games: 0,
            sync_categories: vec!["arcade".into(), "k".into()],
            ..ProductSettings::default()
        };

        let selected = select_games(&snapshot, &settings, 0);

        assert_eq!(
            selected
                .iter()
                .map(|game| game.id.as_str())
                .collect::<Vec<_>>(),
            vec!["ascii-match"]
        );
    }

    #[test]
    fn included_plugins_use_ascii_case_insensitive_matching() {
        let snapshot = Snapshot {
            games: vec![game("ascii-match", &[], "P3", "")],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let settings = ProductSettings {
            recent_games: 0,
            sync_plugins: vec!["p3".into()],
            ..ProductSettings::default()
        };

        let selected = select_games(&snapshot, &settings, 0);

        assert_eq!(
            selected
                .iter()
                .map(|game| game.id.as_str())
                .collect::<Vec<_>>(),
            vec!["ascii-match"]
        );
    }

    #[test]
    fn plugin_inclusion_does_not_unicode_case_fold() {
        let kelvin_plugin = Snapshot {
            games: vec![game("kelvin", &[], "K", "")],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let ascii_setting = ProductSettings {
            recent_games: 0,
            sync_plugins: vec!["k".into()],
            ..ProductSettings::default()
        };
        assert!(select_games(&kelvin_plugin, &ascii_setting, 0).is_empty());

        let ascii_plugin = Snapshot {
            games: vec![game("ascii", &[], "k", "")],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let unicode_setting = ProductSettings {
            recent_games: 0,
            sync_plugins: vec!["K".into()],
            ..ProductSettings::default()
        };
        assert!(select_games(&ascii_plugin, &unicode_setting, 0).is_empty());
    }

    #[test]
    fn excluded_plugins_use_ascii_case_insensitive_matching() {
        let snapshot = Snapshot {
            games: vec![game("ascii-match", &[], "STEAM", "")],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let settings = ProductSettings {
            recent_games: 0,
            sync_all_installed: true,
            exclude_plugins: vec!["steam".into()],
            ..ProductSettings::default()
        };

        assert!(select_games(&snapshot, &settings, 0).is_empty());
    }

    #[test]
    fn plugin_exclusion_does_not_unicode_case_fold() {
        let kelvin_plugin = Snapshot {
            games: vec![game("kelvin", &[], "K", "")],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let ascii_setting = ProductSettings {
            recent_games: 0,
            sync_all_installed: true,
            exclude_plugins: vec!["k".into()],
            ..ProductSettings::default()
        };
        assert_eq!(
            select_games(&kelvin_plugin, &ascii_setting, 0)
                .iter()
                .map(|game| game.id.as_str())
                .collect::<Vec<_>>(),
            vec!["kelvin"]
        );

        let ascii_plugin = Snapshot {
            games: vec![game("ascii", &[], "k", "")],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let unicode_setting = ProductSettings {
            recent_games: 0,
            sync_all_installed: true,
            exclude_plugins: vec!["K".into()],
            ..ProductSettings::default()
        };
        assert_eq!(
            select_games(&ascii_plugin, &unicode_setting, 0)
                .iter()
                .map(|game| game.id.as_str())
                .collect::<Vec<_>>(),
            vec!["ascii"]
        );
    }

    #[test]
    fn game_id_exclusions_do_not_unicode_case_fold() {
        let kelvin_id = Snapshot {
            games: vec![game("K", &[], "other", "")],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let ascii_setting = ProductSettings {
            recent_games: 0,
            sync_all_installed: true,
            exclude_games: vec!["k".into()],
            ..ProductSettings::default()
        };
        assert_eq!(
            select_games(&kelvin_id, &ascii_setting, 0)
                .iter()
                .map(|game| game.id.as_str())
                .collect::<Vec<_>>(),
            vec!["K"]
        );

        let ascii_id = Snapshot {
            games: vec![game("k", &[], "other", "")],
            categories: Vec::new(),
            plugins: Vec::new(),
        };
        let unicode_setting = ProductSettings {
            recent_games: 0,
            sync_all_installed: true,
            exclude_games: vec!["K".into()],
            ..ProductSettings::default()
        };
        assert_eq!(
            select_games(&ascii_id, &unicode_setting, 0)
                .iter()
                .map(|game| game.id.as_str())
                .collect::<Vec<_>>(),
            vec!["k"]
        );
    }

    #[test]
    fn playnite_launch_is_guid_typed_and_ignores_catalog_executable_metadata() {
        let valid_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        let launch = playnite_launch(&Game {
            id: valid_id.into(),
            exe: "playnite://untrusted-executable-metadata".into(),
            args: "ignored by typed Playnite launch".into(),
            ..game("unused", &[], "", "")
        })
        .unwrap();
        assert_eq!(launch["kind"], "playnite");
        assert_eq!(launch["game_id"], valid_id);
        assert_eq!(launch["exit_timeout_seconds"], 10);
        assert!(
            playnite_launch(&Game {
                id: "../../fake".into(),
                ..game("bad", &[], "", "")
            })
            .is_none()
        );
    }

    #[test]
    fn iso8601_parser_validates_offsets_and_calendar_dates() {
        assert_eq!(
            parse_iso8601_unix("2026-09-24T02:30:00+02:30"),
            parse_iso8601_unix("2026-09-24T00:00:00Z")
        );
        assert!(parse_iso8601_unix("2024-02-29T00:00:00Z").is_some());
        assert_eq!(parse_iso8601_unix("2023-02-29T00:00:00Z"), None);
        assert_eq!(parse_iso8601_unix("2026-20-24T00:00:00Z"), None);
    }
}
