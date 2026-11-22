//! Platform rules for validating native application and preparation commands.

pub use amaterasu_extension_api::environment_name_key;

/// On Windows, spawning a batch file can implicitly invoke the command interpreter.
pub fn requires_command_interpreter(path: &std::path::Path) -> bool {
    cfg!(windows)
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("bat") || ext.eq_ignore_ascii_case("cmd"))
}

/// Expand bounded launch path references with the caller's environment lookup.
pub fn expand_launch_path(
    value: &str,
    mut lookup: impl FnMut(&str) -> Option<String>,
) -> Result<String, &'static str> {
    let mut expanded = String::new();
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '$' {
            let mut buffer = [0; 4];
            append_bounded(&mut expanded, character.encode_utf8(&mut buffer))?;
            continue;
        }

        match characters.peek() {
            Some('(') => {
                characters.next();
                let mut name = String::new();
                let mut closed = false;
                for character in characters.by_ref() {
                    match character {
                        ')' => {
                            closed = true;
                            break;
                        }
                        '(' => return Err("unsupported_launch_spec"),
                        _ => {
                            name.push(character);
                            if name.len() > 128 {
                                return Err("unsupported_launch_spec");
                            }
                        }
                    }
                }
                if !closed
                    || name.is_empty()
                    || name.len() > 128
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    return Err("unsupported_launch_spec");
                }
                let value = lookup(&name).unwrap_or_default();
                append_bounded(&mut expanded, &value)?;
            }
            Some('$') => {
                characters.next();
                append_bounded(&mut expanded, "$")?;
            }
            _ => append_bounded(&mut expanded, "$")?,
        }
    }
    Ok(expanded)
}

fn append_bounded(output: &mut String, value: &str) -> Result<(), &'static str> {
    if output.len().saturating_add(value.len()) > 1_024 {
        return Err("unsupported_launch_spec");
    }
    output.push_str(value);
    Ok(())
}

pub fn is_bare_command(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains(':')
}

pub fn trim_path_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'"' | b'\'') && bytes.last() == Some(&bytes[0]) {
        &value[1..value.len() - 1]
    } else {
        value
    }
}
