use crate::settings::Document;
use serde_json::Value;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveSettings {
    pub file_state: String,
    pub vibeshine_file_state: String,
    pub credentials_file: String,
    pub pkey: String,
    pub cert: String,
    pub log_path: String,
    pub min_log_level: crate::settings::MinLogLevel,
}

pub fn trusted_root_from_settings(settings: &Value) -> Result<PathBuf, String> {
    let root = settings
        .get("host_state_root")
        .and_then(Value::as_str)
        .ok_or_else(|| "host-injected host_state_root is unavailable".to_owned())?;
    Ok(PathBuf::from(root))
}

pub fn validate_trusted_root(root: &Path) -> Result<PathBuf, String> {
    if !root.is_absolute() {
        return Err("host_state_root must be absolute".into());
    }
    let metadata =
        fs::symlink_metadata(root).map_err(|_| "host_state_root is unavailable".to_owned())?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err("host_state_root must be a real directory".into());
    }
    let canonical = root
        .canonicalize()
        .map_err(|_| "host_state_root cannot be canonicalized".to_owned())?;
    if !same_path(&canonical, root) {
        return Err("host_state_root must already be canonical".into());
    }
    Ok(canonical)
}

pub fn effective_settings(
    document: &Document,
    trusted_root: &Path,
) -> Result<EffectiveSettings, String> {
    document.validate()?;
    let root = validate_trusted_root(trusted_root)?;
    let settings = document.settings();
    Ok(EffectiveSettings {
        file_state: resolve_path(&root, &settings.file_state)?,
        vibeshine_file_state: resolve_path(&root, &settings.vibeshine_file_state)?,
        credentials_file: resolve_path(
            &root,
            if settings.credentials_file.is_empty() {
                &settings.file_state
            } else {
                &settings.credentials_file
            },
        )?,
        pkey: resolve_path(&root, &settings.pkey)?,
        cert: resolve_path(&root, &settings.cert)?,
        log_path: resolve_path(&root, &settings.log_path)?,
        min_log_level: settings
            .min_log_level
            .for_product_channel(env!("CARGO_PKG_VERSION")),
    })
}

pub fn effective_as_value(effective: &EffectiveSettings) -> Value {
    serde_json::json!({
        "file_state": effective.file_state,
        "vibeshine_file_state": effective.vibeshine_file_state,
        "credentials_file": effective.credentials_file,
        "pkey": effective.pkey,
        "cert": effective.cert,
        "log_path": effective.log_path,
        "min_log_level": effective.min_log_level,
    })
}

fn resolve_path(root: &Path, configured: &str) -> Result<String, String> {
    let path = Path::new(configured);
    if path.is_absolute() {
        return Ok(path.to_string_lossy().into_owned());
    }
    check_relative_components(root, path)?;
    Ok(root.join(path).to_string_lossy().into_owned())
}

fn check_relative_components(root: &Path, relative: &Path) -> Result<(), String> {
    let components: Vec<_> = relative.components().collect();
    let mut current = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::Normal(name) => current.push(name),
            Component::CurDir => continue,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("relative path must remain beneath host_state_root".into());
            }
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if is_reparse_or_symlink(&metadata) => {
                return Err("relative path contains a symlink or reparse point".into());
            }
            Ok(metadata) if index + 1 < components.len() && !metadata.is_dir() => {
                return Err("relative path parent is not a directory".into());
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err("relative path component cannot be inspected".into()),
        }
    }
    Ok(())
}

fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        let normalize = |path: &Path| {
            let value = path.to_string_lossy();
            value
                .strip_prefix(r"\\?\")
                .unwrap_or(value.as_ref())
                .replace('/', "\\")
        };
        normalize(left).eq_ignore_ascii_case(&normalize(right))
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Document;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("amaterasu-host-config-{nonce}"))
    }

    #[test]
    fn relative_paths_resolve_under_root_and_credentials_inherit_file_state() {
        let root = temporary_root();
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let effective = effective_settings(&Document::default(), &root).unwrap();
        assert_eq!(
            effective.file_state,
            root.join("sunshine_state.json")
                .to_string_lossy()
                .into_owned()
        );
        assert_eq!(effective.credentials_file, effective.file_state);
        assert_eq!(
            effective.log_path,
            root.join("amaterasu.log").to_string_lossy().into_owned()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_absolute_paths_remain_absolute() {
        let root = temporary_root();
        fs::create_dir_all(&root).unwrap();
        let absolute = root.join("elsewhere").to_string_lossy().into_owned();
        let mut document = Document::default();
        document.log_path = absolute.clone();
        let effective = effective_settings(&document, &root.canonicalize().unwrap()).unwrap();
        assert_eq!(effective.log_path, absolute);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn trusted_root_must_be_existing_and_canonical() {
        let root = temporary_root();
        assert!(validate_trusted_root(&root).is_err());
        fs::create_dir_all(&root).unwrap();
        let canonical = root.canonicalize().unwrap();
        assert_eq!(validate_trusted_root(&canonical).unwrap(), canonical);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn existing_relative_symlink_targets_are_rejected() {
        use std::os::unix::fs::symlink;

        let root = temporary_root();
        fs::create_dir_all(&root).unwrap();
        let outside = temporary_root();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("redirect")).unwrap();
        let mut document = Document::default();
        document.log_path = "redirect/service.log".into();
        assert!(effective_settings(&document, &root.canonicalize().unwrap()).is_err());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
