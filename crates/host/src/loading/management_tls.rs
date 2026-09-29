//! Host boundary for separately provisioned browser TLS files.
use amaterasu_contracts::RuntimeConfig;
use amaterasu_package::PackageResult;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const MANAGEMENT_COMPONENT: &str = "amaterasu.management";
const STATE_ROOT_TOKEN: &str = "__AMATERASU_WORKER_STATE_ROOT__/management-tls/";

pub(super) fn validate(config: &mut RuntimeConfig, state_root: &Path) -> PackageResult<()> {
    let Some(settings) = config.settings.get_mut(MANAGEMENT_COMPONENT) else {
        return Ok(());
    };
    let settings = settings
        .as_object_mut()
        .ok_or("management settings must be an object")?;
    let fields = ["tls_bind", "tls_certificate_path", "tls_private_key_path"];
    let present: Vec<_> = fields
        .iter()
        .map(|field| settings.contains_key(*field))
        .collect();
    if !present.iter().any(|present| *present) {
        return Ok(());
    }
    if !present.iter().all(|present| *present) {
        return Err("management TLS settings must be configured together".into());
    }
    validate_bind(&settings["tls_bind"])?;
    let certificate = setting_path(settings, "tls_certificate_path", state_root, "cert")?;
    let key = setting_path(settings, "tls_private_key_path", state_root, "key")?;
    if same_path(&certificate, &key) {
        return Err("management TLS certificate and key must be distinct".into());
    }
    validate_file(&certificate, state_root)?;
    validate_file(&key, state_root)?;
    verify_file_digest(&certificate)?;
    verify_file_digest(&key)?;
    settings.insert(
        "tls_certificate_path".into(),
        serde_json::json!(super::path_to_unicode_string(&certificate)?),
    );
    settings.insert(
        "tls_private_key_path".into(),
        serde_json::json!(super::path_to_unicode_string(&key)?),
    );
    Ok(())
}

fn validate_bind(value: &serde_json::Value) -> PackageResult<()> {
    let bind = value
        .as_str()
        .ok_or("management TLS bind must be a socket address")?
        .parse::<std::net::SocketAddr>()
        .map_err(|_| "management TLS bind must be a socket address")?;
    if bind.port() == 0 {
        return Err("management TLS bind must use a nonzero port".into());
    }
    Ok(())
}

fn setting_path(
    settings: &serde_json::Map<String, serde_json::Value>,
    name: &str,
    state_root: &Path,
    role: &str,
) -> PackageResult<PathBuf> {
    let value = settings[name]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or("management TLS path must be a nonempty string")?;
    let file_name = value
        .strip_prefix(STATE_ROOT_TOKEN)
        .ok_or("management TLS path must use the worker state root token")?;
    validate_file_name(file_name, role)?;
    let path = state_root.join("management-tls").join(file_name);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("management TLS path must be absolute and normalized".into());
    }
    Ok(path)
}

fn validate_file(path: &Path, state_root: &Path) -> PackageResult<()> {
    let enclave = state_root.join("management-tls");
    let relative = path
        .strip_prefix(&enclave)
        .map_err(|_| "management TLS path is outside its dedicated state directory")?;
    if relative.components().count() < 1 {
        return Err("management TLS path must name a file".into());
    }
    validate_directory(state_root)?;
    validate_directory(&enclave)?;
    let mut parent = enclave;
    if let Some(segments) = relative.parent() {
        for component in segments.components() {
            parent.push(component);
            validate_directory(&parent)?;
        }
    }
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| "management TLS file unavailable")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("management TLS path is not a regular file".into());
    }
    validate_platform_path(path, &metadata)
}

pub(crate) fn validate_file_name(name: &str, role: &str) -> PackageResult<()> {
    let digest = name
        .strip_prefix(&format!("management-{role}-"))
        .and_then(|name| name.strip_suffix(".pem"))
        .ok_or("management TLS filename has an invalid role")?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("management TLS filename has an invalid digest".into());
    }
    Ok(())
}

#[cfg(any(windows, test))]
pub(crate) fn active_file_names(config: &RuntimeConfig) -> PackageResult<Option<[String; 2]>> {
    let Some(settings) = config.settings.get(MANAGEMENT_COMPONENT) else {
        return Ok(None);
    };
    let settings = settings
        .as_object()
        .ok_or("management settings must be an object")?;
    let keys = ["tls_bind", "tls_certificate_path", "tls_private_key_path"];
    let count = keys
        .iter()
        .filter(|key| settings.contains_key(**key))
        .count();
    if count == 0 {
        return Ok(None);
    }
    if count != 3 {
        return Err("management TLS settings must be configured together".into());
    }
    let mut names = [String::new(), String::new()];
    for (index, (key, role)) in [
        ("tls_certificate_path", "cert"),
        ("tls_private_key_path", "key"),
    ]
    .iter()
    .enumerate()
    {
        let value = settings[*key]
            .as_str()
            .ok_or("management TLS path must be a string")?;
        let name = value
            .strip_prefix(STATE_ROOT_TOKEN)
            .ok_or("management TLS path must use the worker state root token")?;
        validate_file_name(name, role)?;
        names[index] = name.to_owned();
    }
    Ok(Some(names))
}

