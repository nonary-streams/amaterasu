//! Signed helper process boundary. No shell commands or caller-supplied arguments.
use amaterasu_module_sdk::drivers::{Action, DriverState, HelperReply, HelperRequest};
use amaterasu_package::{
    PackageResult,
    plugins::{
        installed,
        operation::{DriverBackend, Operation},
    },
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus},
    time::{Duration, Instant},
};
pub fn runtime_identity() -> PackageResult<String> {
    #[cfg(unix)]
    {
        Ok(unsafe { libc::geteuid() }.to_string())
    }
    #[cfg(windows)]
    {
        amaterasu_input_companion::installation_identity().map_err(|e| format!("{e:?}"))
    }
}
pub struct Backend {
    pub root: PathBuf,
    pub broker: PathBuf,
    pub repositories: PathBuf,
    pub cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl DriverBackend for Backend {
    fn request(
        &mut self,
        operation: &Operation,
        request: &HelperRequest,
    ) -> PackageResult<HelperReply> {
        if matches!(request.action, Action::Install | Action::Reconcile) {
            return serde_json::from_value(super::broker::client(
                &self.broker,
                &super::broker::Request::Execute {
                    operation: operation.id.clone(),
                    request: request.clone(),
                },
            )?)
            .map_err(|e| e.to_string());
        }
        let bootstrap = amaterasu_package::read_json(&self.root.join("bootstrap.json"))?;
        let store = amaterasu_package::repository::RepositoryStore::new(self.repositories.clone())?;
        store.with_active_keys(|_| {
            let plugins =
                installed::verify_sources_from_store(&store, &operation.sources, &bootstrap)?;
            let plugin = plugins
                .iter()
                .find(|plugin| plugin.manifest.drivers.contains(&request.requirement))
                .ok_or("driver_requirement_not_signed")?;
            run_cancellable(&plugin.directory, request, &self.cancelled)
        })
    }
}
pub fn run_protected(
    source: &Path,
    request: &HelperRequest,
    parent: &Path,
) -> PackageResult<HelperReply> {
    super::security::machine_private(parent)?;
    run_in(
        source,
        request,
        &std::sync::atomic::AtomicBool::new(false),
        parent,
    )
}
pub fn run_cancellable(
    source: &Path,
    request: &HelperRequest,
    cancelled: &std::sync::atomic::AtomicBool,
) -> PackageResult<HelperReply> {
    run_in(source, request, cancelled, &std::env::temp_dir())
}
fn run_in(
    source: &Path,
    request: &HelperRequest,
    cancelled: &std::sync::atomic::AtomicBool,
    parent: &Path,
) -> PackageResult<HelperReply> {
    let stage = parent.join(format!(
        "amaterasu-driver-{}",
        amaterasu_package::random_identifier()?
    ));
    amaterasu_package::create_machine_private_directory(&stage)?;
    let io = stage.join(".io");
    amaterasu_package::create_machine_private_directory(&io)?;
    let result = (|| {
        for artifact in
            std::iter::once(&request.requirement.installer).chain(&request.requirement.payloads)
        {
            amaterasu_package::plugins::verify_artifact(source, artifact)?;
            fs::copy(source.join(&artifact.file), stage.join(&artifact.file))
                .map_err(|e| e.to_string())?;
            amaterasu_package::plugins::verify_artifact(&stage, artifact)?;
        }
        let exe = stage.join(&request.requirement.installer.file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exe, fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let input = io.join("request.json");
        fs::write(
            &input,
            serde_json::to_vec(request).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let output = io.join("reply.json");
        let errors = io.join("errors.log");
        let mut command = Command::new(exe);
        command
            .arg("--amaterasu-driver-helper-v1")
            .current_dir(&stage)
            .stdin(fs::File::open(input).map_err(|e| e.to_string())?)
            .stdout(fs::File::create(&output).map_err(|e| e.to_string())?)
            .stderr(fs::File::create(&errors).map_err(|e| e.to_string())?);
        command.env_clear();
        #[cfg(windows)]
        for name in ["SystemRoot", "ProgramData"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x00000004);
        }
        #[cfg(windows)]
        {
            command.env("TEMP", &io).env("TMP", &io);
        }
        let mut child = HelperProcess::start(command)?;
        let deadline = Instant::now() + Duration::from_secs(600);
        let status = loop {
            if cancelled.load(std::sync::atomic::Ordering::Acquire)
                && matches!(request.action, Action::Detect | Action::Verify)
            {
                return Err("plugin_preparation_cancelled".into());
            }
            if fs::metadata(&output).map_err(|e| e.to_string())?.len() > 65536
                || fs::metadata(&errors).map_err(|e| e.to_string())?.len() > 65536
                || Instant::now() >= deadline
            {
                break Err("driver_helper_limit_exceeded".to_string());
            }
            if child.exited()? {
                break child.finish();
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        if !status?.success() {
            return Err("driver_helper_failed".into());
        }
        let bytes = amaterasu_package::read_bounded(&output, 65536)?;
        let reply: HelperReply = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        reply.validate_for(request)?;
        Ok(reply)
    })();
    let _ = fs::remove_dir_all(&stage);
    result
}

/// Retain the child identity until its entire contained process tree is retired.
/// Every error path (including failed output reads) runs this cleanup before
/// protected staging is removed.
struct HelperProcess {
    child: Child,
    retired: bool,
    #[cfg(windows)]
    job: Option<super::adapters::windows_job::Job>,
}
impl HelperProcess {
    fn start(mut command: Command) -> PackageResult<Self> {
        let child = command.spawn().map_err(|e| e.to_string())?;
        #[allow(unused_mut)] // Windows assigns its suspended child to a job before resuming.
        let mut process = Self {
            child,
            retired: false,
            #[cfg(windows)]
            job: None,
        };
        #[cfg(windows)]
        {
            process.job = Some(super::adapters::windows_job::contain_and_resume(
                &mut process.child,
            )?);
        }
        Ok(process)
    }
    fn exited(&mut self) -> PackageResult<bool> {
        #[cfg(target_os = "linux")]
        {
            let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: the child belongs to this guard. WNOWAIT deliberately
            // keeps its PID reserved until we terminate its process group.
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child.id(),
                    &mut information,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result != 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            Ok(unsafe { information.si_pid() } != 0)
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.child
                .try_wait()
                .map(|status| status.is_some())
                .map_err(|e| e.to_string())
        }
    }
    fn terminate_tree(&mut self) {
        #[cfg(target_os = "linux")]
        // SAFETY: this unreaped child still reserves its process group identity.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        #[cfg(windows)]
        {
            self.job.take();
        }
    }
    fn finish(&mut self) -> PackageResult<ExitStatus> {
        self.terminate_tree();
        let status = self.child.wait().map_err(|e| e.to_string())?;
        self.retired = true;
        Ok(status)
    }
}
impl Drop for HelperProcess {
    fn drop(&mut self) {
        if !self.retired {
            self.terminate_tree();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
pub fn reply(request: &HelperRequest, state: DriverState, message: &str) -> HelperReply {
    HelperReply {
        schema: 1,
        operation: request.operation.clone(),
        driver: request.requirement.id.clone(),
        state,
        installed_version: None,
        message: message.into(),
        cancellable: !matches!(request.action, Action::Install | Action::Reconcile),
    }
}
pub fn main() -> PackageResult<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if let [command, feature, directory] = args.as_slice()
        && command == "--describe-requirement"
    {
        let requirement = super::adapters::packaging::requirement(
            feature,
            Path::new(directory),
            &std::env::current_exe().map_err(|e| e.to_string())?,
        )?;
        return std::io::stdout()
            .write_all(&serde_json::to_vec_pretty(&requirement).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string());
    }
    if args != ["--amaterasu-driver-helper-v1"] {
        return Err("usage: amaterasu-driver-helper --describe-requirement FEATURE PAYLOADS | --amaterasu-driver-helper-v1".into());
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 65536 {
        return Err("driver_request_too_large".into());
    }
    let request: HelperRequest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    amaterasu_package::plugins::validate_requirement(&request.requirement)?;
    let reply = super::adapters::execute(&request)?;
    std::io::stdout()
        .write_all(&serde_json::to_vec(&reply).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
