use amaterasu_media::{CaptureGeometry, DesktopRect};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

pub const OUTPUT: &str = "nv12-fixture-display";
pub const WIDTH: u32 = 64;
pub const HEIGHT: u32 = 48;
pub const FPS: u64 = 30;
pub const FRAME_COUNT: u64 = 12;
pub const MAX_STREAMS: usize = 4;
pub const MAX_OPENS: u64 = 128;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Status,
    Enumerate {
        offset: Option<usize>,
        limit: Option<usize>,
    },
    Open {
        output: String,
        operation_id: Option<String>,
        #[serde(default, deserialize_with = "deserialize_clock")]
        resource_capture_clock: Option<ResourceCaptureClock>,
        #[serde(default, deserialize_with = "deserialize_callback_contract")]
        resource_callback_contract: Option<String>,
    },
    Lookup {
        operation_id: String,
    },
    Close {
        stream: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourceCaptureClock {
    pub domain: String,
    pub epoch_id: String,
}

fn deserialize_clock<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ResourceCaptureClock>, D::Error> {
    ResourceCaptureClock::deserialize(deserializer).map(Some)
}

fn deserialize_callback_contract<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

impl ResourceCaptureClock {
    pub fn epoch(&self) -> FixtureResult<[u8; 16]> {
        let text = self.epoch_id.as_bytes();
        if self.domain != "host_monotonic"
            || text.len() != 32
            || !text
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
        {
            return Err("invalid_resource_capture_clock");
        }
        let mut epoch = [0; 16];
        for (index, pair) in text.as_chunks::<2>().0.iter().enumerate() {
            let hex = |value: u8| {
                if value <= b'9' {
                    value - b'0'
                } else {
                    value - b'a' + 10
                }
            };
            epoch[index] = hex(pair[0]) * 16 + hex(pair[1]);
        }
        if epoch == [0; 16] {
            return Err("invalid_resource_capture_clock");
        }
        Ok(epoch)
    }
}

pub type FixtureResult<T> = Result<T, &'static str>;

pub fn error_payload(code: &str) -> Value {
    json!({"ok": false, "error": {"code": code, "message": code}})
}

pub fn geometry() -> CaptureGeometry {
    let rectangle = DesktopRect {
        left: 0,
        top: 0,
        width: WIDTH,
        height: HEIGHT,
    };
    CaptureGeometry {
        output: rectangle,
        virtual_desktop: rectangle,
    }
}

pub fn frame_bytes(sequence: u64) -> Vec<u8> {
    let luma_length = WIDTH as usize * HEIGHT as usize;
    let mut bytes = vec![128; luma_length * 3 / 2];
    for row in bytes[..luma_length].as_chunks_mut::<{ WIDTH as usize }>().0 {
        for (column, pixel) in row.iter_mut().enumerate() {
            let bright = (column < WIDTH as usize / 2) == sequence.is_multiple_of(2);
            *pixel = if bright { 220 } else { 32 };
        }
    }
    bytes
}

pub fn validate_operation_id(operation: &str) -> FixtureResult<()> {
    if operation.is_empty() || operation.len() > 128 || operation.chars().any(char::is_control) {
        return Err("invalid_operation_id");
    }
    Ok(())
}
