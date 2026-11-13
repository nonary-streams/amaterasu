//! Which set a repository offers this host, and how it differs from the
//! running one. Everything here is a pure decision over verified metadata.
use super::environment::RunningSet;
use amaterasu_package::repository::{Candidate, CandidateKind};
use amaterasu_package::{ComponentManifest, ReleaseManifest};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Modules one status reply lists.
const MAX_LISTED_MODULES: usize = 128;
/// Bytes of module rows one status reply carries, so the whole reply stays
/// below the management response limit whatever names a manifest declares.
const LISTED_MODULE_BYTES: usize = 16 * 1024;

/// The catalog does not single out one set for this host.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Ambiguous;

/// The native candidates of the one line this host follows.
///
/// A line is a `(slot, implementation)` pair. The host stays on the line that
/// still lists its running set; a host whose set is no longer listed follows
/// the only line the repository offers.
pub(crate) fn update_line<'a>(
    candidates: &'a [Candidate],
    running_digest: &str,
) -> Result<Vec<&'a Candidate>, Ambiguous> {
    let native: Vec<&Candidate> = candidates
        .iter()
        .filter(|candidate| candidate.kind == CandidateKind::NativeSet)
        .collect();
    let line_of =
        |candidate: &&'a Candidate| (candidate.slot.as_str(), candidate.implementation.as_str());
    let running_lines: BTreeSet<_> = native
        .iter()
        .filter(|candidate| candidate.manifest_sha256 == running_digest)
        .map(line_of)
        .collect();
    let lines = if running_lines.is_empty() {
        native.iter().map(line_of).collect()
    } else {
        running_lines
    };
    let mut lines = lines.into_iter();
    let (Some(line), None) = (lines.next(), lines.next()) else {
        return Err(Ambiguous);
    };
    Ok(native
        .iter()
        .filter(|candidate| line_of(candidate) == line)
        .copied()
        .collect())
}

/// What a check found.
pub(crate) enum Checked {
    /// Nothing newer than the running set is offered.
    Current,
    /// The repository serves its publisher's own catalog, older than the one
    /// this host already accepted. It can offer this host nothing.
    RepositoryBehind,
    Available(Box<Offer>),
}

/// Chooses the newest offered set and decides whether it is an update.
///
/// `offered` holds the line's candidates with their authenticated manifests.
/// A set is an update only when it is not the running one and its release
/// sequence is above both the running set's and the highest the host has
/// accepted, which is the floor the host refuses to activate at or below.
pub(crate) fn select_update(
    offered: Vec<(Candidate, ReleaseManifest)>,
    running: &RunningSet,
) -> Result<Checked, Ambiguous> {
    let floor = running.manifest.sequence.max(running.accepted_sequence);
    let mut newer = offered.into_iter().filter(|(candidate, manifest)| {
        candidate.manifest_sha256 != running.digest && manifest.sequence > floor
    });
    let Some(mut newest) = newer.next() else {
        return Ok(Checked::Current);
    };
    let mut contested = false;
    for offer in newer {
        if offer.1.sequence > newest.1.sequence {
            newest = offer;
            contested = false;
        } else if offer.1.sequence == newest.1.sequence
            && offer.0.manifest_sha256 != newest.0.manifest_sha256
        {
            contested = true;
        }
    }
    if contested {
        // Two different sets claim the same newest release.
        return Err(Ambiguous);
    }
    let (candidate, manifest) = newest;
    Ok(Checked::Available(Box::new(Offer::new(
        candidate,
        manifest,
        &running.manifest,
    ))))
}

/// A newer set the source repository offers, with its authenticated manifest.
pub(crate) struct Offer {
    pub candidate: Candidate,
    pub manifest: ReleaseManifest,
    pub modules: Vec<ModuleChange>,
    /// Bytes of signed files the running set does not already hold.
    pub download_bytes: u64,
    /// Bytes of every signed file of the offered set.
    pub total_bytes: u64,
}

impl Offer {
    fn new(candidate: Candidate, manifest: ReleaseManifest, running: &ReleaseManifest) -> Self {
        let modules = module_changes(&running.components, &manifest.components);
        let (download_bytes, total_bytes) = byte_totals(running, &manifest);
        Self {
            candidate,
            manifest,
            modules,
            download_bytes,
            total_bytes,
        }
    }

