use crate::lifecycle::validate_environment;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path as CommandPath;
#[cfg(all(windows, test))]
use std::path::Path;

pub(crate) const MAX_DETACHED_COMMANDS: usize = 16;
const MAX_DETACHED_COMMAND_BYTES: usize = 2048;
const MAX_DETACHED_ARGUMENTS: usize = 64;
const MAX_DETACHED_ARGUMENT_BYTES: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DetachedCommand {
    pub executable: String,
    pub args: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub elevated: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum DetachedCommandInput {
    Legacy(String),
    Typed(DetachedCommandRequest),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DetachedCommandRequest {
    pub command: String,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default, alias = "working_directory")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub stdout: Option<String>,
    #[serde(default)]
    pub stderr: Option<String>,
    #[serde(default)]
    pub elevated: Option<bool>,
}

impl From<DetachedCommandInput> for DetachedCommandRequest {
    fn from(value: DetachedCommandInput) -> Self {
        match value {
            DetachedCommandInput::Legacy(command) => Self {
                command,
                environment: BTreeMap::new(),
                cwd: None,
                stdout: None,
                stderr: None,
                elevated: None,
            },
            DetachedCommandInput::Typed(request) => request,
        }
    }
}

pub(crate) fn prepare_detached_requests(
    requests: &[DetachedCommandRequest],
    launch_environment: &BTreeMap<String, String>,
    host_environment_lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<DetachedCommand>, &'static str> {
    validate_detached_command_texts(
        &requests
            .iter()
            .map(|request| request.command.clone())
            .collect::<Vec<_>>(),
    )?;
    requests
        .iter()
        .map(|request| {
            validate_environment(&request.environment)?;
            if request.cwd.as_deref().is_some_and(|cwd| {
                cwd.is_empty()
                    || cwd.len() > 1024
                    || cwd.contains('\0')
                    || !CommandPath::new(cwd).is_absolute()
            }) {
                return Err("detached cwd must be an absolute path");
            }
            if [request.stdout.as_deref(), request.stderr.as_deref()]
                .into_iter()
                .flatten()
                .any(|path| {
                    path.is_empty()
                        || path.len() > 1024
                        || path.chars().any(char::is_control)
                        || !CommandPath::new(path).is_absolute()
                })
            {
                return Err("detached output must be an absolute path");
            }
            let expanded =
                expand_environment_references(&request.command, &request.environment, &|name| {
                    environment_reference_value(name, launch_environment, host_environment_lookup)
                })?;
            let mut command = parse_detached_commands(vec![expanded])?.remove(0);
            command.environment = launch_environment.clone();
            for (key, value) in &request.environment {
                #[cfg(windows)]
                command
                    .environment
                    .retain(|existing, _| !existing.eq_ignore_ascii_case(key));
                command.environment.insert(key.clone(), value.clone());
            }
            validate_environment(&command.environment)?;
            command.cwd = request.cwd.clone();
            command.stdout = request.stdout.clone();
            command.stderr = request.stderr.clone();
            command.elevated = request.elevated;
            Ok(command)
        })
        .collect()
}

pub(crate) fn parse_detached_commands(
    commands: Vec<String>,
) -> Result<Vec<DetachedCommand>, &'static str> {
    validate_detached_command_texts(&commands)?;

    commands
        .into_iter()
        .map(|command| {
            let mut arguments = split_command_line(&command).ok_or("invalid detached command")?;
            if arguments.is_empty()
                || arguments[0].is_empty()
                || arguments.iter().any(|argument| argument.contains('\0'))
                || arguments.len() > MAX_DETACHED_ARGUMENTS
                || arguments.iter().map(String::len).sum::<usize>() > MAX_DETACHED_ARGUMENT_BYTES
            {
                return Err("invalid detached command arguments");
            }

            let executable = arguments.remove(0);
            Ok(DetachedCommand {
                executable,
                args: arguments,
                environment: BTreeMap::new(),
                cwd: None,
                stdout: None,
                stderr: None,
                elevated: None,
            })
        })
        .collect()
}

