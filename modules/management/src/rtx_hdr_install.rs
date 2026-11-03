//! Authenticated, bounded NVIDIA runtime staging for the optional RTX HDR path.
//! Native composition activation remains the responsibility of the installed host.
use amaterasu_module_sdk::{ModuleResult, network::Network};
use serde_json::{Value, json};
#[cfg(any(test, target_os = "linux"))]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::Duration;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
};

pub struct Installer {
    #[cfg(target_os = "linux")]
    parent: PathBuf,
    #[cfg(target_os = "linux")]
    network: Network,
    cancelled: Arc<AtomicBool>,
    transferred: Arc<AtomicU64>,
    job: Option<Job>,
    phase: &'static str,
    error: Option<&'static str>,
    runtime_ready: bool,
    #[cfg(target_os = "linux")]
    package_roots: Option<(PathBuf, PathBuf)>,
    #[cfg(target_os = "linux")]
    candidate_job: Option<
        JoinHandle<amaterasu_package::PackageResult<crate::rtx_hdr_candidate::PreparedCandidate>>,
    >,
    #[cfg(target_os = "linux")]
    candidate: Option<crate::rtx_hdr_candidate::PreparedCandidate>,
    install_requested: bool,
}

struct Job {
    thread: JoinHandle<std::io::Result<()>>,
    verifying: bool,
}

