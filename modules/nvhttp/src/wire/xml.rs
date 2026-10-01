use super::{Reply, RouterConfig};

pub fn escape(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            character if character.is_control() || matches!(character, '\u{fffe}' | '\u{ffff}') => {
                output.push(' ')
            }
            character => output.push(character),
        }
    }
    output
}

pub fn response(status: u16, message: &str, fields: &[(&str, &str)]) -> Reply {
    let mut body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><root status_code=\"{status}\" status_message=\"{}\">",
        escape(message)
    );
    for (name, value) in fields {
        // Names are static literals chosen by this module, never client input.
        body.push_str(&format!("<{name}>{}</{name}>", escape(value)));
    }
    body.push_str("</root>");
    Reply {
        status,
        content_type: "application/xml; charset=utf-8",
        body: body.into_bytes(),
        requires_paired_tls: false,
    }
}

pub fn error(status: u16, message: &str) -> Reply {
    response(status, message, &[("paired", "0")])
}

pub fn paired() -> Reply {
    response(200, "OK", &[("paired", "1")])
}

/// `SCM_H264` in moonlight-common-c `Limelight.h`. moonlight-qt forwards the
/// advertised mask unchanged and `LiStartConnection` aborts when it is zero, so
/// the baseline H.264 bit is advertised unconditionally. Software H.264 is the
/// guaranteed encoder; HEVC, 10-bit, 4:4:4 and AV1 are never claimed here.
const SERVER_CODEC_MODE_SUPPORT_H264: u32 = 0x0001;

pub fn server_info(
    config: &RouterConfig,
    host_id: &str,
    paired: bool,
    current_game: u32,
    busy: bool,
    streaming_supported: bool,
) -> Reply {
    response(
        200,
        "OK",
        &[
            ("hostname", &config.hostname),
            (
                "appversion",
                if streaming_supported {
                    "7.1.431.0"
                } else {
                    "7.1.0.0"
                },
            ),
            ("GfeVersion", "0.0.0.0"),
            ("uniqueid", host_id),
            ("HttpsPort", &config.https_port.to_string()),
            ("ExternalPort", &config.http_port.to_string()),
            ("mac", "00:00:00:00:00:00"),
            ("PairStatus", if paired { "1" } else { "0" }),
            ("currentgame", &current_game.to_string()),
            (
                "state",
                if busy {
                    "AMATERASU_SERVER_BUSY"
                } else {
                    "AMATERASU_SERVER_FREE"
                },
            ),
            (
                "ServerCodecModeSupport",
                &SERVER_CODEC_MODE_SUPPORT_H264.to_string(),
            ),
            ("MaxLumaPixelsHEVC", "0"),
            (
                "StreamingSupported",
                if streaming_supported { "1" } else { "0" },
            ),
        ],
    )
}
