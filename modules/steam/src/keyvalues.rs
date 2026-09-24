//! Strict bounded parser for the text KeyValues subset used by Steam manifests.
use std::collections::BTreeMap;

#[derive(Debug)]
pub enum Value {
    Text(String),
    Object(BTreeMap<String, Value>),
}

impl Value {
    pub fn field(&self, name: &str) -> Result<&Value, String> {
        match self {
            Self::Object(fields) => fields
                .get(&name.to_ascii_lowercase())
                .ok_or_else(|| format!("missing field {name}")),
            _ => Err("expected object".into()),
        }
    }

    pub fn text(&self) -> Result<&str, String> {
        match self {
            Self::Text(text) => Ok(text),
            _ => Err("expected text".into()),
        }
    }
}

pub fn parse(source: &str) -> Result<Value, String> {
    parse_bounded(source, 1_048_576)
}

pub fn parse_bounded(source: &str, max_bytes: usize) -> Result<Value, String> {
    if source.len() > max_bytes {
        return Err("KeyValues document exceeds its byte limit".into());
    }
    let mut parser = Parser {
        source: source.trim_start_matches('\u{feff}').as_bytes(),
        position: 0,
        nodes: 0,
    };
    parser.object(0, false)
}

struct Parser<'a> {
    source: &'a [u8],
    position: usize,
    nodes: usize,
}

impl Parser<'_> {
    fn whitespace(&mut self) {
        loop {
            while self
                .source
                .get(self.position)
                .is_some_and(u8::is_ascii_whitespace)
            {
                self.position += 1;
            }
            if self.source.get(self.position..self.position + 2) != Some(b"//") {
                break;
            }
            while self
                .source
                .get(self.position)
                .is_some_and(|byte| *byte != b'\n')
            {
                self.position += 1;
            }
        }
    }

    fn object(&mut self, depth: usize, nested: bool) -> Result<Value, String> {
        if depth > 16 {
            return Err("nesting exceeds 16".into());
        }
        let mut fields = BTreeMap::new();
        loop {
            self.whitespace();
            match self.source.get(self.position) {
                None if !nested => break,
                Some(b'}') if nested => {
                    self.position += 1;
                    break;
                }
                None | Some(b'}') => return Err("unbalanced braces".into()),
                _ => {}
            }
            self.nodes += 1;
            if self.nodes > 32_768 {
                return Err("too many manifest fields".into());
            }
            let key = self.token()?.to_ascii_lowercase();
            if key.starts_with('#') {
                return Err("KeyValues directives are unsupported".into());
            }
            self.whitespace();
            let value = if self.source.get(self.position) == Some(&b'{') {
                self.position += 1;
                self.object(depth + 1, true)?
            } else {
                Value::Text(self.token()?)
            };
            if fields.insert(key, value).is_some() {
                return Err("duplicate manifest key".into());
            }
        }
        Ok(Value::Object(fields))
    }

    fn token(&mut self) -> Result<String, String> {
        self.whitespace();
        let mut bytes = Vec::new();
        if self.source.get(self.position) == Some(&b'"') {
            self.position += 1;
            loop {
                let byte = *self.source.get(self.position).ok_or("unterminated quote")?;
                self.position += 1;
                match byte {
                    b'"' => break,
                    b'\\' => {
                        let escaped = *self
                            .source
                            .get(self.position)
                            .ok_or("unterminated escape")?;
                        self.position += 1;
                        match escaped {
                            b'"' | b'\\' => bytes.push(escaped),
                            b'n' => bytes.push(b'\n'),
                            b't' => bytes.push(b'\t'),
                            b'r' => bytes.push(b'\r'),
                            _ => {
                                bytes.push(b'\\');
                                bytes.push(escaped);
                            }
                        }
                    }
                    0 => return Err("NUL in manifest".into()),
                    _ => bytes.push(byte),
                }
                if bytes.len() > 8192 {
                    return Err("token exceeds 8192 bytes".into());
                }
            }
        } else {
            while let Some(&byte) = self.source.get(self.position) {
                if byte.is_ascii_whitespace() || matches!(byte, b'{' | b'}') {
                    break;
                }
                if matches!(byte, 0 | b'"') {
                    return Err("invalid unquoted token".into());
                }
                bytes.push(byte);
                self.position += 1;
                if bytes.len() > 8192 {
                    return Err("token exceeds 8192 bytes".into());
                }
            }
            if bytes.is_empty() {
                return Err("expected token".into());
            }
        }
        String::from_utf8(bytes).map_err(|_| "invalid UTF-8".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_comments_unicode_and_escaped_paths() {
        let document =
            parse("// comment\n Root { \"path\" \"C:\\\\Games\" name \"遊戲 \\\"A\\\"\" }")
                .unwrap();
        let root = document.field("ROOT").unwrap();
        assert_eq!(root.field("path").unwrap().text().unwrap(), "C:\\Games");
        assert_eq!(root.field("name").unwrap().text().unwrap(), "遊戲 \"A\"");
    }

    #[test]
    fn rejects_partial_ambiguous_and_excessive_documents() {
        for malformed in [
            "a {",
            "a b }",
            "a \"unfinished",
            "a b A c",
            "#include file",
            "a b trailing",
        ] {
            assert!(parse(malformed).is_err(), "{malformed}");
        }
        let nested = format!("{}{}", "a { ".repeat(18), "}".repeat(18));
        assert!(parse(&nested).is_err());
    }
}