impl Installer {
    pub fn new(parent: PathBuf, network: Network) -> ModuleResult<Self> {
        if !parent.is_absolute()
            || parent
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err("RTX HDR runtime parent must be an absolute trusted path".into());
        }
        #[cfg(not(target_os = "linux"))]
        let _ = network;
        let installer = Self {
            #[cfg(target_os = "linux")]
            parent,
            #[cfg(target_os = "linux")]
            network,
            cancelled: Arc::new(AtomicBool::new(false)),
            transferred: Arc::new(AtomicU64::new(0)),
            job: None,
            phase: "idle",
            error: None,
            runtime_ready: false,
            #[cfg(target_os = "linux")]
            package_roots: None,
            #[cfg(target_os = "linux")]
            candidate_job: None,
            #[cfg(target_os = "linux")]
            candidate: None,
            install_requested: false,
        };
        #[cfg(target_os = "linux")]
        {
            let mut installer = installer;
            if installer
                .parent
                .join(amaterasu_rtx_runtime::DIRECTORY)
                .exists()
            {
                installer.start_verify()?;
            }
            Ok(installer)
        }
        #[cfg(not(target_os = "linux"))]
        Ok(installer)
    }

    pub fn configure_package(
        &mut self,
        repository_directory: Option<PathBuf>,
        package_root: Option<PathBuf>,
    ) {
        #[cfg(target_os = "linux")]
        {
            self.package_roots = repository_directory
                .zip(package_root)
                .filter(|(repository, root)| repository.is_absolute() && root.is_absolute());
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (repository_directory, package_root);
    }

    pub fn advance(&mut self) {
        self.retire_finished();
        #[cfg(target_os = "linux")]
        self.advance_candidate();
    }

    pub fn request(&mut self, action: &str) -> Value {
        self.advance();
        match action {
            "rtx_hdr_status" => {}
            "rtx_hdr_install" => {
                if matches!(self.phase, "waiting_stream" | "activating") {
                    return json!({"ok":false,"code":"activation_in_progress"});
                }
                if self.busy() {
                    return json!({"ok":false,"code":"installation_in_progress"});
                }
                if self.error == Some("runtime_verification_failed") {
                    return json!({"ok":false,"code":"runtime_verification_failed"});
                }
                self.install_requested = true;
                if !self.runtime_ready && self.resume_or_install().is_err() {
                    return json!({"ok":false,"code":"installation_unavailable"});
                }
                self.advance();
            }
            "rtx_hdr_cancel" => {
                if matches!(self.phase, "waiting_stream" | "activating") {
                    return json!({"ok":false,"code":"activation_in_progress"});
                }
                self.install_requested = false;
                if self.busy() {
                    self.cancelled.store(true, Ordering::Release);
                }
            }
            _ => return json!({"ok":false,"code":"invalid_request"}),
        }
        self.status()
    }

    /// Builds an exact request from host-injected roots and the verified cache;
    /// browser input cannot choose paths, publishers or package identities.
    #[cfg(target_os = "linux")]
    pub fn activation_request(
        &mut self,
    ) -> Result<amaterasu_contracts::HostPackageActivation, &'static str> {
        self.advance();
        if matches!(self.phase, "waiting_stream" | "activating") {
            return Err("activation_in_progress");
        }
        if self.busy() || !self.runtime_ready {
            return Err("runtime_not_ready");
        }
        let candidate = self.candidate.as_ref().ok_or("package_not_ready")?;
        let root = &self.package_roots.as_ref().ok_or("package_unavailable")?.1;
        let bytes = amaterasu_package::read_bounded(&root.join("selection.json"), 1024 * 1024)
            .map_err(|_| "selection_unavailable")?;
        let selection: amaterasu_package::ActivationState =
            serde_json::from_slice(&bytes).map_err(|_| "selection_unavailable")?;
        let active = selection.active.ok_or("selection_unavailable")?;
        if selection.pending.is_some() || selection.trial.is_some() {
            return Err("activation_in_progress");
        }
        Ok(amaterasu_contracts::HostPackageActivation {
            repository_id: candidate.repository_id.clone(),
            catalog_digest: candidate.catalog_digest.clone(),
            manifest_digest: candidate.digest.clone(),
            expected_active: active,
            expected_revision: amaterasu_package::sha256(&bytes),
        })
    }

    pub fn mark_activation_requested(&mut self) {
        self.phase = "waiting_stream";
        self.error = None;
    }

    pub fn waiting_for_stream(&self) -> bool {
        self.phase == "waiting_stream"
    }
    pub fn cancel_pending_activation(&mut self) {
        self.phase = "ready";
        self.error = None;
    }
    pub fn update_activation_state(&mut self, pending: bool, started: bool) {
        if !matches!(self.phase, "waiting_stream" | "activating") {
            return;
        }
        if started {
            self.phase = "activating";
        } else if pending {
            self.phase = "waiting_stream";
        } else {
            self.phase = "failed";
            self.error = Some("activation_interrupted");
        }
    }

    /// Reconcile a host-written receipt with the digest of the running set.
    /// The receipt reports progress only; it never authorizes native code.
    #[cfg(target_os = "linux")]
    pub fn reconcile_activation(&self, value: &mut Value, running_digest: Option<&str>) {
        use std::os::unix::fs::MetadataExt;
        let Some((_, root)) = &self.package_roots else {
            return;
        };
        let path = root.join("data/state/package-activation-status.json");
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            return;
        };
        // SAFETY: geteuid has no pointer or lifetime requirements.
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o777 != 0o600
        {
            return;
        }
        let Ok(bytes) = amaterasu_package::read_bounded(&path, 4096) else {
            return;
        };
        let Ok(receipt) = serde_json::from_slice::<Value>(&bytes) else {
            return;
        };
        if receipt.get("schema").and_then(Value::as_u64) != Some(1) {
            return;
        }
        let Some(digest) = receipt.get("digest").and_then(Value::as_str) else {
            return;
        };
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return;
        }
        match receipt.get("status").and_then(Value::as_str) {
            Some("committed") if running_digest == Some(digest) => {
                value["phase"] = json!("ready");
                value["package_ready"] = json!(true);
            }
            Some("failed")
                if !self.busy() && !matches!(self.phase, "waiting_stream" | "activating") =>
            {
                value["phase"] = json!("failed");
                value["error"] = json!("activation_failed");
            }
            _ => {}
        }
    }

    fn busy(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.job.is_some() || self.candidate_job.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.job.is_some()
        }
    }

    fn status(&self) -> Value {
        #[cfg(target_os = "linux")]
        let package_ready = self.candidate.is_some();
        #[cfg(not(target_os = "linux"))]
        let package_ready = false;
        json!({"ok":true,"supported":cfg!(target_os = "linux"),"phase":self.phase,"transferred_bytes":self.transferred.load(Ordering::Acquire),"total_bytes":null,"error":self.error,"runtime_ready":self.runtime_ready,"package_ready":package_ready,"provider_active":null,"restart_required":false})
    }

    #[cfg(target_os = "linux")]
    fn advance_candidate(&mut self) {
        if self
            .candidate_job
            .as_ref()
            .is_some_and(|job| job.is_finished())
        {
            let job = self
                .candidate_job
                .take()
                .expect("finished candidate job exists");
            match job.join() {
                Ok(Ok(candidate)) => {
                    self.candidate = Some(candidate);
                    self.phase = "ready";
                    self.error = None;
                }
                _ if self.cancelled.load(Ordering::Acquire) => {
                    self.phase = "cancelled";
                    self.error = None;
                }
                _ => {
                    self.phase = "failed";
                    self.error = Some("rtx_hdr_package_unavailable");
                }
            }
            self.install_requested = false;
        }
        if self.install_requested && self.runtime_ready && self.candidate.is_none() && !self.busy()
        {
            let Some((repository, root)) = self.package_roots.clone() else {
                self.phase = "failed";
                self.error = Some("rtx_hdr_package_unavailable");
                self.install_requested = false;
                return;
            };
            self.cancelled = Arc::new(AtomicBool::new(false));
            let cancelled = self.cancelled.clone();
            let transferred = self.transferred.clone();
            let network = self.network.clone();
            match thread::Builder::new()
                .name("amaterasu-rtx-package".into())
                .spawn(move || {
                    crate::rtx_hdr_candidate::prepare(
                        &repository,
                        &root,
                        network,
                        cancelled,
                        transferred,
                    )
                }) {
                Ok(job) => {
                    self.candidate_job = Some(job);
                    self.phase = "installing";
                    self.error = None;
                }
                Err(_) => {
                    self.phase = "failed";
                    self.error = Some("rtx_hdr_package_unavailable");
                    self.install_requested = false;
                }
            }
        }
    }

    fn retire_finished(&mut self) {
        if !self
            .job
            .as_ref()
            .is_some_and(|job| job.thread.is_finished())
        {
            return;
        }
        let job = self.job.take().expect("finished job exists");
        match job.thread.join() {
            Ok(Ok(())) => {
                self.phase = "ready";
                self.runtime_ready = true;
                self.error = None;
            }
            Ok(Err(error))
                if error.kind() == std::io::ErrorKind::Interrupted
                    || self.cancelled.load(Ordering::Acquire) =>
            {
                self.phase = "cancelled";
                self.error = None;
            }
            Ok(Err(_)) | Err(_) if job.verifying => {
                self.phase = "failed";
                self.error = Some("runtime_verification_failed");
                self.runtime_ready = false;
            }
            _ => {
                self.phase = "failed";
                self.error = Some("runtime_install_failed");
                self.runtime_ready = false;
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn resume_or_install(&mut self) -> ModuleResult<()> {
        if self.parent.join(amaterasu_rtx_runtime::DIRECTORY).exists() {
            self.cancelled = Arc::new(AtomicBool::new(false));
            self.start_verify()
        } else {
            self.start_install()
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn resume_or_install(&mut self) -> ModuleResult<()> {
        self.start_install()
    }

    #[cfg(target_os = "linux")]
    fn start_verify(&mut self) -> ModuleResult<()> {
        let parent = self.parent.clone();
        let cancel = self.cancelled.clone();
        self.phase = "verifying";
        self.job = Some(Job {
            verifying: true,
            thread: thread::Builder::new()
                .name("amaterasu-rtx-verify".into())
                .spawn(move || amaterasu_rtx_runtime::verify(&parent, &cancel).map(|_| ()))
                .map_err(|_| "runtime verification worker unavailable")?,
        });
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn start_install(&mut self) -> ModuleResult<()> {
        let parent = self.parent.clone();
        let network = self.network.clone();
        self.cancelled = Arc::new(AtomicBool::new(false));
        self.transferred = Arc::new(AtomicU64::new(0));
        let cancel = self.cancelled.clone();
        let transferred = self.transferred.clone();
        let thread = thread::Builder::new()
            .name("amaterasu-rtx-install".into())
            .spawn(move || {
                let config = ureq::Agent::config_builder()
                    .https_only(true)
                    .max_redirects(0)
                    .timeout_global(Some(Duration::from_secs(600)))
                    .timeout_resolve(Some(Duration::from_secs(10)))
                    .timeout_connect(Some(Duration::from_secs(10)))
                    .timeout_send_request(Some(Duration::from_secs(10)))
                    .timeout_recv_response(Some(Duration::from_secs(15)))
                    .timeout_recv_body(Some(Duration::from_secs(30)))
                    .max_response_header_size(8192)
                    .build();
                let agent = amaterasu_module_sdk::network_http::agent(config, network);
                let wheel =
                    amaterasu_rtx_runtime::RemoteWheel::open(&agent, cancel.clone(), transferred)?;
                amaterasu_rtx_runtime::install(wheel, &parent, &cancel).map(|_| ())
            })
            .map_err(|_| "runtime installation worker unavailable")?;
        self.job = Some(Job {
            verifying: false,
            thread,
        });
        self.phase = "downloading";
        self.error = None;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    fn start_install(&mut self) -> ModuleResult<()> {
        Err("RTX HDR installation is unavailable on this platform".into())
    }
}

impl Drop for Installer {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(job) = self.job.take() {
            let _ = job.thread.join();
        }
        #[cfg(target_os = "linux")]
        if let Some(job) = self.candidate_job.take() {
            let _ = job.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn retire(installer: &mut Installer) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while installer.job.is_some() {
            installer.retire_finished();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn activation_request_uses_only_the_prepared_candidate_and_current_selection() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-activation-request-{}",
            amaterasu_package::random_identifier().unwrap()
        ));
        std::fs::create_dir_all(root.join("data/state")).unwrap();
        let selection = amaterasu_package::ActivationState {
            active: Some("a".repeat(64)),
            ..Default::default()
        };
        amaterasu_package::write_json_atomically(&root.join("selection.json"), &selection).unwrap();
        let mut installer = Installer::new(
            PathBuf::from("/tmp/rtx-test-nonexistent"),
            Network::test_provider(),
        )
        .unwrap();
        installer.package_roots = Some((root.join("repositories"), root.clone()));
        assert!(installer.activation_request().is_err());
        installer.runtime_ready = true;
        installer.candidate = Some(crate::rtx_hdr_candidate::PreparedCandidate {
            repository_id: "approved".into(),
            catalog_digest: "b".repeat(64),
            digest: "c".repeat(64),
        });
        let request = installer.activation_request().unwrap();
        installer.mark_activation_requested();
        assert_eq!(
            installer.request("rtx_hdr_install")["code"],
            "activation_in_progress"
        );
        assert_eq!(
            installer.request("rtx_hdr_cancel")["code"],
            "activation_in_progress"
        );
        assert_eq!(
            installer.activation_request().unwrap_err(),
            "activation_in_progress"
        );
        installer.phase = "ready";
        assert_eq!(request.repository_id, "approved");
        assert_eq!(request.expected_active, "a".repeat(64));
        assert_eq!(
            request.expected_revision,
            amaterasu_package::sha256(&std::fs::read(root.join("selection.json")).unwrap())
        );
        let mut status = installer.status();
        amaterasu_package::write_json_atomically(
            &root.join("data/state/package-activation-status.json"),
            &json!({"schema":1,"digest":"c".repeat(64),"status":"failed"}),
        )
        .unwrap();
        installer.reconcile_activation(&mut status, Some(&"a".repeat(64)));
        assert_eq!(status["error"], "activation_failed");
        amaterasu_package::write_json_atomically(
            &root.join("data/state/package-activation-status.json"),
            &json!({"schema":1,"digest":"c".repeat(64),"status":"committed"}),
        )
        .unwrap();
        let mut status = installer.status();
        installer.candidate = None;
        installer.reconcile_activation(&mut status, Some(&"c".repeat(64)));
        assert_eq!(status["phase"], "ready");
        assert_eq!(status["package_ready"], true);
        assert!(status["provider_active"].is_null());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reports_ready_only_after_verified_worker_completes() {
        let mut installer = Installer::new(
            PathBuf::from("/tmp/rtx-test-nonexistent"),
            Network::test_provider(),
        )
        .unwrap();
        let (release, wait) = std::sync::mpsc::sync_channel::<()>(1);
        installer.phase = "verifying";
        installer.job = Some(Job {
            verifying: true,
            thread: thread::spawn(move || {
                wait.recv().unwrap();
                Ok(())
            }),
        });
        assert_eq!(installer.request("rtx_hdr_status")["runtime_ready"], false);
        assert_eq!(
            installer.request("rtx_hdr_install")["code"],
            "installation_in_progress"
        );
        release.send(()).unwrap();
        retire(&mut installer);
        let status = installer.request("rtx_hdr_status");
        assert_eq!(status["phase"], "ready");
        assert_eq!(status["runtime_ready"], true);
        assert!(status["provider_active"].is_null());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runtime_and_candidate_readiness_are_reported_independently() {
        let mut installer = Installer::new(
            PathBuf::from("/tmp/rtx-test-nonexistent"),
            Network::test_provider(),
        )
        .unwrap();
        installer.runtime_ready = true;
        installer.phase = "installing";
        let (release, wait) = std::sync::mpsc::sync_channel::<()>(1);
        installer.candidate_job = Some(thread::spawn(move || {
            wait.recv().unwrap();
            Ok(crate::rtx_hdr_candidate::PreparedCandidate {
                repository_id: "project".into(),
                catalog_digest: "a".repeat(64),
                digest: "b".repeat(64),
            })
        }));
        let pending = installer.request("rtx_hdr_status");
        assert_eq!(pending["runtime_ready"], true);
        assert_eq!(pending["package_ready"], false);
        assert_eq!(
            installer.request("rtx_hdr_install")["code"],
            "installation_in_progress"
        );
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while installer.candidate_job.is_some() {
            installer.advance();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let ready = installer.request("rtx_hdr_status");
        assert_eq!(ready["package_ready"], true);
        assert_eq!(ready["phase"], "ready");
        assert!(ready["provider_active"].is_null());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_candidate_configuration_reports_failure_without_claiming_activation() {
        let mut installer = Installer::new(
            PathBuf::from("/tmp/rtx-test-nonexistent"),
            Network::test_provider(),
        )
        .unwrap();
        installer.runtime_ready = true;
        let status = installer.request("rtx_hdr_install");
        assert_eq!(status["phase"], "failed");
        assert_eq!(status["error"], "rtx_hdr_package_unavailable");
        assert_eq!(status["runtime_ready"], true);
        assert_eq!(status["package_ready"], false);
        assert!(status["provider_active"].is_null());
    }

    #[test]
    fn cancellation_waits_for_worker_retirement_before_restart() {
        let mut installer = Installer::new(
            PathBuf::from("/tmp/rtx-test-nonexistent"),
            Network::test_provider(),
        )
        .unwrap();
        let cancel = installer.cancelled.clone();
        let (release, wait) = std::sync::mpsc::sync_channel::<()>(1);
        installer.phase = "downloading";
        installer.job = Some(Job {
            verifying: false,
            thread: thread::spawn(move || {
                wait.recv().unwrap();
                assert!(cancel.load(Ordering::Acquire));
                Err(std::io::ErrorKind::Interrupted.into())
            }),
        });
        installer.request("rtx_hdr_cancel");
        assert_eq!(
            installer.request("rtx_hdr_install")["code"],
            "installation_in_progress"
        );
        release.send(()).unwrap();
        retire(&mut installer);
        assert_eq!(installer.request("rtx_hdr_status")["phase"], "cancelled");
    }
}

#[cfg(all(test, target_os = "linux"))]
mod stalled_tests;