pub(crate) fn validate_detached_command_texts(commands: &[String]) -> Result<(), &'static str> {
    if commands.len() > MAX_DETACHED_COMMANDS {
        return Err("too many detached commands");
    }

    if commands.iter().any(|command| {
        command.is_empty()
            || command.len() > MAX_DETACHED_COMMAND_BYTES
            || command.chars().any(char::is_control)
    }) {
        return Err("detached command exceeds bounds");
    }

    Ok(())
}

#[cfg(test)]
pub(crate) fn prepare_detached_commands(
    command_texts: &[String],
    environment: &BTreeMap<String, String>,
    host_environment_lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<DetachedCommand>, &'static str> {
    validate_detached_command_texts(command_texts)?;

    let expanded_commands = command_texts
        .iter()
        .map(|command| expand_environment_references(command, environment, host_environment_lookup))
        .collect::<Result<Vec<_>, _>>()?;

    parse_detached_commands(expanded_commands)
}

fn expand_environment_references(
    command: &str,
    environment: &BTreeMap<String, String>,
    host_environment_lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<String, &'static str> {
    let mut expanded = String::with_capacity(command.len().min(MAX_DETACHED_COMMAND_BYTES));
    let mut cursor = 0;

    while let Some(relative_dollar) = command[cursor..].find('$') {
        let dollar = cursor + relative_dollar;
        append_bounded(&mut expanded, &command[cursor..dollar])?;
        let after_dollar = dollar + 1;

        match command.as_bytes().get(after_dollar) {
            Some(b'$') => {
                append_bounded(&mut expanded, "$")?;
                cursor = after_dollar + 1;
            }
            Some(b'(') => {
                let mut depth = 0usize;
                let mut closing_parenthesis = None;
                for (offset, character) in command[after_dollar..].char_indices() {
                    match character {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                closing_parenthesis = Some(after_dollar + offset);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let closing_parenthesis = closing_parenthesis
                    .ok_or("detached command environment reference is not closed")?;
                let name = &command[after_dollar + 1..closing_parenthesis];
                let value = environment_reference_value(name, environment, host_environment_lookup);
                append_bounded(&mut expanded, value.as_deref().unwrap_or_default())?;
                cursor = closing_parenthesis + 1;
            }
            _ => {
                append_bounded(&mut expanded, "$")?;
                cursor = after_dollar;
            }
        }
    }

    append_bounded(&mut expanded, &command[cursor..])?;
    Ok(expanded)
}

fn append_bounded(output: &mut String, value: &str) -> Result<(), &'static str> {
    if output.len().saturating_add(value.len()) > MAX_DETACHED_COMMAND_BYTES {
        return Err("expanded detached command exceeds bounds");
    }
    output.push_str(value);
    Ok(())
}

fn environment_reference_value(
    name: &str,
    environment: &BTreeMap<String, String>,
    host_environment_lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    #[cfg(windows)]
    let configured = environment
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone());
    #[cfg(not(windows))]
    let configured = environment.get(name).cloned();

    configured.or_else(|| host_environment_lookup(name))
}

#[cfg(not(windows))]
fn split_command_line(command: &str) -> Option<Vec<String>> {
    shlex::split(command)
}

