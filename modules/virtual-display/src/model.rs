use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub dd_activate_virtual_display: bool,
    pub dd_use_sunshine_virtual_display_driver: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            dd_activate_virtual_display: false,
            dd_use_sunshine_virtual_display_driver: true,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Create {
        identity: String,
        width: u32,
        height: u32,
        refresh_millihz: u32,
        name: String,
    },
    Status {
        identity: Option<String>,
    },
    Remove {
        identity: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Specification {
    pub identity: String,
    pub width: u32,
    pub height: u32,
    pub refresh_millihz: u32,
    pub name: String,
}

impl Specification {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_identity(&self.identity)
            || !(320..=7680).contains(&self.width)
            || !(200..=4320).contains(&self.height)
            || !(1000..=1_000_000).contains(&self.refresh_millihz)
            || self.name.is_empty()
            || self.name.len() > 31
            || !self
                .name
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err("invalid_request");
        }
        Ok(())
    }
}

pub fn valid_identity(identity: &str) -> bool {
    identity.len() == 36
        && identity.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
        && identity.bytes().any(|byte| byte != b'0' && byte != b'-')
}

pub fn display_id(caller: &str, identity: &str) -> u64 {
    let mut digest = Sha256::new();
    digest.update(b"amaterasu.virtual-display/1\0");
    digest.update((caller.len() as u64).to_le_bytes());
    digest.update(caller.as_bytes());
    digest.update(identity.as_bytes());
    let bytes = digest.finalize();
    u64::from_le_bytes(bytes[..8].try_into().expect("digest has eight bytes")) | (1 << 63)
}

pub fn failure(code: &str) -> Value {
    json!({"ok":false,"code":code})
}
