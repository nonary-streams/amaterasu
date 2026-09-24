use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

const MAX_PREPARATION_STEPS: usize = 8;
const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 10_000;
const MAX_COMMAND_TIMEOUT_MS: u64 = 30_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparationStep {
    pub(crate) run: ConfiguredCommand,
    #[serde(default)]
    pub(crate) undo: Option<ConfiguredCommand>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfiguredCommand {
    pub(crate) executable: String,
    #[serde(default)]
    pub(crate) args: Vec<String>,
    #[serde(default)]
    pub(crate) environment: BTreeMap<String, String>,
    pub(crate) cwd: Option<String>,
    #[serde(default = "default_command_timeout_ms")]
    pub(crate) timeout_ms: u64,
}

fn default_command_timeout_ms() -> u64 {
    DEFAULT_COMMAND_TIMEOUT_MS
}

pub(crate) fn validate_preparation(steps: &[PreparationStep]) -> bool {
    steps.len() <= MAX_PREPARATION_STEPS
        && steps
            .iter()
            .all(|step| valid_command(&step.run) && step.undo.as_ref().is_none_or(valid_command))
}

fn valid_command(command: &ConfiguredCommand) -> bool {
    if !valid_native_path(&command.executable)
        || command
            .cwd
            .as_deref()
            .is_some_and(|cwd| !valid_native_path(cwd))
        || command.args.len() > 64
        || command.args.iter().map(String::len).sum::<usize>() > 8_192
        || command
            .args
            .iter()
            .any(|argument| argument.len() > 1_024 || argument.contains('\0'))
        || !(1..=MAX_COMMAND_TIMEOUT_MS).contains(&command.timeout_ms)
        || command.environment.len() > 32
        || command
            .environment
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            > 4_096
        || command.environment.iter().any(|(key, value)| {
            key.is_empty()
                || key.len() > 128
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || value.len() > 1_024
                || value.contains('\0')
        })
    {
        return false;
    }

    if Path::new(&command.executable)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
        })
    {
        return false;
    }

    let mut keys = command
        .environment
        .keys()
        .map(|key| key.to_ascii_uppercase())
        .collect::<Vec<_>>();
    keys.sort_unstable();
    !keys.windows(2).any(|pair| pair[0] == pair[1])
}

fn valid_native_path(value: &str) -> bool {
    Path::new(value).is_absolute() && value.len() <= 1_024 && !value.contains('\0')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_the_launch_preparation_shape_and_bounds() {
        let step: PreparationStep = serde_json::from_value(json!({
            "run":{"executable":std::env::current_exe().unwrap()},
            "undo":null
        }))
        .unwrap();
        assert!(validate_preparation(std::slice::from_ref(&step)));
        assert_eq!(step.run.timeout_ms, DEFAULT_COMMAND_TIMEOUT_MS);
        assert!(
            serde_json::from_value::<PreparationStep>(json!({
                "run":{"executable":"relative-command"}
            }))
            .is_ok()
        );
        let invalid: PreparationStep = serde_json::from_value(json!({
            "run":{"executable":"relative-command"}
        }))
        .unwrap();
        assert!(!validate_preparation(&[invalid]));
        assert!(!validate_preparation(&vec![
            step;
            MAX_PREPARATION_STEPS + 1
        ]));
    }
}