#[cfg(windows)]
fn split_command_line(command: &str) -> Option<Vec<String>> {
    let mut arguments = Vec::new();
    let mut argument = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut characters = command.chars().peekable();

    while let Some(character) = characters.next() {
        if character == '\\' {
            let mut slash_count = 1usize;
            while characters.peek() == Some(&'\\') {
                characters.next();
                slash_count += 1;
            }
            if characters.peek() == Some(&'"') {
                argument.extend(std::iter::repeat_n('\\', slash_count / 2));
                if slash_count.is_multiple_of(2) {
                    quoted = !quoted;
                } else {
                    argument.push('"');
                }
                characters.next();
                started = true;
            } else {
                argument.extend(std::iter::repeat_n('\\', slash_count));
                started = true;
            }
            continue;
        }

        match character {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            character if character.is_whitespace() && !quoted => {
                if started {
                    arguments.push(std::mem::take(&mut argument));
                    started = false;
                }
            }
            character => {
                argument.push(character);
                started = true;
            }
        }
    }

    if quoted {
        return None;
    }
    if started {
        arguments.push(argument);
    }
    Some(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ordered_command_strings_without_shell_expansion() {
        let commands = parse_detached_commands(vec![
            "display-tool --layout \"Desk Mode\"".into(),
            "worker --literal=$HOME".into(),
        ])
        .unwrap();
        assert_eq!(commands[0].executable, "display-tool");
        assert_eq!(commands[0].args, ["--layout", "Desk Mode"]);
        assert_eq!(commands[1].args, ["--literal=$HOME"]);
    }

    #[test]
    fn expands_configured_and_host_environment_without_shell_interpretation() {
        let environment = BTreeMap::from([("TITLE".to_owned(), "App Mode".to_owned())]);
        let command_texts = vec![
            "tool --title=\"$(TITLE)\" --host=\"$(HOST_ONLY)\" missing=$(MISSING) escaped=$$(TITLE) literal=$HOME"
                .to_owned(),
        ];

        let commands =
            prepare_detached_commands(&command_texts, &environment, &|name| match name {
                "TITLE" => Some("host title".to_owned()),
                "HOST_ONLY" => Some("host value".to_owned()),
                _ => None,
            })
            .unwrap();

        assert_eq!(commands[0].executable, "tool");
        assert_eq!(
            commands[0].args,
            [
                "--title=App Mode",
                "--host=host value",
                "missing=",
                "escaped=$(TITLE)",
                "literal=$HOME",
            ]
        );
    }

    #[test]
    fn typed_command_expansion_prefers_override_then_launch_then_host() {
        let launch = BTreeMap::from([
            ("OVERRIDE".into(), "launch value".into()),
            ("LAUNCH".into(), "launch only".into()),
        ]);
        let requests = [DetachedCommandRequest {
            command: "tool \"$(OVERRIDE)\" \"$(LAUNCH)\" \"$(HOST)\"".into(),
            environment: BTreeMap::from([("OVERRIDE".into(), "command value".into())]),
            cwd: None,
            stdout: None,
            stderr: None,
            elevated: None,
        }];
        let commands = prepare_detached_requests(&requests, &launch, &|name| {
            (name == "HOST").then(|| "host only".into())
        })
        .unwrap();
        assert_eq!(
            commands[0].args,
            ["command value", "launch only", "host only"]
        );
        assert_eq!(commands[0].environment["OVERRIDE"], "command value");
        assert_eq!(commands[0].environment["LAUNCH"], "launch only");
    }

    #[test]
    fn rejects_unclosed_and_oversized_environment_expansions() {
        let environment = BTreeMap::from([("VALUE".to_owned(), "x".repeat(2_048))]);

        assert!(
            prepare_detached_commands(&["tool $(VALUE".to_owned()], &environment, &|_| None,)
                .is_err()
        );
        assert!(
            prepare_detached_commands(&["tool $(VALUE)".to_owned()], &environment, &|_| None,)
                .is_err()
        );
    }

    #[test]
    fn rejects_malformed_or_unbounded_command_lists() {
        assert!(parse_detached_commands(vec!["".into()]).is_err());
        assert!(parse_detached_commands(vec!["tool \"unclosed".into()]).is_err());
        assert!(parse_detached_commands(vec!["tool".repeat(MAX_DETACHED_COMMAND_BYTES)]).is_err());
        assert!(parse_detached_commands(vec!["tool".into(); MAX_DETACHED_COMMANDS + 1]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_command_parser_preserves_backslashes_and_quotes() {
        let executable = Path::new(r"C:\Program Files\Example\game.exe");
        let line = format!(r#""{}" --name "Desk Mode""#, executable.display());
        let parsed = split_command_line(&line).unwrap();
        assert_eq!(parsed[0], executable.to_string_lossy());
        assert_eq!(parsed[1..], ["--name", "Desk Mode"]);
    }
}