    /// The `update` object of a status reply.
    pub(crate) fn describe(&self, repository: &str) -> Value {
        json!({
            "repository": repository,
            "slot": self.candidate.slot,
            "implementation": self.candidate.implementation,
            "version": self.candidate.version,
            "digest": self.candidate.manifest_sha256,
            "sequence": self.manifest.sequence.to_string(),
            "modules": listed_modules(&self.modules),
            "download_bytes": self.download_bytes,
            "total_bytes": self.total_bytes,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Change {
    /// The version differs.
    Updated,
    /// The version is the same and the library bytes are not.
    Rebuilt,
    Added,
    Removed,
    Unchanged,
}

impl Change {
    fn name(self) -> &'static str {
        match self {
            Self::Updated => "updated",
            Self::Rebuilt => "rebuilt",
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Unchanged => "unchanged",
        }
    }
}

/// How one module differs between the running set and the offered one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModuleChange {
    pub id: String,
    /// The running version; absent for an added module.
    pub from: Option<String>,
    /// The offered version; absent for a removed module.
    pub to: Option<String>,
    pub change: Change,
    /// The offered library's size; zero for a removed module.
    pub size: u64,
}

/// Every module of the offered set and every module only the running set has,
/// ordered by identity.
pub(crate) fn module_changes(
    running: &[ComponentManifest],
    offered: &[ComponentManifest],
) -> Vec<ModuleChange> {
    let running: BTreeMap<&str, &ComponentManifest> = running
        .iter()
        .map(|component| (component.id.as_str(), component))
        .collect();
    let offered: BTreeMap<&str, &ComponentManifest> = offered
        .iter()
        .map(|component| (component.id.as_str(), component))
        .collect();
    let identities: BTreeSet<&str> = running.keys().chain(offered.keys()).copied().collect();
    identities
        .into_iter()
        .map(|id| {
            let (from, to) = (running.get(id), offered.get(id));
            let change = match (from, to) {
                (Some(from), Some(to)) if from.version != to.version => Change::Updated,
                (Some(from), Some(to)) if from.sha256 != to.sha256 => Change::Rebuilt,
                (Some(_), Some(_)) => Change::Unchanged,
                (None, _) => Change::Added,
                (Some(_), None) => Change::Removed,
            };
            ModuleChange {
                id: id.into(),
                from: from.map(|component| component.version.clone()),
                to: to.map(|component| component.version.clone()),
                change,
                size: to.map_or(0, |component| component.size),
            }
        })
        .collect()
}

/// One signed file of a set: a module library or a privileged helper.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Payload<'a> {
    pub file: &'a str,
    pub sha256: &'a str,
    pub size: u64,
}

/// The files a set's manifest signs, which are the files a download fetches.
pub(crate) fn payloads(manifest: &ReleaseManifest) -> impl Iterator<Item = Payload<'_>> {
    let libraries = manifest.components.iter().map(|component| Payload {
        file: &component.file,
        sha256: &component.sha256,
        size: component.size,
    });
    let helpers = manifest.privileged_requests.iter().map(|request| Payload {
        file: &request.helper.file,
        sha256: &request.helper.sha256,
        size: request.helper.size,
    });
    libraries.chain(helpers)
}

/// The running set's file that holds the same bytes as an offered one.
pub(crate) fn held_by<'a>(
    running: &'a ReleaseManifest,
    offered: Payload<'_>,
) -> Option<Payload<'a>> {
    payloads(running).find(|held| held.sha256 == offered.sha256 && held.size == offered.size)
}

/// `(download_bytes, total_bytes)` for an offered set: the bytes the running
/// set does not already hold, and the bytes of every signed file.
pub(crate) fn byte_totals(running: &ReleaseManifest, offered: &ReleaseManifest) -> (u64, u64) {
    payloads(offered).fold((0_u64, 0_u64), |(download, total), payload| {
        let fetched = if held_by(running, payload).is_some() {
            0
        } else {
            payload.size
        };
        (
            download.saturating_add(fetched),
            total.saturating_add(payload.size),
        )
    })
}

