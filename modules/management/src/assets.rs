//! Verified static assets and version-pinned browser loads, independent of native activation.
use amaterasu_package::{PackageResult, repository::RepositoryStore, unix_timestamp, web};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

const RECOVERY_PAGE: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Amaterasu recovery</title></head><body><main><h1>Amaterasu UI recovery</h1><p>This management-owned page remains available if a selected UI bundle fails.</p><p>Restore the previous verified UI with <code>amaterasu-release repository-restore-ui ROOT</code>. If it is unavailable, specify the digest of another retained bundle signed by a still-trusted key: <code>amaterasu-release repository-restore-ui ROOT DIGEST</code>. Then refresh the console.</p></main></body></html>";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSettings {
    directory: PathBuf,
    trusted_keys: BTreeMap<String, String>,
}

impl WebSettings {
    /// What decides whether the selected console bundle still verifies.
    pub fn console_trust(&self) -> crate::repository_admin::ConsoleTrust {
        crate::repository_admin::ConsoleTrust {
            directory: self.directory.clone(),
            trusted_keys: self.trusted_keys.clone(),
        }
    }

    pub fn repository_directory(&self) -> PackageResult<PathBuf> {
        Ok(self
            .directory
            .parent()
            .ok_or("web directory has no parent")?
            .join("repositories"))
    }
}

pub struct Assets {
    settings: WebSettings,
    authorized_keys: BTreeMap<String, String>,
    cache: BTreeMap<String, web::VerifiedBundle>,
    current: Option<String>,
    highest_sequence: u64,
}

impl Assets {
    pub fn new(settings: WebSettings) -> PackageResult<Self> {
        if !settings.directory.is_absolute()
            || settings.trusted_keys.is_empty()
            || settings.trusted_keys.len() > 16
        {
            return Err("web assets require an absolute directory and 1..16 trusted keys".into());
        }
        for key in settings.trusted_keys.values() {
            amaterasu_package::decode_hex::<32>(key)?;
        }
        RepositoryStore::new(settings.repository_directory()?)?;
        Ok(Self {
            settings,
            authorized_keys: BTreeMap::new(),
            cache: BTreeMap::new(),
            current: None,
            highest_sequence: 0,
        })
    }

    fn refresh(
        &mut self,
        now: u64,
        active_keys: &BTreeMap<String, String>,
    ) -> PackageResult<String> {
        let mut keys = self.settings.trusted_keys.clone();
        keys.extend(active_keys.clone());
        if keys != self.authorized_keys {
            self.cache.clear();
            self.current = None;
            self.authorized_keys = keys;
        }
        let selection = web::read_selection(&self.settings.directory)?;
        if selection.highest_sequence < self.highest_sequence {
            return Err("web selection regressed".into());
        }
        if self.current.as_deref() != Some(&selection.digest) {
            if self.current.is_some()
                && selection.highest_sequence == self.highest_sequence
                && selection.previous_digest.as_deref() != self.current.as_deref()
            {
                return Err("web selection changed without advancing sequence".into());
            }
            let verified = web::load_version(
                &self.settings.directory,
                &selection.digest,
                &self.authorized_keys,
                now,
            )?;
            if verified.manifest.sequence
                != selection
                    .active_sequence
                    .unwrap_or(selection.highest_sequence)
            {
                return Err("web selection sequence mismatch".into());
            }
            self.highest_sequence = selection.highest_sequence;
            self.current = Some(verified.digest.clone());
            self.make_room();
            self.cache.insert(verified.digest.clone(), verified);
        }
        if selection.highest_sequence != self.highest_sequence {
            return Err("web selection sequence mismatch".into());
        }
        Ok(selection.digest)
    }

    fn make_room(&mut self) {
        while self.cache.len() >= 4 {
            let key = self
                .cache
                .keys()
                .find(|key| Some(key.as_str()) != self.current.as_deref())
                .cloned();
            if let Some(key) = key {
                self.cache.remove(&key);
            } else {
                break;
            }
        }
    }

    pub fn response(&mut self, path: &str) -> Vec<u8> {
        if path == "/recovery" {
            return asset_response(200, "text/html; charset=utf-8", RECOVERY_PAGE.as_bytes());
        }
        self.verified_response(path)
            .unwrap_or_else(|_| failure(503, "Verified web bundle unavailable"))
    }

    fn verified_response(&mut self, path: &str) -> PackageResult<Vec<u8>> {
        let repository_store = RepositoryStore::new(self.settings.repository_directory()?)?;
        repository_store
            .with_active_keys(|active_keys| self.verified_response_with_keys(path, active_keys))
    }

