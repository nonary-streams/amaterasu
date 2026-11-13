//! The sets known to have run on this host with version switching.
//!
//! A set's management module either serves the `versions` and `restore`
//! routes or it does not, and nothing a publisher signs says which. So a set
//! that serves them records its own digest here once the host shell has shown
//! that it takes restore requests. The console reads the record to warn before
//! a switch to a set whose own console could not switch again.
//!
//! The record is a fact about what has run, kept for a person to read. The
//! host never reads it and it authorizes nothing: a set the record does not
//! name is exactly as restorable as one it does.
//!
//! It lives beside the approved repositories, in the directory this module
//! already keeps its package state in, and is read and written only on a
//! worker thread.
use crate::package_status;
use amaterasu_package::{create_private_directory, read_bounded, write_json_atomically};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

/// The record's file in the repository directory.
pub(super) const FILE: &str = "switching-sets.json";
/// How many sets the record names. The ones that ran most recently are kept.
pub(super) const CAPACITY: usize = 256;
/// Several times a full record, so a file beyond it is not one.
const MAX_BYTES: u64 = 64 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u32,
    /// Oldest first; the set that ran most recently is last.
    sets: Vec<String>,
}

/// What the record's file held when it was read.
enum Stored {
    /// The sets it names, none when there is no record yet.
    Sets(Vec<String>),
    /// A regular file that is not a record. It names nothing, and the next
    /// set to record itself replaces it.
    Corrupt,
    /// Something that could not be read as a file. It names nothing and is
    /// left as it is.
    Unreadable,
}

fn stored(path: &Path) -> Stored {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Stored::Sets(Vec::new());
        }
        Err(_) => return Stored::Unreadable,
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Stored::Unreadable;
    }
    if metadata.len() > MAX_BYTES {
        return Stored::Corrupt;
    }
    let Ok(bytes) = read_bounded(path, MAX_BYTES) else {
        return Stored::Unreadable;
    };
    match serde_json::from_slice::<Document>(&bytes) {
        Ok(document) if valid(&document) => Stored::Sets(document.sets),
        _ => Stored::Corrupt,
    }
}

fn valid(document: &Document) -> bool {
    document.schema == 1
        && document.sets.len() <= CAPACITY
        && document
            .sets
            .iter()
            .all(|digest| package_status::is_sha256(digest))
        && document.sets.iter().collect::<BTreeSet<_>>().len() == document.sets.len()
}

/// The sets the record names. A record that is missing, unreadable or not a
/// record names none; reading it never fails.
pub(super) fn recorded(repository_directory: &Path) -> Vec<String> {
    match stored(&repository_directory.join(FILE)) {
        Stored::Sets(sets) => sets,
        Stored::Corrupt | Stored::Unreadable => Vec::new(),
    }
}

