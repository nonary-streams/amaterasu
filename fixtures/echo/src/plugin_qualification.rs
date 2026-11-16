//! Opt-in native service qualification. Never included in product packages.
use amaterasu_module_sdk::{
    Command, ModuleResult, Response, RuntimeClient, drivers::INSTALLATION_SERVICE,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
pub struct Qualification {
    manifest: String,
    result: PathBuf,
    pending: Option<u64>,
    counter: u64,
    next: Instant,
    done: bool,
    echo: bool,
    retry_failed: bool,
}
impl Qualification {
    pub fn new(runtime: &RuntimeClient, settings: &Value) -> ModuleResult<Option<Self>> {
        let Some(manifest) = settings["fixture_plugin_manifest"].as_str() else {
            return Ok(None);
        };
        runtime.require_success(Command::HostPackageBarrierRegister {})?;
        Ok(Some(Self {
            manifest: manifest.into(),
            result: runtime
                .runtime_state_root()?
                .join("plugin-qualification-result.json"),
            pending: None,
            counter: 0,
            next: Instant::now(),
            done: false,
            echo: false,
            retry_failed: settings["fixture_retry_failed"].as_bool().unwrap_or(false),
        }))
    }
    fn call(&mut self, runtime: &RuntimeClient, method: &str, payload: Value) -> ModuleResult<()> {
        self.counter += 1;
        let response = runtime.send(Command::CallApi {
            service: INSTALLATION_SERVICE.into(),
            method: method.into(),
            interface_major: 1,
            interface_minor: 0,
            payload,
            timeout_ms: 30000,
            operation_id: format!("qualify-{}-{}", std::process::id(), self.counter),
            parent: None,
        })?;
        let Response::Accepted { id } = response else {
            return Err(format!("qualification admission failed: {response:?}"));
        };
        self.pending = Some(id);
        Ok(())
    }
    pub fn tick(&mut self, runtime: &RuntimeClient) -> ModuleResult<()> {
        if let Response::HostPackageActivationState { pending, .. } =
            runtime.send(Command::HostPackageBarrierPending {})?
        {
            if pending {
                runtime.require_success(Command::HostPackageBarrierReady { ready: true })?;
            }
        }
        if self.done || Instant::now() < self.next {
            return Ok(());
        }
        self.next = Instant::now() + Duration::from_millis(25);
        let Some(id) = self.pending else {
            return self.call(runtime, "status", json!({}));
        };
        let payload = match runtime.send(Command::TakeResult { request: id })? {
            Response::Pending => return Ok(()),
            Response::Result { payload } => payload,
            response => return Err(format!("qualification request failed: {response:?}")),
        };
        self.pending = None;
        if self.echo {
            if payload["echo"]["qualification"] != true {
                return Err("native plugin echo was not delivered".into());
            }
            amaterasu_package::write_json_atomically(
                &self.result,
                &json!({"phase":"ready","reply":payload}),
            )?;
            self.done = true;
            return Ok(());
        }
        if payload["ok"] != true {
            return Err(format!("installation refused: {payload}"));
        }
        if payload.get("supported").is_none() {
            return Ok(());
        }
        let operation = &payload["operation"];
        if operation.is_null() {
            return self.call(runtime,"prepare",json!({"repository":"plugins","manifest":self.manifest,"installation_revision":payload["installation_revision"]}));
        }
        match operation["phase"].as_str().unwrap_or("") {
        "awaiting_approval"=>self.call(runtime,"approve",json!({"operation":operation["id"],"revision":operation["revision"],"installation_revision":operation["installation_revision"],"approval":operation["approval"]})),
        "ready"=>{
            self.counter+=1;
            let Response::Accepted{id}=runtime.send(Command::Request{service:"example.echo/1".into(),payload:json!({"qualification":true}),timeout_ms:1000,operation_id:format!("echo-{}",self.counter),parent:None})? else{return Err("native echo unavailable".into());};
            self.pending=Some(id);self.echo=true;Ok(())
        },
        "failed" if self.retry_failed=>self.call(runtime,"retry",json!({"operation":operation["id"],"revision":operation["revision"],"installation_revision":operation["installation_revision"]})),
        "failed"|"cancelled"=>Err(format!("installation did not finish: {operation}")),
        _=>Ok(()),
    }
    }
}