fn listed_modules(modules: &[ModuleChange]) -> Vec<Value> {
    let mut listed_bytes = 0_usize;
    modules
        .iter()
        .take(MAX_LISTED_MODULES)
        .map(|module| {
            json!({
                "id": module.id,
                "from": module.from,
                "to": module.to,
                "change": module.change.name(),
                "size": module.size,
            })
        })
        .take_while(|row| {
            listed_bytes = listed_bytes.saturating_add(row.to_string().len());
            listed_bytes <= LISTED_MODULE_BYTES
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_contracts::Grants;
    use amaterasu_package::{PrivilegedOperation, PrivilegedRequest};
    use std::path::PathBuf;

    fn candidate(slot: &str, implementation: &str, version: &str, digest: char) -> Candidate {
        Candidate {
            slot: slot.into(),
            implementation: implementation.into(),
            version: version.into(),
            kind: CandidateKind::NativeSet,
            manifest_sha256: digest.to_string().repeat(64),
        }
    }

    fn session(version: &str, digest: char) -> Candidate {
        candidate(
            "amaterasu.runtime",
            "amaterasu-linux-session",
            version,
            digest,
        )
    }

    fn component(id: &str, version: &str, contents: char, size: u64) -> ComponentManifest {
        ComponentManifest {
            id: id.into(),
            version: version.into(),
            file: format!("{id}.so"),
            sha256: contents.to_string().repeat(64),
            size,
            abi_major: 1,
            runtime: id == "amaterasu.runtime",
            requires: BTreeMap::new(),
            requested: Grants::default(),
        }
    }

    fn manifest(sequence: u64, components: Vec<ComponentManifest>) -> ReleaseManifest {
        ReleaseManifest {
            schema: 1,
            host_context_schema: 1,
            project: "amaterasu".into(),
            sequence,
            expires_unix: u64::MAX,
            target: "linux-x86_64".into(),
            components,
            privileged_requests: Vec::new(),
        }
    }

    fn running(sequence: u64) -> RunningSet {
        RunningSet {
            digest: "a".repeat(64),
            manifest: manifest(
                sequence,
                vec![component("amaterasu.runtime", "0.1.0", '1', 10)],
            ),
            directory: PathBuf::from("/nonexistent/sets/running"),
            accepted_sequence: sequence,
        }
    }

    fn lines(chosen: &[&Candidate]) -> Vec<String> {
        chosen
            .iter()
            .map(|candidate| candidate.manifest_sha256[..1].to_owned())
            .collect()
    }

    #[test]
    fn the_host_stays_on_the_line_that_lists_its_running_set() {
        let candidates = vec![
            session("0.1.0", 'a'),
            session("0.2.0", 'b'),
            candidate(
                "amaterasu.rtx-hdr",
                "amaterasu-linux-session-rtx-hdr",
                "0.2.0",
                'c',
            ),
        ];
        let line = update_line(&candidates, &"a".repeat(64)).unwrap();
        assert_eq!(lines(&line), ["a", "b"]);
        // The same repository serves an RTX HDR host its own line.
        let line = update_line(&candidates, &"c".repeat(64)).unwrap();
        assert_eq!(lines(&line), ["c"]);
    }

    #[test]
    fn an_unlisted_running_set_follows_the_only_offered_line() {
        let mut web = session("7", 'e');
        web.slot = "amaterasu.ui".into();
        web.kind = CandidateKind::WebBundle;
        let candidates = vec![session("0.2.0", 'b'), session("0.3.0", 'c'), web];
        let line = update_line(&candidates, &"a".repeat(64)).unwrap();
        assert_eq!(lines(&line), ["b", "c"]);
    }

    #[test]
    fn a_catalog_that_singles_out_no_line_is_ambiguous() {
        let rtx = candidate(
            "amaterasu.rtx-hdr",
            "amaterasu-linux-session-rtx-hdr",
            "0.2.0",
            'c',
        );
        // Two lines and the running set in neither.
        let two_lines = vec![session("0.2.0", 'b'), rtx.clone()];
        assert_eq!(
            update_line(&two_lines, &"a".repeat(64)).unwrap_err(),
            Ambiguous
        );
        // The same slot from two implementations is two lines.
        let two_implementations = vec![
            session("0.2.0", 'b'),
            candidate("amaterasu.runtime", "other-session", "0.2.0", 'd'),
        ];
        assert!(update_line(&two_implementations, &"a".repeat(64)).is_err());
        // The running set listed on two lines does not choose between them.
        let mut relisted = rtx;
        relisted.manifest_sha256 = "a".repeat(64);
        let listed_twice = vec![session("0.1.0", 'a'), relisted];
        assert!(update_line(&listed_twice, &"a".repeat(64)).is_err());
        // Nothing native is offered at all.
        assert!(update_line(&[], &"a".repeat(64)).is_err());
    }

    fn offered(version: &str, digest: char, sequence: u64) -> (Candidate, ReleaseManifest) {
        (
            session(version, digest),
            manifest(
                sequence,
                vec![component("amaterasu.runtime", version, digest, 20)],
            ),
        )
    }

    fn chosen(checked: Result<Checked, Ambiguous>) -> Option<String> {
        match checked.unwrap() {
            Checked::Current | Checked::RepositoryBehind => None,
            Checked::Available(offer) => Some(offer.candidate.version.clone()),
        }
    }

    #[test]
    fn the_newest_set_above_the_running_sequence_is_the_update() {
        let running = running(10);
        let update = select_update(
            vec![
                offered("0.2.0", 'b', 11),
                offered("0.4.0", 'd', 13),
                offered("0.3.0", 'c', 12),
            ],
            &running,
        );
        assert_eq!(chosen(update).as_deref(), Some("0.4.0"));
    }

    #[test]
    fn an_older_or_equal_sequence_is_not_an_update() {
        let running = running(10);
        assert_eq!(chosen(select_update(vec![], &running)), None);
        assert_eq!(
            chosen(select_update(vec![offered("0.0.9", 'b', 9)], &running)),
            None
        );
        // The same release number with other bytes is never an update.
        assert_eq!(
            chosen(select_update(vec![offered("0.1.1", 'b', 10)], &running)),
            None
        );
    }

    #[test]
    fn the_running_digest_is_current_whatever_sequence_it_carries() {
        let running = running(10);
        assert_eq!(
            chosen(select_update(vec![offered("0.1.0", 'a', 99)], &running)),
            None
        );
    }

    #[test]
    fn a_set_the_host_would_refuse_as_a_replay_is_not_offered() {
        // The host accepted release 20 before returning to release 10.
        let mut running = running(10);
        running.accepted_sequence = 20;
        assert_eq!(
            chosen(select_update(vec![offered("0.2.0", 'b', 20)], &running)),
            None
        );
        assert_eq!(
            chosen(select_update(vec![offered("0.3.0", 'c', 21)], &running)).as_deref(),
            Some("0.3.0")
        );
    }

    #[test]
    fn two_sets_claiming_the_newest_release_are_ambiguous() {
        let running = running(10);
        let contested = vec![offered("0.2.0", 'b', 12), offered("0.2.1", 'c', 12)];
        assert!(select_update(contested, &running).is_err());
        // An older tie does not matter once a newer release exists.
        let settled = vec![
            offered("0.2.0", 'b', 12),
            offered("0.2.1", 'c', 12),
            offered("0.3.0", 'd', 13),
        ];
        assert_eq!(
            chosen(select_update(settled, &running)).as_deref(),
            Some("0.3.0")
        );
        // Neither does a tie the running set already superseded.
        let superseded = vec![offered("0.0.8", 'b', 9), offered("0.0.9", 'c', 9)];
        assert_eq!(chosen(select_update(superseded, &running)), None);
    }

    #[test]
    fn module_changes_name_every_difference_between_two_sets() {
        let running = [
            component("amaterasu.audio", "0.1.0", '1', 100),
            component("amaterasu.display", "0.1.0", '2', 200),
            component("amaterasu.input", "0.1.0", '3', 300),
            component("amaterasu.legacy", "0.1.0", '4', 400),
        ];
        let offered = [
            // Listed out of order: the reply is ordered by identity.
            component("amaterasu.stats", "0.1.0", '9', 900),
            component("amaterasu.input", "0.2.0", '8', 350),
            component("amaterasu.display", "0.1.0", '7', 250),
            component("amaterasu.audio", "0.1.0", '1', 100),
        ];
        let row = |id: &str, from: Option<&str>, to: Option<&str>, change, size| ModuleChange {
            id: id.into(),
            from: from.map(str::to_owned),
            to: to.map(str::to_owned),
            change,
            size,
        };
        assert_eq!(
            module_changes(&running, &offered),
            [
                row(
                    "amaterasu.audio",
                    Some("0.1.0"),
                    Some("0.1.0"),
                    Change::Unchanged,
                    100
                ),
                row(
                    "amaterasu.display",
                    Some("0.1.0"),
                    Some("0.1.0"),
                    Change::Rebuilt,
                    250
                ),
                row(
                    "amaterasu.input",
                    Some("0.1.0"),
                    Some("0.2.0"),
                    Change::Updated,
                    350
                ),
                row("amaterasu.legacy", Some("0.1.0"), None, Change::Removed, 0),
                row("amaterasu.stats", None, Some("0.1.0"), Change::Added, 900),
            ]
        );
    }

    fn helper_request(contents: char, size: u64) -> PrivilegedRequest {
        PrivilegedRequest {
            schema: 1,
            component: "amaterasu.capture".into(),
            operation: PrivilegedOperation::VkmsFrameExportV1,
            target: "linux-x86_64".into(),
            resource: amaterasu_package::PrivilegedResourceScope { drm_card: 0 },
            helper: amaterasu_package::PrivilegedHelperArtifact {
                file: "capture-helper".into(),
                sha256: contents.to_string().repeat(64),
                size,
            },
        }
    }

    #[test]
    fn byte_totals_count_only_files_the_running_set_does_not_hold() {
        let mut running = manifest(
            1,
            vec![
                component("amaterasu.runtime", "0.1.0", '1', 100),
                component("amaterasu.input", "0.1.0", '2', 200),
            ],
        );
        let mut offered = manifest(
            2,
            vec![
                component("amaterasu.runtime", "0.1.0", '1', 100),
                component("amaterasu.input", "0.2.0", '3', 250),
                component("amaterasu.stats", "0.1.0", '4', 900),
            ],
        );
        assert_eq!(byte_totals(&running, &offered), (1150, 1250));
        // A module that moved to another identity still holds the same bytes.
        offered.components[2].sha256 = "2".repeat(64);
        offered.components[2].size = 200;
        assert_eq!(byte_totals(&running, &offered), (250, 550));
        // A matching digest with another size is not the same file.
        offered.components[2].size = 201;
        assert_eq!(byte_totals(&running, &offered), (451, 551));

        // A privileged helper is a signed file a download fetches too.
        offered.privileged_requests.push(helper_request('5', 40));
        assert_eq!(byte_totals(&running, &offered), (491, 591));
        running.privileged_requests.push(helper_request('5', 40));
        assert_eq!(byte_totals(&running, &offered), (451, 591));
        assert_eq!(byte_totals(&offered, &offered), (0, 591));
    }

    #[test]
    fn status_rows_are_bounded_in_count_and_bytes() {
        let update = |components: Vec<ComponentManifest>| {
            let running = running(1);
            let checked = select_update(
                vec![(session("0.2.0", 'b'), manifest(2, components))],
                &running,
            );
            let Checked::Available(offer) = checked.unwrap() else {
                panic!("an update is offered");
            };
            offer.describe("amaterasu")
        };
        let many: Vec<_> = (0..200)
            .map(|index| component(&format!("example.module-{index:03}"), "0.2.0", 'b', 1))
            .collect();
        let described = update(many);
        assert_eq!(described["modules"].as_array().unwrap().len(), 128);
        assert_eq!(described["total_bytes"], 200);
        assert_eq!(described["sequence"], "2");
        assert_eq!(described["repository"], "amaterasu");

        // The longest names a manifest may declare still fit one reply.
        let long: Vec<_> = (0..128)
            .map(|index| {
                component(
                    &format!("{index:03}{}", "m".repeat(125)),
                    &"9".repeat(128),
                    'b',
                    1,
                )
            })
            .collect();
        let described = update(long);
        let rows = described["modules"].as_array().unwrap();
        assert!(!rows.is_empty() && rows.len() < 128);
        assert!(described.to_string().len() < crate::http::MAX_RESPONSE - 2048);
        assert_eq!(described["total_bytes"], 128);
    }
}
