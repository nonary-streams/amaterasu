use amaterasu_media::{CaptureGeometry, DesktopRect};
use serde::Deserialize;
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
    },
    Lookup {
        operation_id: String,
    },
    Close {
        stream: u64,
    },
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
