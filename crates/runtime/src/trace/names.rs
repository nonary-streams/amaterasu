//! Interned names for tracks, stages, spans and labels. Cold path only.
use amaterasu_abi::MAX_TRACE_NAME_BYTES;
use std::collections::BTreeMap;

pub(crate) const MAX_NAMES: usize = 1024;

#[derive(Default)]
pub(crate) struct Names {
    identifiers: BTreeMap<String, u16>,
    names: Vec<String>,
}

impl Names {
    /// Identifiers start at one; zero is reserved for "no name".
    pub fn intern(&mut self, name: &str) -> Option<u16> {
        if !valid_trace_name(name) {
            return None;
        }
        if let Some(identifier) = self.identifiers.get(name) {
            return Some(*identifier);
        }
        if self.names.len() >= MAX_NAMES {
            return None;
        }
        self.names.push(name.to_owned());
        let identifier = self.names.len() as u16;
        self.identifiers.insert(name.to_owned(), identifier);
        Some(identifier)
    }

    pub fn name(&self, identifier: u16) -> &str {
        identifier
            .checked_sub(1)
            .and_then(|index| self.names.get(usize::from(index)))
            .map_or("unknown", String::as_str)
    }
}

/// Names appear verbatim in reports, so they are restricted to characters that
/// need no escaping and cannot carry user data such as paths with spaces.
pub(crate) fn valid_trace_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_TRACE_NAME_BYTES
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/' | b'@' | b':')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_name_keeps_its_identifier_and_invalid_names_are_refused() {
        let mut names = Names::default();
        let first = names.intern("input").unwrap();
        let second = names.intern("host.tick").unwrap();
        assert_eq!((first, second), (1, 2));
        assert_eq!(names.intern("input"), Some(first));
        assert_eq!(names.name(second), "host.tick");
        assert_eq!(names.name(0), "unknown");
        assert_eq!(names.name(900), "unknown");
        for invalid in ["", "has space", "quote\"", "line\nbreak", &"x".repeat(97)] {
            assert_eq!(names.intern(invalid), None, "{invalid:?}");
        }
    }

    #[test]
    fn the_table_is_bounded() {
        let mut names = Names::default();
        for index in 0..MAX_NAMES {
            assert!(names.intern(&format!("name-{index}")).is_some());
        }
        assert_eq!(names.intern("one-more"), None);
        assert_eq!(names.intern("name-7"), Some(8));
    }
}
