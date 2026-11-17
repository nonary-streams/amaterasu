//! Stream failures this module reports to the launch owner's lifecycle store.
//!
//! A session knows the launch owner's stream key as its `rtsp_operation_id`.
//! Its failure paths queue issues on the record; the tick publishes them on
//! the SDK's stream-issue topic, where the runtime stamps this module as the
//! reporter.
use super::{Pipeline, Send, record::Record, record::Resource, workflow::Step};
use amaterasu_module_sdk::stream_lifecycle::{LifecycleLayer, StreamIssue};

/// The layer a session step depends on.
pub(super) fn step_layer(step: Step) -> LifecycleLayer {
    use Step::*;
    match step {
        Open(resource)
        | ProbeAudio(resource)
        | ProbeAudioReconfiguration(resource)
        | ApplyAudioReconfiguration(resource)
        | Find(resource)
        | Close(resource)
        | AwaitClosed(resource) => resource_layer(resource),
        Authenticate | ConfirmControlDisconnect => LifecycleLayer::ControlTransport,
        MediaReady | ConfigureMediaVideoFormat | TransportStatus => LifecycleLayer::MediaTransport,
        CreateVirtualDisplay
        | StatusVirtualDisplay
        | RemoveVirtualDisplay
        | CreateOwnedVirtualDisplay
        | FindOwnedVirtualDisplay
        | RemoveOwnedVirtualDisplay => LifecycleLayer::VirtualDisplay,
        SnapshotDisplay | CheckDisplay | ApplyDisplay | FindDisplay | RestoreDisplay
        | RecoverDisplay => LifecycleLayer::Display,
        AcquireOutput
        | OpenSourceScope
        | ReadSourceHookSettings
        | PrepareSourceHook
        | CloseSourceScope
        | DrainVideo
        | CloseCapture
        | ForceIdr => LifecycleLayer::Capture,
        ValidateLifecycleCandidate
        | ReadFrozenProcessingSettings
        | PlanGpuProcessing
        | ObserveLifecycleCapture
        | AckLifecycleCapture
        | AckLifecycleStream
        | StartLifecycle
        | PauseLifecycle
        | RetireLifecycle
        | RecoverLifecycle
        | ResolveLifecyclePreparation
        | ObserveActiveBinding
        | ValidateActiveBinding
        | NotifyCaptureInvalidated
        | IssueLifecycleInputBinding
        | AckLifecycleInput => LifecycleLayer::SessionLifecycle,
        ProbeExtensionSource | WaitVideoReady => LifecycleLayer::EncodeVideo,
        AcquireAudio | RecoverAudio => LifecycleLayer::Audio,
        AcquireInput | FindInput | ReleaseInput | RenewInput | InputBatch | FeedbackPeek
        | FeedbackAck | PollGamepadFeedback | RumbleDrain => LifecycleLayer::Input,
        Activate | WaitDrain | StopSession | FinishSession | PersistJournal => {
            LifecycleLayer::StreamSession
        }
    }
}

fn resource_layer(resource: Resource) -> LifecycleLayer {
    match resource {
        Resource::Session => LifecycleLayer::StreamSession,
        Resource::AudioEncoder => LifecycleLayer::AudioEncode,
        Resource::AudioProcessor | Resource::AudioCapture => LifecycleLayer::Audio,
        Resource::Media => LifecycleLayer::MediaTransport,
        Resource::Control => LifecycleLayer::ControlTransport,
        Resource::Video => LifecycleLayer::EncodeVideo,
    }
}

/// The layer whose failure a media plane code reports.
pub(super) fn media_layer(code: &str) -> LifecycleLayer {
    if code.starts_with("audio_encoder") {
        LifecycleLayer::AudioEncode
    } else if code.starts_with("audio") {
        LifecycleLayer::Audio
    } else if code.starts_with("video") || code.starts_with("encoder") {
        LifecycleLayer::EncodeVideo
    } else if code.starts_with("capture") {
        LifecycleLayer::Capture
    } else {
        LifecycleLayer::MediaTransport
    }
}

/// `WaitDrain` becomes `wait_drain`; `Open(Video)` becomes `open_video`.
pub(super) fn step_label(step: Step) -> String {
    let name = format!("{step:?}");
    let mut label = String::with_capacity(name.len() + 4);
    for (index, character) in name.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index > 0 && !label.ends_with('_') {
                label.push('_');
            }
            label.push(character.to_ascii_lowercase());
        } else if character.is_ascii_alphanumeric() {
            label.push(character);
        } else if character == '(' {
            label.push('_');
        }
    }
    label
}

impl Record {
    #[track_caller]
    pub(super) fn report_extension_probe_fallback(&mut self, reason: &str) {
        let Some(operation) = self.config.rtsp_operation_id.clone() else {
            return;
        };
        self.issues.push_logged(
            None,
            StreamIssue::demoted(
                operation,
                LifecycleLayer::StreamSession,
                "probe_extension_source",
                "extension_source_unavailable",
                "negotiate_standard_video",
            )
            .because(reason)
            .caused_by(LifecycleLayer::EncodeVideo),
        );
    }
    /// Record a failure that stops this session, under this module's own
    /// layer. `dependency` names the layer it was reacting to; that layer
    /// reports its own failure. Sessions no launch owner named have nowhere
    /// to report and record nothing.
    #[track_caller]
    pub(super) fn report_failure(&mut self, dependency: LifecycleLayer, phase: &str, code: &str) {
        let Some(operation) = self.config.rtsp_operation_id.clone() else {
            return;
        };
        let mut issue =
            StreamIssue::critical(operation, LifecycleLayer::StreamSession, phase, code);
        if dependency != LifecycleLayer::StreamSession {
            issue = issue.caused_by(dependency);
        }
        self.issues.push(issue);
    }
}

impl Pipeline {
    /// Report on the launch owner's topic. Off by default so a pipeline in a
    /// test sends only the commands that test scripts.
    pub fn enable_stream_issue_reports(&mut self) {
        self.report_stream_issues = true;
    }

    /// Publish what each session recorded. A refused publication keeps the
    /// rest for the next tick; the outbox is bounded either way.
    pub(super) fn publish_stream_issues(&mut self, send: &mut Send<'_>) {
        for record in self.records.values_mut() {
            if !self.report_stream_issues {
                record.issues.drain().for_each(drop);
                continue;
            }
            if record.issues.is_empty() {
                continue;
            }
            if let Err(error) = record.issues.flush_with(&mut |command| send(command)) {
                eprintln!("amaterasu session stream issue report failed error={error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_labels_are_identifiers() {
        assert_eq!(step_label(Step::WaitDrain), "wait_drain");
        assert_eq!(step_label(Step::Open(Resource::Video)), "open_video");
    }

    #[test]
    fn media_codes_name_their_layer() {
        assert_eq!(
            media_layer("video_encoder_failed"),
            LifecycleLayer::EncodeVideo
        );
        assert_eq!(
            media_layer("audio_encoder_failed"),
            LifecycleLayer::AudioEncode
        );
        assert_eq!(
            media_layer("transport_lost"),
            LifecycleLayer::MediaTransport
        );
    }
}