    fn verified_response_with_keys(
        &mut self,
        path: &str,
        active_keys: &BTreeMap<String, String>,
    ) -> PackageResult<Vec<u8>> {
        let now = unix_timestamp();
        let current = self.refresh(now, active_keys)?;
        let (digest, name) = if path == "/" || path == "/index.html" {
            (current.as_str(), "index.html")
        } else if let Some(versioned) = path.strip_prefix("/web/") {
            let (digest, name) = versioned.split_once('/').ok_or("missing asset path")?;
            if name == "index.html" {
                return Ok(failure(404, "Not found"));
            }
            (digest, name)
        } else {
            (
                current.as_str(),
                path.strip_prefix('/').ok_or("invalid asset path")?,
            )
        };
        if web::validate_asset_name(name).is_err() {
            return Ok(failure(404, "Not found"));
        }
        if !self.cache.contains_key(digest) {
            let bundle =
                web::load_version(&self.settings.directory, digest, &self.authorized_keys, now)?;
            self.make_room();
            self.cache.insert(digest.to_owned(), bundle);
        }
        let bundle = self.cache.get(digest).ok_or("bundle cache missing")?;
        if bundle.manifest.expires_unix <= now {
            return Err("web metadata expired".into());
        }
        let Some(bytes) = bundle.assets.get(name) else {
            return Ok(failure(404, "Not found"));
        };
        let content_type = web::content_type(name).ok_or("asset content type missing")?;
        if name == "index.html" {
            let html = std::str::from_utf8(bytes).map_err(|_| "index is not UTF-8")?;
            if !html.contains("<head>") || html.contains("<base") {
                return Err("index requires a plain head and no existing base".into());
            }
            let html = html.replacen(
                "<head>",
                &format!("<head><base href=\"/web/{digest}/\">"),
                1,
            );
            Ok(asset_response(200, content_type, html.as_bytes()))
        } else {
            Ok(asset_response(200, content_type, bytes))
        }
    }
}

pub fn failure(status: u16, message: &str) -> Vec<u8> {
    asset_response(status, "text/plain; charset=utf-8", message.as_bytes())
}

pub(crate) fn asset_response(status: u16, content_type: &str, body: &[u8]) -> Vec<u8> {
    let reason = if status == 200 {
        "OK"
    } else if status == 404 {
        "Not Found"
    } else {
        "Service Unavailable"
    };
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'self'; img-src 'self' blob:; script-src 'self'; style-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'self'\r\nReferrer-Policy: no-referrer\r\nCross-Origin-Resource-Policy: same-origin\r\n\r\n", body.len()
    ).into_bytes();
    response.extend_from_slice(body);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_package::repository::ApprovedRepository;
    use amaterasu_package::{
        ReleaseSignature, encode_hex, random_identifier, sha256, write_json_atomically,
    };
    use ed25519_dalek::{Signer, SigningKey};
    use std::fs;

    #[test]
    fn artwork_blob_urls_are_allowed_only_for_images() {
        let response = String::from_utf8(asset_response(200, "text/html", b"fixture")).unwrap();
        let policy = response
            .lines()
            .find_map(|line| line.strip_prefix("Content-Security-Policy: "))
            .unwrap();
        assert!(policy.contains("img-src 'self' blob:"));
        assert!(policy.contains("script-src 'self'"));
        assert!(policy.contains("style-src 'self'"));
        assert!(policy.contains("object-src 'none'"));
        assert_eq!(policy.matches("blob:").count(), 1);
    }

    #[test]
    fn approved_ui_is_served_until_revocation_while_recovery_remains_available() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-assets-publisher-{}",
            random_identifier().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let result = (|| -> PackageResult<()> {
            let signer = SigningKey::from_bytes(&[74; 32]);
            let public_key = encode_hex(signer.verifying_key().as_bytes());
            let fingerprint = sha256(signer.verifying_key().as_bytes());
            let approved = ApprovedRepository::approve(
                "fixture-publisher".into(),
                "https://updates.example.test".into(),
                public_key.clone(),
                fingerprint.clone(),
            )?;
            let store = RepositoryStore::new(root.join("repositories"))?;
            store.approve(approved)?;
            let source = root.join("source");
            fs::create_dir(&source).map_err(|error| error.to_string())?;
            fs::write(
                source.join("index.html"),
                "<head></head><body>approved UI</body>",
            )
            .map_err(|error| error.to_string())?;
            let bundle = root.join("bundle");
            let manifest = web::prepare_bundle(&source, &bundle, 1, unix_timestamp() + 3600)?;
            let raw = serde_json::to_vec(&manifest).map_err(|error| error.to_string())?;
            fs::write(bundle.join("manifest.json"), &raw).map_err(|error| error.to_string())?;
            write_json_atomically(
                &bundle.join("manifest.sig"),
                &ReleaseSignature {
                    key_id: fingerprint.clone(),
                    signature: encode_hex(&signer.sign(&raw).to_bytes()),
                },
            )?;
            web::stage_repository_bundle(
                &bundle,
                &root.join("web"),
                &BTreeMap::from([(fingerprint, public_key)]),
                &sha256(signer.verifying_key().as_bytes()),
                unix_timestamp(),
            )?;
            let mut assets = Assets::new(WebSettings {
                directory: root.join("web"),
                trusted_keys: BTreeMap::from([("project".into(), "0".repeat(64))]),
            })?;
            assert!(assets.response("/").starts_with(b"HTTP/1.1 200"));
            store.revoke("fixture-publisher")?;
            assert!(assets.response("/").starts_with(b"HTTP/1.1 503"));
            assert!(assets.response("/recovery").starts_with(b"HTTP/1.1 200"));
            Ok(())
        })();
        fs::remove_dir_all(root).unwrap();
        result.unwrap();
    }
}