pub(crate) fn verify_file_digest(path: &Path) -> PackageResult<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("management TLS filename is invalid")?;
    let digest = name
        .strip_suffix(".pem")
        .and_then(|stem| stem.rsplit_once('-'))
        .map(|(_, digest)| digest)
        .ok_or("management TLS filename is invalid")?;
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() == 0 || metadata.len() > 64 * 1024 {
        return Err("management TLS file size is invalid".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.is_empty() || bytes.len() > 64 * 1024 {
        return Err("management TLS file size is invalid".into());
    }
    if amaterasu_package::sha256(&bytes) != digest {
        return Err("management TLS file digest does not match signed path".into());
    }
    Ok(())
}

fn validate_directory(path: &Path) -> PackageResult<()> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| "management TLS directory unavailable")?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("management TLS directory is not real".into());
    }
    validate_platform_path(path, &metadata)
}

#[cfg(windows)]
fn validate_platform_path(path: &Path, metadata: &std::fs::Metadata) -> PackageResult<()> {
    use std::os::windows::fs::MetadataExt;
    if metadata.file_attributes() & 0x400 != 0 {
        return Err("management TLS path contains a reparse point".into());
    }
    crate::state_root::validate_management_tls_acl(path)
}

#[cfg(unix)]
fn validate_platform_path(_path: &Path, metadata: &std::fs::Metadata) -> PackageResult<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid has no arguments or memory ownership requirements.
    let user = unsafe { geteuid() };
    if metadata.uid() != user || metadata.permissions().mode() & 0o077 != 0 {
        return Err("management TLS path is not owned and private".into());
    }
    Ok(())
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn trusted_tls_bind_accepts_explicit_remote_and_wildcard_selection_without_opening_sockets() {
        for bind in [
            "127.0.0.1:47990",
            "[::1]:47990",
            "0.0.0.0:47990",
            "[::]:47990",
            "192.168.0.1:47990",
            "203.0.113.1:47990",
        ] {
            validate_bind(&json!(bind)).unwrap();
        }
        for bind in [
            json!("127.0.0.1:0"),
            json!("0.0.0.0:0"),
            json!("localhost:47990"),
            json!("relative"),
            json!(true),
            serde_json::Value::Null,
        ] {
            assert!(validate_bind(&bind).is_err());
        }
    }

    fn configured(cert: &str, key: &str) -> RuntimeConfig {
        let mut config = RuntimeConfig::default();
        config.settings.insert(
            MANAGEMENT_COMPONENT.into(),
            json!({
                "tls_bind": "127.0.0.1:47990",
                "tls_certificate_path": format!("{STATE_ROOT_TOKEN}{cert}"),
                "tls_private_key_path": format!("{STATE_ROOT_TOKEN}{key}"),
            }),
        );
        config
    }

    #[test]
    fn active_pair_requires_exact_token_roles_and_complete_triple() {
        let digest = "a".repeat(64);
        let cert = format!("management-cert-{digest}.pem");
        let key = format!("management-key-{digest}.pem");
        let mut config = configured(&cert, &key);
        assert_eq!(active_file_names(&config).unwrap(), Some([cert, key]));
        config.settings.get_mut(MANAGEMENT_COMPONENT).unwrap()["tls_private_key_path"] =
            json!(r"__AMATERASU_WORKER_STATE_ROOT__\management-tls\management-key-a.pem");
        assert!(active_file_names(&config).is_err());
        config
            .settings
            .get_mut(MANAGEMENT_COMPONENT)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("tls_bind");
        assert!(active_file_names(&config).is_err());
    }

    #[test]
    fn tls_bind_port_zero_fails_before_file_access() {
        let digest = "b".repeat(64);
        let mut config = configured(
            &format!("management-cert-{digest}.pem"),
            &format!("management-key-{digest}.pem"),
        );
        config.settings.get_mut(MANAGEMENT_COMPONENT).unwrap()["tls_bind"] = json!("127.0.0.1:0");
        assert!(validate(&mut config, Path::new(r"C:\state")).is_err());
    }

    #[test]
    fn content_addressed_file_rejects_changed_bytes() {
        let bytes = b"browser TLS test material";
        let digest = amaterasu_package::sha256(bytes);
        let directory =
            std::env::temp_dir().join(format!("amaterasu-tls-digest-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("management-cert-{digest}.pem"));
        std::fs::write(&path, bytes).unwrap();
        verify_file_digest(&path).unwrap();
        std::fs::write(&path, b"changed").unwrap();
        assert!(verify_file_digest(&path).is_err());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
