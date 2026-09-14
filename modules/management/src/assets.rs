//! Verified static assets and version-pinned browser loads, independent of native activation.
use amaterasu_package::{PackageResult, unix_timestamp, web};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSettings {
    directory: PathBuf,
    trusted_keys: BTreeMap<String, String>,
}

pub struct Assets {
    settings: WebSettings,
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
        Ok(Self {
            settings,
            cache: BTreeMap::new(),
            current: None,
            highest_sequence: 0,
        })
    }

    fn refresh(&mut self, now: u64) -> PackageResult<String> {
        let selection = web::read_selection(&self.settings.directory)?;
        if selection.highest_sequence < self.highest_sequence {
            return Err("web selection regressed".into());
        }
        if self.current.as_deref() != Some(&selection.digest) {
            if self.current.is_some() && selection.highest_sequence == self.highest_sequence {
                return Err("web selection changed without advancing sequence".into());
            }
            let verified = web::load_version(
                &self.settings.directory,
                &selection.digest,
                &self.settings.trusted_keys,
                now,
            )?;
            if verified.manifest.sequence != selection.highest_sequence {
                return Err("web selection sequence mismatch".into());
            }
            self.highest_sequence = verified.manifest.sequence;
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
        self.verified_response(path)
            .unwrap_or_else(|_| failure(503, "Verified web bundle unavailable"))
    }

    fn verified_response(&mut self, path: &str) -> PackageResult<Vec<u8>> {
        let now = unix_timestamp();
        let current = self.refresh(now)?;
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
            let bundle = web::load_version(
                &self.settings.directory,
                digest,
                &self.settings.trusted_keys,
                now,
            )?;
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

fn asset_response(status: u16, content_type: &str, body: &[u8]) -> Vec<u8> {
    let reason = if status == 200 {
        "OK"
    } else if status == 404 {
        "Not Found"
    } else {
        "Service Unavailable"
    };
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'self'\r\nReferrer-Policy: no-referrer\r\nCross-Origin-Resource-Policy: same-origin\r\n\r\n", body.len()
    ).into_bytes();
    response.extend_from_slice(body);
    response
}