/// Records `running` as the set that ran most recently and returns the sets
/// the record durably names afterwards. A set already named moves to the end,
/// and the oldest names go once the record is full. A write that fails
/// changes nothing, and an unreadable record is not written over.
pub(super) fn record(repository_directory: &Path, running: &str) -> Vec<String> {
    if !package_status::is_sha256(running) {
        return recorded(repository_directory);
    }
    let path = repository_directory.join(FILE);
    let before = match stored(&path) {
        Stored::Sets(sets) if sets.last().map(String::as_str) == Some(running) => return sets,
        Stored::Sets(sets) => sets,
        Stored::Corrupt => Vec::new(),
        Stored::Unreadable => return Vec::new(),
    };
    let mut sets: Vec<String> = before
        .iter()
        .filter(|digest| digest.as_str() != running)
        .cloned()
        .collect();
    sets.push(running.to_owned());
    let excess = sets.len().saturating_sub(CAPACITY);
    sets.drain(..excess);
    let document = Document { schema: 1, sets };
    let written = (|| {
        if !repository_directory
            .try_exists()
            .map_err(|error| error.to_string())?
        {
            create_private_directory(repository_directory)?;
        }
        write_json_atomically(&path, &document)
    })();
    match written {
        Ok(()) => document.sets,
        Err(_) => before,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "amaterasu-switching-record-{}",
                amaterasu_package::random_identifier().unwrap()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn file(&self) -> PathBuf {
            self.0.join(FILE)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn digest(number: usize) -> String {
        format!("{number:064x}")
    }

    #[test]
    fn a_set_is_recorded_once_and_the_most_recent_is_last() {
        let directory = Directory::new();
        assert!(recorded(&directory.0).is_empty());
        assert_eq!(record(&directory.0, &digest(1)), [digest(1)]);
        assert_eq!(record(&directory.0, &digest(2)), [digest(1), digest(2)]);
        // Running again moves a set to the end without naming it twice.
        assert_eq!(record(&directory.0, &digest(1)), [digest(2), digest(1)]);
        assert_eq!(recorded(&directory.0), [digest(2), digest(1)]);

        // Recording the set that is already last writes nothing.
        let before = fs::metadata(directory.file()).unwrap().modified().unwrap();
        let bytes = fs::read(directory.file()).unwrap();
        assert_eq!(record(&directory.0, &digest(1)), [digest(2), digest(1)]);
        assert_eq!(fs::read(directory.file()).unwrap(), bytes);
        assert_eq!(
            fs::metadata(directory.file()).unwrap().modified().unwrap(),
            before
        );

        // The file is private to the user the host runs as.
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(directory.file()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({"schema": 1, "sets": [digest(2), digest(1)]})
        );
    }

    #[test]
    fn the_record_keeps_the_sets_that_ran_most_recently() {
        let directory = Directory::new();
        for number in 0..CAPACITY + 40 {
            record(&directory.0, &digest(number));
        }
        let sets = recorded(&directory.0);
        assert_eq!(sets.len(), CAPACITY);
        assert_eq!(sets.first(), Some(&digest(40)));
        assert_eq!(sets.last(), Some(&digest(CAPACITY + 39)));
        assert!(fs::metadata(directory.file()).unwrap().len() < MAX_BYTES);
        // A set that runs again stays, at the expense of no other.
        assert_eq!(record(&directory.0, &digest(40)).len(), CAPACITY);
        assert_eq!(recorded(&directory.0).last(), Some(&digest(40)));
        assert_eq!(recorded(&directory.0).first(), Some(&digest(41)));
    }

    #[test]
    fn a_file_that_is_not_a_record_names_nothing_and_is_replaced() {
        let directory = Directory::new();
        let oversized = format!(
            r#"{{"schema":1,"sets":[{}]}}"#,
            (0..CAPACITY + 1)
                .map(|number| format!("\"{}\"", digest(number)))
                .collect::<Vec<_>>()
                .join(",")
        );
        for contents in [
            b"not json".to_vec(),
            br#"{"schema":2,"sets":[]}"#.to_vec(),
            br#"{"schema":1,"sets":["previous"]}"#.to_vec(),
            br#"{"schema":1,"sets":[],"trusted":true}"#.to_vec(),
            format!(r#"{{"schema":1,"sets":["{0}","{0}"]}}"#, digest(7)).into_bytes(),
            oversized.into_bytes(),
            vec![b' '; MAX_BYTES as usize + 1],
        ] {
            fs::write(directory.file(), &contents).unwrap();
            assert!(recorded(&directory.0).is_empty());
            // Reading leaves the file for whoever wants to look at it.
            assert_eq!(fs::read(directory.file()).unwrap(), contents);
            assert_eq!(record(&directory.0, &digest(3)), [digest(3)]);
            assert_eq!(recorded(&directory.0), [digest(3)]);
        }
    }

    #[test]
    fn something_that_cannot_be_read_names_nothing_and_is_left_alone() {
        // A directory where the file belongs.
        let directory = Directory::new();
        fs::create_dir(directory.file()).unwrap();
        assert!(recorded(&directory.0).is_empty());
        assert!(record(&directory.0, &digest(1)).is_empty());
        assert!(directory.file().is_dir());

        // A link to a record kept somewhere else.
        let directory = Directory::new();
        let elsewhere = Directory::new();
        record(&elsewhere.0, &digest(1));
        std::os::unix::fs::symlink(elsewhere.file(), directory.file()).unwrap();
        assert!(recorded(&directory.0).is_empty());
        assert!(record(&directory.0, &digest(2)).is_empty());
        assert_eq!(recorded(&elsewhere.0), [digest(1)]);

        // A malformed digest is never written.
        let directory = Directory::new();
        assert!(record(&directory.0, "previous").is_empty());
        assert!(!directory.file().exists());

        // A repository directory that does not exist yet is created private.
        let parent = Directory::new();
        let missing = parent.0.join("repositories");
        assert!(recorded(&missing).is_empty());
        assert_eq!(record(&missing, &digest(1)), [digest(1)]);
        assert_eq!(recorded(&missing), [digest(1)]);
    }

    #[test]
    fn a_write_that_fails_reports_what_the_record_still_names() {
        use std::os::unix::fs::PermissionsExt;
        let directory = Directory::new();
        record(&directory.0, &digest(1));
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o500)).unwrap();
        // A process that may write anywhere cannot be refused this way.
        let refused = fs::write(directory.0.join("probe"), b"").is_err();
        let named = record(&directory.0, &digest(2));
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();
        if refused {
            assert_eq!(named, [digest(1)]);
            assert_eq!(recorded(&directory.0), [digest(1)]);
        }
    }
}
