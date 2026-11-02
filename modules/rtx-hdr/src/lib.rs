//! NVIDIA TrueHDR as a host-invoked post-processing plugin.
#[cfg(all(target_os = "linux", feature = "linux-nvidia"))]
mod linux;
mod settings;
pub use settings::Settings;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};

pub const SERVICE: &str = "amaterasu.rtx-hdr/1";

#[cfg(all(target_os = "linux", feature = "linux-nvidia"))]
type CreateProcessor =
    Box<dyn Fn(&str) -> Result<linux::Processor, amaterasu_module_sdk::abi::Status> + Send + Sync>;

pub struct RtxHdr {
    #[cfg(all(target_os = "linux", feature = "linux-nvidia"))]
    runtime: RuntimeClient,
    #[cfg(all(target_os = "linux", feature = "linux-nvidia"))]
    logs: amaterasu_module_sdk::LogQueue,
    #[cfg(all(target_os = "linux", feature = "linux-nvidia"))]
    _factory: Box<amaterasu_module_sdk::processing::GpuProcessorFactory<CreateProcessor>>,
}

impl ManagedModule for RtxHdr {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("RTX HDR settings unavailable".into());
        };
        let settings: Settings = serde_json::from_value(payload)
            .map_err(|error| format!("invalid RTX HDR settings: {error}"))?;
        settings.validate()?;
        #[cfg(all(target_os = "linux", feature = "linux-nvidia"))]
        {
            let settings = Settings {
                runtime_directory: std::fs::canonicalize(&settings.runtime_directory)
                    .map_err(|error| format!("RTX HDR runtime directory unavailable: {error}"))?,
                ..settings
            };
            let (logger, logs) = runtime.log_emitter();
            let create: CreateProcessor = Box::new(move |_principal| {
                Ok(linux::Processor::new(settings.clone()).with_logger(logger.clone()))
            });
            let factory = amaterasu_module_sdk::processing::GpuProcessorFactory::new(create);
            runtime.require_success(Command::Register {
                service: SERVICE.into(),
            })?;
            // SAFETY: the boxed SDK factory lives through this module. Signed
            // composition must stop video/encoder consumers before this plugin.
            unsafe { factory.register(&runtime, SERVICE) }?;
            Ok(Self {
                _factory: factory,
                runtime,
                logs,
            })
        }
        #[cfg(not(all(target_os = "linux", feature = "linux-nvidia")))]
        Err("RTX HDR requires a build with the linux-nvidia backend".into())
    }

    fn tick(&mut self) -> ModuleResult<()> {
        #[cfg(all(target_os = "linux", feature = "linux-nvidia"))]
        self.logs.flush(&self.runtime);
        Ok(())
    }
}

amaterasu_module_sdk::export_module!(RtxHdr);
