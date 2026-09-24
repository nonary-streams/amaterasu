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
}

pub(crate) fn parse_detached_commands(
    commands: Vec<String>,
) -> Result<Vec<DetachedCommand>, &'static str> {
    if commands.len() > MAX_DETACHED_COMMANDS {
        return Err("too many detached commands");
    }

    commands
        .into_iter()
        .map(|command| {
            if command.is_empty()
                || command.len() > MAX_DETACHED_COMMAND_BYTES
                || command.chars().any(char::is_control)
            {
                return Err("detached command exceeds bounds");
            }

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
            })
        })
        .collect()
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
