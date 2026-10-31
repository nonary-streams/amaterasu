//! Validated artwork references and bounded provider chunks.
use super::error;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};

const CHUNK: u64 = 8192;
const MAX_ARTWORK: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Artwork {
    Steam { app_id: u32, revision: String },
    Lutris { game_id: i64, revision: String },
    Playnite { game_id: String, revision: String },
}
impl Artwork {
    pub(super) fn revision(&self) -> &str {
        match self {
            Self::Steam { revision, .. }
            | Self::Lutris { revision, .. }
            | Self::Playnite { revision, .. } => revision,
        }
    }
    pub(super) fn request(
        &self,
        expected: Option<&str>,
        offset: u64,
    ) -> Option<(&'static str, Value)> {
        if expected.is_some_and(|value| value != self.revision()) {
            return None;
        }
        match self {
            Self::Steam { app_id, revision }
                if *app_id > 0
                    && revision
                        .parse::<u64>()
                        .is_ok_and(|n| n.to_string() == *revision) =>
            {
                Some((
                    "amaterasu.steam/1",
                    json!({"action":"artwork_read","app_id":app_id,"revision":revision,"offset":offset}),
                ))
            }
            Self::Lutris { game_id, revision } if *game_id > 0 && valid_digest(revision) => Some((
                "amaterasu.lutris/1",
                json!({"action":"artwork_read","game_id":game_id,"revision":revision,"offset":offset}),
            )),
            Self::Playnite { game_id, revision }
                if valid_guid(game_id) && valid_digest(revision) =>
            {
                Some((
                    "amaterasu.playnite/1",
                    json!({"action":"artwork_read","game_id":game_id,"revision":revision,"offset":offset}),
                ))
            }
            _ => None,
        }
    }
    fn matches_id(&self, payload: &Value) -> bool {
        match self {
            Self::Steam { app_id, .. } => payload["app_id"].as_u64() == Some(u64::from(*app_id)),
            Self::Lutris { game_id, .. } => payload["game_id"].as_i64() == Some(*game_id),
            Self::Playnite { game_id, .. } => payload["game_id"].as_str() == Some(game_id),
        }
    }
}
pub(super) fn valid_artwork_cursor(revision: Option<&str>, offset: u64) -> bool {
    if offset >= MAX_ARTWORK || !offset.is_multiple_of(CHUNK) {
        return false;
    }
    match revision {
        Some(value) => !value.is_empty() && value.len() <= 64,
        None => offset == 0,
    }
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}
fn decoded_length(data: &str) -> Option<usize> {
    if data.is_empty() || data.len() > 4 * (CHUNK as usize).div_ceil(3) {
        return None;
    }
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()
        .map(|bytes| bytes.len())
}
pub(super) fn artwork_chunk(
    payload: Value,
    artwork: &Artwork,
    revision: &str,
    offset: u64,
) -> Value {
    let valid = || -> Option<Value> {
        if payload["ok"] != true
            || !artwork.matches_id(&payload)
            || payload["revision"].as_str()? != revision
            || payload["offset"].as_u64()? != offset
        {
            return None;
        }
        let total = payload["total_bytes"].as_u64()?;
        if total == 0 || total > MAX_ARTWORK || offset >= total {
            return None;
        }
        let end = (offset + CHUNK).min(total);
        let next = payload.get("next_offset")?;
        if (end < total && next.as_u64() != Some(end)) || (end == total && !next.is_null()) {
            return None;
        }
        let mime = payload["mime_type"].as_str()?;
        if !matches!(mime, "image/png" | "image/jpeg" | "image/webp") {
            return None;
        }
        let data = payload["data"].as_str()?;
        if decoded_length(data)? as u64 != end - offset {
            return None;
        }
        Some(
            json!({"ok":true,"revision":revision,"offset":offset,"next_offset":next,"total_bytes":total,"mime_type":mime,"data":data}),
        )
    };
    valid().unwrap_or_else(|| {
        error(
            "invalid_artwork_response",
            "provider returned invalid artwork",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::decoded_length;

    #[test]
    fn rejects_noncanonical_base64_padding_bits() {
        assert_eq!(decoded_length("YQ=="), Some(1));
        assert_eq!(decoded_length("YWI="), Some(2));
        assert_eq!(decoded_length("YR=="), None);
        assert_eq!(decoded_length("YWJ="), None);
    }
}
