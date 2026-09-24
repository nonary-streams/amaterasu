use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const MAX_ENTRIES: usize = 100_000;
const MAX_PREPARATION_STEPS: usize = 8;
const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 10_000;
const MAX_COMMAND_TIMEOUT_MS: u64 = 30_000;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub launch: Launch,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Launch {
    Executable {
        program: String,
        #[serde(default)]
        arguments: Vec<String>,
        // Accept the sibling's per-application spelling but store one canonical field.
        #[serde(
            default,
            alias = "working-dir",
            deserialize_with = "empty_working_directory_as_none"
        )]
        working_directory: Option<String>,
        #[serde(default = "default_auto_detach", alias = "auto-detach")]
        auto_detach: bool,
        #[serde(default = "default_wait_all", alias = "wait-all")]
        wait_all: bool,
        #[serde(default = "default_exit_timeout_seconds", alias = "exit-timeout")]
        exit_timeout_seconds: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        detached: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        preparation: Vec<PreparationStep>,
    },
    Application {
        path: String,
    },
}

fn empty_working_directory_as_none<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.filter(|directory| !directory.is_empty()))
}

fn default_auto_detach() -> bool {
    true
}

fn default_wait_all() -> bool {
    true
}

fn default_exit_timeout_seconds() -> u64 {
    10
}

const MAX_EXIT_TIMEOUT_SECONDS: u64 = 300;
const MAX_DETACHED_COMMANDS: usize = 16;
const MAX_DETACHED_COMMAND_BYTES: usize = 2048;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationStep {
    pub run: ConfiguredCommand,
    pub undo: Option<ConfiguredCommand>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredCommand {
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    pub cwd: Option<String>,
    #[serde(default = "default_command_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_command_timeout_ms() -> u64 {
    DEFAULT_COMMAND_TIMEOUT_MS
}

impl Entry {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !bounded_text(&self.name, 160) || self.name.trim().is_empty() {
            return Err("name must contain 1..160 bytes of printable text");
        }
        if !bounded_text(&self.description, 1024) {
            return Err("description exceeds 1024 bytes or contains control characters");
        }
        if self.tags.len() > 16
            || self
                .tags
                .iter()
                .any(|tag| tag.trim().is_empty() || !bounded_text(tag, 64))
        {
            return Err("at most 16 nonempty printable tags of 64 bytes are allowed");
        }
        match &self.launch {
            Launch::Executable {
                program,
                arguments,
                working_directory,
                auto_detach: _,
                wait_all: _,
                exit_timeout_seconds,
                detached,
                preparation,
            } => {
                validate_path(program)?;
                if arguments.len() > 32 || arguments.iter().any(|arg| !bounded_text(arg, 1024)) {
                    return Err("at most 32 printable arguments of 1024 bytes are allowed");
                }
                if let Some(directory) = working_directory {
                    validate_path(directory)?;
                }
                if *exit_timeout_seconds > MAX_EXIT_TIMEOUT_SECONDS
                    || detached.len() > MAX_DETACHED_COMMANDS
                    || detached.iter().any(|command| {
                        command.is_empty()
                            || command.len() > MAX_DETACHED_COMMAND_BYTES
                            || command.chars().any(char::is_control)
                    })
                {
                    return Err("invalid per-application process policy");
                }
                validate_preparation(preparation)?;
            }
            Launch::Application { path } => validate_path(path)?,
        }
        if serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > 11_950) {
            return Err("encoded entry exceeds 11950 bytes");
        }
        Ok(())
    }
}

fn validate_preparation(steps: &[PreparationStep]) -> Result<(), &'static str> {
    if steps.len() > MAX_PREPARATION_STEPS {
        return Err("at most 8 preparation steps are allowed");
    }
    for step in steps {
        validate_command(&step.run)?;
        if let Some(undo) = &step.undo {
            validate_command(undo)?;
        }
    }
    Ok(())
}

fn validate_command(command: &ConfiguredCommand) -> Result<(), &'static str> {
    if !valid_command_path(&command.executable)
        || command
            .cwd
            .as_deref()
            .is_some_and(|directory| !valid_command_path(directory))
    {
        return Err("command executable and cwd must be native absolute paths");
    }
    if Path::new(&command.executable)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
        })
    {
        return Err("batch command targets require an explicit interpreter");
    }
    if command.args.len() > 64
        || command
            .args
            .iter()
            .any(|argument| argument.len() > 1_024 || argument.contains('\0'))
        || command.args.iter().map(String::len).sum::<usize>() > 8_192
    {
        return Err("command arguments exceed bounds");
    }
    if !(1..=MAX_COMMAND_TIMEOUT_MS).contains(&command.timeout_ms) {
        return Err("command timeout exceeds bounds");
    }
    validate_command_environment(&command.environment)
}

fn valid_command_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1_024
        && !value.contains('\0')
        && Path::new(value).is_absolute()
}

fn validate_command_environment(
    environment: &BTreeMap<String, String>,
) -> Result<(), &'static str> {
    if environment.len() > 32
        || environment.iter().any(|(key, value)| {
            key.is_empty()
                || key.len() > 128
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || value.len() > 1_024
                || value.contains('\0')
        })
        || environment
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            > 4_096
    {
        return Err("command environment exceeds bounds");
    }
    let mut names = environment
        .keys()
        .map(|key| key.to_ascii_uppercase())
        .collect::<Vec<_>>();
    names.sort_unstable();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("command environment names are ambiguous");
    }
    Ok(())
}

pub fn bounded_text(value: &str, limit: usize) -> bool {
    value.len() <= limit && !value.chars().any(char::is_control)
}

fn validate_path(path: &str) -> Result<(), &'static str> {
    // Accept explicit POSIX, drive-rooted Windows, and UNC metadata on any host.
    // This is structural validation, not filesystem access or execution authorization.
    let bytes = path.as_bytes();
    let drive_rooted = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    if !bounded_text(path, 2048)
        || !(path.starts_with('/') || path.starts_with("\\\\") || drive_rooted)
        || path
            .split(['/', '\\'])
            .any(|part| part == ".." || part == ".")
    {
        return Err(
            "launch paths must be absolute, printable, at most 2048 bytes, without dot segments",
        );
    }
    Ok(())
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub revision: u64,
    pub next_id: u64,
    pub entries: BTreeMap<String, Entry>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            revision: 0,
            next_id: 1,
            entries: BTreeMap::new(),
        }
    }
}

impl Document {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.next_id == 0 || self.entries.len() > MAX_ENTRIES {
            return Err("invalid catalog size or ID counter");
        }
        for (id, entry) in &self.entries {
            let number = parse_id(id).ok_or("invalid entry ID")?;
            if number >= self.next_id {
                return Err("entry ID exceeds allocated range");
            }
            entry.validate()?;
        }
        Ok(())
    }
}

pub fn parse_id(id: &str) -> Option<u64> {
    let number = u64::from_str_radix(id.strip_prefix("app-")?, 16).ok()?;
    (number != 0 && id == format!("app-{number:016x}")).then_some(number)
}
