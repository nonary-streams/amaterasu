//! Pack the official provider and its optional Linux display integration.
use crate::profiles::{
    RUNTIME_ID,
    linux_session::{DISPLAY_SETTINGS, FOREGROUND_CONTEXT, RTX_HDR_COMPONENT},
};
use amaterasu_package::{
    ComponentManifest, PackageResult,
    plugins::{PluginManifest, rtx_hdr::runtime_supports_addon},
    sha256_reader,
};
use ed25519_dalek::SigningKey;
use std::{fs::File, path::Path};

pub(crate) fn pack(
    artifacts: &Path,
    output: &Path,
    version: &str,
    core: &str,
    sequence: u64,
    expires_unix: u64,
    signer: &SigningKey,
) -> PackageResult<()> {
    if !runtime_supports_addon(core) {
        return Err("RTX HDR add-on requires a compatible Linux x86-64 Core runtime".into());
    }
    let components = [DISPLAY_SETTINGS, FOREGROUND_CONTEXT, RTX_HDR_COMPONENT]
        .into_iter()
        .map(|definition| {
            let file = format!("lib{}.so", definition.library);
            let artifact =
                File::open(artifacts.join(&file)).map_err(|error| format!("{file}: {error}"))?;
            let size = artifact
                .metadata()
                .map_err(|error| error.to_string())?
                .len();
            let sha256 = sha256_reader(artifact)?;
            let mut requires = definition
                .dependencies
                .iter()
                .map(|&id| {
                    (
                        id.to_owned(),
                        if id == RUNTIME_ID { core } else { version }.to_owned(),
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            let mut requested = definition.grants();
            if definition.id == RTX_HDR_COMPONENT.id {
                requires.insert(FOREGROUND_CONTEXT.id.into(), version.into());
                requested
                    .call
                    .insert(amaterasu_module_sdk::foreground_context::SERVICE.into());
            }
            Ok(ComponentManifest {
                id: definition.id.into(),
                version: version.into(),
                file,
                sha256,
                size,
                abi_major: 1,
                runtime: false,
                requires,
                requested,
            })
        })
        .collect::<PackageResult<Vec<_>>>()?;
    let manifest = PluginManifest {
        schema: 1,
        host_context_schema: 1,
        project: "amaterasu".into(),
        id: RTX_HDR_COMPONENT.id.into(),
        version: version.into(),
        sequence,
        expires_unix,
        target: amaterasu_package::current_target(),
        components,
        drivers: Vec::new(),
        api_methods: Vec::new(),
        settings: Default::default(),
    };
    crate::plugin_package::pack_manifest(&manifest, artifacts, output, signer)
}
