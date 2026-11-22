use super::query::{Query, hex_bytes};
use crate::launch::{AdvisoryWarnings, LaunchRequest};
use amaterasu_module_sdk::stream_warnings::{MAX_SAFE_INTEGER, StreamWarning, WarningValue};

const LAUNCH_FIELDS: &[&str] = &[
    "appid",
    "mode",
    "clientName",
    "rikey",
    "rikeyid",
    "uniqueid",
    "uuid",
    "additionalStates",
    "sops",
    "hdrMode",
    "clientHdrCapVersion",
    "clientHdrCapSupportedFlagsInUint32",
    "clientHdrCapMetaDataId",
    "clientHdrCapDisplayData",
    "localAudioPlayMode",
    "surroundAudioInfo",
    "remoteControllersBitmap",
    "gcmap",
    "gcpmap",
    "psmap",
    "gcpersist",
    "clientVrrRequested",
    "corever",
    "continuousAudio",
];

#[derive(Clone, Copy)]
enum RequestKind {
    Launch,
    Resume,
}

/// Only fixed protocol field names and bounded numbers can reach a refusal record.
#[derive(Debug)]
pub(crate) struct StreamQueryError {
    pub message: &'static str,
    pub field: Option<&'static str>,
    pub requested: Option<i64>,
    pub site: &'static std::panic::Location<'static>,
}

impl StreamQueryError {
    #[track_caller]
    fn field(message: &'static str, field: &'static str) -> Self {
        Self {
            message,
            field: Some(field),
            requested: None,
            site: std::panic::Location::caller(),
        }
    }

    #[track_caller]
    fn numeric(message: &'static str, field: &'static str, value: Option<&str>) -> Self {
        Self {
            requested: safe_integer(value),
            ..Self::field(message, field)
        }
    }
}

fn safe_integer(value: Option<&str>) -> Option<i64> {
    value?
        .parse::<i64>()
        .ok()
        .filter(|value| (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(value))
}

pub(super) fn launch_request(query: &Query) -> Result<LaunchRequest, StreamQueryError> {
    streaming_request(query, RequestKind::Launch)
}

/// A resume may omit its application selection. Zero remains unresolved here;
/// the authenticated session owner must supply the actual target later.
pub(super) fn resume_request(query: &Query) -> Result<LaunchRequest, StreamQueryError> {
    streaming_request(query, RequestKind::Resume)
}

fn streaming_request(query: &Query, kind: RequestKind) -> Result<LaunchRequest, StreamQueryError> {
    query
        .require_only(LAUNCH_FIELDS)
        .map_err(|message| StreamQueryError {
            message,
            field: None,
            requested: None,
            site: std::panic::Location::caller(),
        })?;
    let mut warnings = validate_metadata(query)?;
    let app_id = match (kind, query.get("appid")) {
        (RequestKind::Resume, None) => 0,
        (_, Some(value)) => application_id(value)
            .map_err(|message| StreamQueryError::numeric(message, "appid", Some(value)))?,
        (RequestKind::Launch, None) => {
            return Err(StreamQueryError::field("missing query field", "appid"));
        }
    };
    let (width, height, fps) = mode(
        query
            .required("mode")
            .map_err(|message| StreamQueryError::field(message, "mode"))?,
    )?;
    let key = query
        .required("rikey")
        .and_then(|value| hex_bytes(value, 16))
        .map_err(|message| StreamQueryError::field(message, "rikey"))?;
    let remote_input_key: [u8; 16] = key
        .try_into()
        .map_err(|_| StreamQueryError::field("remote input key must contain 16 bytes", "rikey"))?;
    let key_id = query
        .required("rikeyid")
        .and_then(key_id)
        .map_err(|message| StreamQueryError::field(message, "rikeyid"))?;
    let host_audio_enabled = query.get("localAudioPlayMode") == Some("1");
    let audio_channels =
        surround_audio_channels(query.get("surroundAudioInfo")).map_err(|message| {
            StreamQueryError::numeric(message, "surroundAudioInfo", query.get("surroundAudioInfo"))
        })?;
    let name = query.get("clientName").unwrap_or_default();
    let device_name = if name.is_empty() {
        None
    } else if name.len() > 128 || name.chars().any(char::is_control) {
        warnings.push(ignored_hint(query, "clientName", false));
        None
    } else {
        Some(name.to_owned())
    };
    Ok(LaunchRequest {
        hdr: query.get("hdrMode") == Some("1"),
        app_id,
        width,
        height,
        fps,
        host_audio_enabled,
        continuous_audio: query.get("continuousAudio") == Some("1"),
        audio_channels,
        device_name,
        remote_input_key,
        key_id,
        advisory_warnings: AdvisoryWarnings(warnings),
    })
}

fn application_id(value: &str) -> Result<u32, &'static str> {
    let app_id = decimal_u32(value)?;
    if app_id == 0 || app_id > i32::MAX as u32 {
        return Err("invalid application identifier");
    }
    Ok(app_id)
}

fn validate_metadata(query: &Query) -> Result<Vec<StreamWarning>, StreamQueryError> {
    if query.get("uniqueid").is_some() {
        query
            .client_id()
            .map_err(|message| StreamQueryError::field(message, "uniqueid"))?;
    }
    // SOPS and VRR are advisory; accepting them does not mutate host display policy.
    let mut warnings = Vec::new();
    for field in [
        "additionalStates",
        "sops",
        "gcpersist",
        "clientVrrRequested",
    ] {
        if optional_boolean(query, field).is_err() {
            warnings.push(ignored_hint(query, field, true));
        }
    }
    optional_boolean(query, "hdrMode")?;
    for field in ["clientHdrCapVersion", "clientHdrCapSupportedFlagsInUint32"] {
        if optional_decimal_mask(query, field, u32::MAX).is_err() {
            warnings.push(ignored_hint(query, field, true));
        }
    }
    if let Some(kind) = query.get("clientHdrCapMetaDataId")
        && !matches!(kind, "0" | "NV_STATIC_METADATA_TYPE_1")
    {
        warnings.push(ignored_hint(query, "clientHdrCapMetaDataId", false));
    }
    if let Some(data) = query.get("clientHdrCapDisplayData")
        && (data.len() > 512
            || !data
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'x'))
    {
        warnings.push(ignored_hint(query, "clientHdrCapDisplayData", false));
    }
    optional_boolean(query, "localAudioPlayMode")?;
    optional_boolean(query, "continuousAudio")?;
    // Slot hints are independent from permission to inject native input. Moonlight
    // reports player one in single-controller mode even without an attached pad.
    for field in ["remoteControllersBitmap", "gcmap", "gcpmap", "psmap"] {
        if optional_decimal_mask(query, field, u16::MAX as u32).is_err() {
            warnings.push(ignored_hint(query, field, true));
        }
    }
    if let Some(version) = query.get("corever")
        && version != "1"
    {
        return Err(StreamQueryError::numeric(
            "unsupported Moonlight core version",
            "corever",
            Some(version),
        ));
    }
    Ok(warnings)
}

fn ignored_hint(query: &Query, field: &str, numeric: bool) -> StreamWarning {
    let requested = if numeric {
        safe_integer(query.get(field)).map(WarningValue::Integer)
    } else {
        None
    };
    StreamWarning {
        code: "invalid_client_hint".into(),
        source: "amaterasu.nvhttp/1".into(),
        reason: field.into(),
        os_error: None,
        requested,
        effective: Some(WarningValue::Label("ignored".into())),
    }
}

/// `surroundAudioInfo` is `channel_mask << 16 | channel_count`, from
/// `SURROUNDAUDIOINFO_FROM_AUDIO_CONFIGURATION` in moonlight-common-c.
fn surround_audio_channels(value: Option<&str>) -> Result<u32, &'static str> {
    match value {
        None | Some("196610") => Ok(2),
        // Moonlight encodes channel_mask << 16 | channel_count.
        Some("4128774") => Ok(6),
        Some("104792072") => Ok(8),
        Some(_) => Err("unsupported surround audio configuration"),
    }
}

fn optional_boolean(query: &Query, name: &'static str) -> Result<(), StreamQueryError> {
    match query.get(name) {
        None | Some("0" | "1") => Ok(()),
        Some(value) => Err(StreamQueryError::numeric(
            "invalid boolean query field",
            name,
            Some(value),
        )),
    }
}

fn optional_decimal_mask(query: &Query, name: &str, maximum: u32) -> Result<(), &'static str> {
    match query.get(name) {
        None => Ok(()),
        Some(value) if decimal_u32(value).is_ok_and(|mask| mask <= maximum) => Ok(()),
        Some(_) => Err("invalid controller capability mask"),
    }
}

fn decimal_u32(value: &str) -> Result<u32, &'static str> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("invalid unsigned decimal value");
    }
    value
        .parse()
        .map_err(|_| "unsigned decimal value is out of range")
}

fn mode(value: &str) -> Result<(u32, u32, u32), StreamQueryError> {
    let mut fields = value.split('x');
    let width = mode_component(fields.next(), "mode.width")?;
    let height = mode_component(fields.next(), "mode.height")?;
    let fps = mode_component(fields.next(), "mode.fps")?;
    if fields.next().is_some() {
        return Err(StreamQueryError::field("invalid streaming mode", "mode"));
    }
    for (field, value) in [("mode.width", width), ("mode.height", height)] {
        if value < 16 || !value.is_multiple_of(2) {
            return Err(StreamQueryError {
                requested: Some(value.into()),
                ..StreamQueryError::field(
                    "streaming dimensions must be even and at least 16 pixels",
                    field,
                )
            });
        }
    }
    if fps == 0 {
        return Err(StreamQueryError {
            requested: Some(fps.into()),
            ..StreamQueryError::field("streaming frame rate must be positive", "mode.fps")
        });
    }
    Ok((width, height, fps))
}

fn mode_component(value: Option<&str>, field: &'static str) -> Result<u32, StreamQueryError> {
    let value = value.ok_or_else(|| StreamQueryError::field("invalid streaming mode", field))?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(StreamQueryError::numeric(
            "invalid streaming mode",
            field,
            Some(value),
        ));
    }
    value.parse().map_err(|_| {
        StreamQueryError::numeric("streaming mode is out of range", field, Some(value))
    })
}

fn key_id(value: &str) -> Result<u32, &'static str> {
    if let Some(magnitude) = value.strip_prefix('-') {
        if magnitude.is_empty() || !magnitude.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("invalid remote input key identifier");
        }
        let signed: i32 = value
            .parse()
            .map_err(|_| "remote input key identifier is out of range")?;
        if signed >= 0 {
            return Err("invalid remote input key identifier");
        }
        return Ok(signed as u32);
    }
    decimal_u32(value).map_err(|_| "invalid remote input key identifier")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(fields: &str) -> Result<LaunchRequest, StreamQueryError> {
        launch_request(&Query::parse(&format!("/launch?{fields}")).unwrap())
    }

    fn parse_resume(fields: &str) -> Result<LaunchRequest, StreamQueryError> {
        resume_request(&Query::parse(&format!("/resume?{fields}")).unwrap())
    }

    #[test]
    fn resume_without_application_id_preserves_streaming_and_client_metadata() {
        let request = parse_resume(concat!(
            "mode=1920x1080x60&rikey=00112233445566778899aabbccddeeff",
            "&rikeyid=-2147483648&uniqueid=untrusted-metadata&uuid=request-ticket",
            "&clientName=Living+Room&hdrMode=1&localAudioPlayMode=1",
            "&continuousAudio=1&surroundAudioInfo=4128774",
            "&additionalStates=1&sops=1&remoteControllersBitmap=1&gcmap=1",
            "&gcpersist=1&corever=1"
        ))
        .unwrap();
        assert_eq!(request.app_id, 0);
        assert_eq!(
            (request.width, request.height, request.fps),
            (1920, 1080, 60)
        );
        assert_eq!(
            request.remote_input_key,
            hex_bytes("00112233445566778899aabbccddeeff", 16)
                .unwrap()
                .as_slice()
        );
        assert_eq!(request.key_id, 0x8000_0000);
        assert_eq!(request.device_name.as_deref(), Some("Living Room"));
        assert!(request.hdr);
        assert!(request.host_audio_enabled);
        assert!(request.continuous_audio);
        assert_eq!(request.audio_channels, 6);
    }

    #[test]
    fn resume_accepts_an_explicit_positive_application_id() {
        let base = "mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=4294967295";
        for app_id in [1, 7, i32::MAX as u32] {
            let request = parse_resume(&format!("appid={app_id}&{base}")).unwrap();
            assert_eq!(request.app_id, app_id);
            assert_eq!(request.key_id, u32::MAX);
        }
    }

    #[test]
    fn explicit_application_ids_have_the_same_positive_range_for_launch_and_resume() {
        let base = "mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=1";
        for app_id in [
            "",
            "0",
            "-1",
            "+1",
            " 1",
            "1 ",
            "1.0",
            "app-1",
            "2147483648",
            "4294967295",
            "4294967296",
        ] {
            let fields = format!("appid={app_id}&{base}");
            assert!(parse(&fields).is_err(), "launch accepted {app_id:?}");
            assert!(parse_resume(&fields).is_err(), "resume accepted {app_id:?}");
        }
        assert!(parse(base).is_err(), "launch accepted an omitted target");
        assert_eq!(parse_resume(base).unwrap().app_id, 0);
    }

    #[test]
    fn unresolved_resume_still_requires_valid_mode_and_encryption_inputs() {
        for fields in [
            "rikey=00000000000000000000000000000000&rikeyid=1",
            "mode=16x16x1&rikeyid=1",
            "mode=16x16x1&rikey=00000000000000000000000000000000",
            "mode=&rikey=00000000000000000000000000000000&rikeyid=1",
            "mode=17x16x1&rikey=00000000000000000000000000000000&rikeyid=1",
            "mode=16x16x0&rikey=00000000000000000000000000000000&rikeyid=1",
            "mode=16x16x1&rikey=&rikeyid=1",
            "mode=16x16x1&rikey=000000000000000000000000000000&rikeyid=1",
            "mode=16x16x1&rikey=0000000000000000000000000000000g&rikeyid=1",
            "mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=",
            "mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=-0",
            "mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=-2147483649",
            "mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=4294967296",
        ] {
            assert!(parse_resume(fields).is_err(), "accepted {fields:?}");
        }
    }

    #[test]
    fn unresolved_resume_retains_metadata_and_configuration_authority_checks() {
        let base = "mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=1";
        for field in [
            "executable=attacker",
            "environment=attacker",
            "catalog_owner=attacker",
            "uniqueid=invalid%20client",
            "localAudioPlayMode=2",
            "continuousAudio=true",
            "surroundAudioInfo=6",
            "corever=2",
        ] {
            assert!(
                parse_resume(&format!("{base}&{field}")).is_err(),
                "accepted {field:?}"
            );
        }
        let bad_name = parse_resume(&format!("{base}&clientName={}", "x".repeat(129))).unwrap();
        assert!(bad_name.device_name.is_none());
        assert_eq!(bad_name.advisory_warnings.0[0].reason, "clientName");
        assert_eq!(
            parse_resume(&format!("{base}&gcmap=65536"))
                .unwrap()
                .advisory_warnings
                .0[0]
                .reason,
            "gcmap"
        );
    }

    #[test]
    fn parses_required_authority_and_normal_moonlight_metadata() {
        let request = parse(concat!(
            "appid=7&mode=1920x1080x60&rikey=00112233445566778899aabbccddeeff",
            "&rikeyid=-2147483648&uniqueid=ignored-for-authority&uuid=session_1",
            "&additionalStates=1&sops=0&hdrMode=0&clientHdrCapVersion=0",
            "&clientHdrCapSupportedFlagsInUint32=0&clientHdrCapMetaDataId=0",
            "&clientHdrCapDisplayData=&localAudioPlayMode=0&surroundAudioInfo=196610",
            "&remoteControllersBitmap=0&gcmap=0&psmap=0&gcpersist=0&corever=1"
        ))
        .unwrap();
        assert_eq!(request.app_id, 7);
        assert_eq!(
            (request.width, request.height, request.fps),
            (1920, 1080, 60)
        );
        assert_eq!(
            request.remote_input_key,
            hex_bytes("00112233445566778899aabbccddeeff", 16)
                .unwrap()
                .as_slice()
        );
        assert_eq!(request.key_id, 0x8000_0000);
        assert!(!request.host_audio_enabled);
    }

    #[test]
    fn parses_macos_desktop_launch_metadata_without_exposing_a_session_key() {
        let request = parse(concat!(
            "appid=2147483647&mode=3840x2160x120",
            "&rikey=00000000000000000000000000000000&rikeyid=-475305041",
            "&additionalStates=1&sops=1&localAudioPlayMode=1",
            "&surroundAudioInfo=196610&remoteControllersBitmap=0",
            "&gcmap=0&gcpmap=0&gcpersist=0&corever=1"
        ))
        .unwrap();
        assert_eq!(request.app_id, i32::MAX as u32);
        assert_eq!(
            (request.width, request.height, request.fps),
            (3840, 2160, 120)
        );
        assert_eq!(request.key_id, (-475_305_041_i32) as u32);
        assert!(request.host_audio_enabled);
    }

    #[test]
    fn accepts_full_unsigned_key_id_range_and_rejects_ambiguous_forms() {
        let request =
            parse("appid=1&mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=4294967295")
                .unwrap();
        assert_eq!(request.key_id, u32::MAX);
        for value in ["+1", "-0", "-2147483649", "4294967296", " 1", "1 "] {
            let fields = format!(
                "appid=1&mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid={value}"
            );
            assert!(parse(&fields).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn preserves_a_bounded_moonlight_client_name_as_the_device_override() {
        let request = parse(concat!(
            "appid=1&mode=16x16x1&rikey=00000000000000000000000000000000",
            "&rikeyid=1&clientName=Living+Room"
        ))
        .unwrap();
        assert_eq!(request.device_name.as_deref(), Some("Living Room"));

        let defaulted = parse(
            "appid=1&mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=1&clientName=",
        )
        .unwrap();
        assert_eq!(defaulted.device_name, None);

        let too_long = format!(
            "appid=1&mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=1&clientName={}",
            "x".repeat(129)
        );
        let ignored = parse(&too_long).unwrap();
        assert!(ignored.device_name.is_none());
        assert_eq!(ignored.advisory_warnings.0[0].reason, "clientName");
    }

    #[test]
    fn rejects_invalid_modes_keys_and_application_ids_without_panicking() {
        for fields in [
            "appid=0&mode=1920x1080x60&rikey=00112233445566778899aabbccddeeff&rikeyid=1",
            "appid=1&mode=1919x1080x60&rikey=00112233445566778899aabbccddeeff&rikeyid=1",
            "appid=1&mode=1920x1079x60&rikey=00112233445566778899aabbccddeeff&rikeyid=1",
            "appid=1&mode=1920x1080x0&rikey=00112233445566778899aabbccddeeff&rikeyid=1",
            "appid=1&mode=1920X1080X60&rikey=00112233445566778899aabbccddeeff&rikeyid=1",
            "appid=1&mode=1920x1080x60&rikey=00112233445566778899aabbccddeef&rikeyid=1",
            "appid=1&mode=1920x1080x60&rikey=00112233445566778899aabbccddeefg&rikeyid=1",
        ] {
            assert!(parse(fields).is_err(), "accepted {fields:?}");
        }
    }

    #[test]
    fn rejects_fields_that_could_override_trusted_launch_configuration() {
        for field in ["executable", "path", "args", "environment", "catalog_owner"] {
            let fields = format!(
                "appid=1&mode=1920x1080x60&rikey=00112233445566778899aabbccddeeff&rikeyid=1&{field}=attacker"
            );
            assert!(matches!(
                parse(&fields),
                Err(StreamQueryError {
                    message: "unsupported query field",
                    ..
                })
            ));
        }
    }

    #[test]
    fn rejects_requested_features_that_the_stream_profile_does_not_implement() {
        let base = concat!(
            "appid=1&mode=1920x1080x60&rikey=00112233445566778899aabbccddeeff",
            "&rikeyid=1"
        );
        for field in [
            "hdrMode=2",
            "surroundAudioInfo=393223",
            "localAudioPlayMode=2",
            "corever=2",
        ] {
            let fields = format!("{base}&{field}");
            assert!(parse(&fields).is_err(), "accepted {field:?}");
        }
    }

    #[test]
    fn invalid_advisory_metadata_warns_without_changing_request_identity() {
        let base = "appid=1&mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=1";
        let original = parse(base).unwrap();
        for field in [
            "additionalStates",
            "sops",
            "gcpersist",
            "clientVrrRequested",
            "remoteControllersBitmap",
            "gcmap",
            "gcpmap",
            "psmap",
            "clientHdrCapVersion",
            "clientHdrCapSupportedFlagsInUint32",
            "clientHdrCapMetaDataId",
            "clientHdrCapDisplayData",
        ] {
            let request = parse(&format!("{base}&{field}=not-a-valid-hint")).unwrap();
            assert!(
                request == original,
                "advisory metadata changed request identity: {field}"
            );
            assert_eq!(request.advisory_warnings.0.len(), 1, "{field}");
            let warning = &request.advisory_warnings.0[0];
            assert_eq!(warning.reason, field);
            assert_eq!(warning.code, "invalid_client_hint");
            assert!(warning.valid());
        }
        let negative = parse(&format!("{base}&gcmap=-1")).unwrap();
        assert_eq!(
            negative.advisory_warnings.0[0].requested,
            Some(WarningValue::Integer(-1))
        );
        let huge = parse(&format!("{base}&gcmap=9007199254740992")).unwrap();
        assert_eq!(huge.advisory_warnings.0[0].requested, None);
    }

    #[test]
    fn local_audio_play_mode_defaults_off_and_accepts_only_zero_or_one() {
        let base = "appid=1&mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=1";
        let defaulted = parse(base).unwrap();
        assert!(!defaulted.host_audio_enabled);
        let disabled = parse(&format!("{base}&localAudioPlayMode=0")).unwrap();
        assert!(!disabled.host_audio_enabled);
        let enabled = parse(&format!("{base}&localAudioPlayMode=1")).unwrap();
        assert!(enabled.host_audio_enabled);

        for value in ["2", "-1", "", "true"] {
            assert!(
                parse(&format!("{base}&localAudioPlayMode={value}")).is_err(),
                "accepted {value:?}"
            );
        }
    }

    #[test]
    fn surround_audio_info_accepts_only_exact_supported_client_layouts() {
        let base = "appid=1&mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=1";
        assert_eq!(parse(base).unwrap().audio_channels, 2);
        // AUDIO_CONFIGURATION_* masks and channel counts in Moonlight's Limelight.h.
        for (mask, channels) in [(0x3_u32, 2), (0x3F, 6), (0x63F, 8)] {
            let value = (mask << 16) | channels;
            assert_eq!(
                parse(&format!("{base}&surroundAudioInfo={value}"))
                    .unwrap()
                    .audio_channels,
                channels
            );
        }
        for value in [
            "",
            "0",
            "6",
            "393222",
            "524296",
            "393223",
            "+4128774",
            "4128774 ",
            "104792073",
            "",
            "0",
            "6",
            "393222",
            "524296",
            "393223",
            "524294",
            "+4128774",
            "4128774 ",
        ] {
            assert!(
                parse(&format!("{base}&surroundAudioInfo={value}")).is_err(),
                "accepted {value:?}"
            );
        }
    }

    #[test]
    fn default_moonlight_qt_launch_query_is_accepted() {
        // Captured from NvHTTP::startApp (nvhttp.cpp:208-240) with default
        // preferences: game optimizations on, multi-controller on with no pad,
        // stereo audio, SDR. Every field below is sent on every launch.
        let request = parse(concat!(
            "uniqueid=0123456789ABCDEF&uuid=0f1e2d3c4b5a69788796a5b4c3d2e1f0",
            "&appid=1&mode=1920x1080x60&additionalStates=1&sops=1",
            "&rikey=00112233445566778899aabbccddeeff&rikeyid=-1419472071",
            "&localAudioPlayMode=0&surroundAudioInfo=196610",
            "&remoteControllersBitmap=0&gcmap=0&psmap=0&gcpersist=0&corever=1"
        ))
        .unwrap();
        assert_eq!(
            (request.width, request.height, request.fps),
            (1920, 1080, 60)
        );
        assert_eq!(request.audio_channels, 2);
    }

    #[test]
    fn single_controller_mode_launch_query_is_accepted() {
        // With multi-controller off moonlight-qt always reports player 1
        // present and persists controllers (`gcpersist=1`); a PlayStation pad
        // sets bit 0 of `psmap`.
        let request = parse(concat!(
            "uniqueid=0123456789ABCDEF&appid=1&mode=1280x720x120&additionalStates=1&sops=1",
            "&rikey=00112233445566778899aabbccddeeff&rikeyid=7&localAudioPlayMode=1",
            "&surroundAudioInfo=4128774&remoteControllersBitmap=1&gcmap=1&psmap=1",
            "&gcpersist=1&corever=1"
        ))
        .unwrap();
        assert_eq!(request.audio_channels, 6);
        assert!(request.host_audio_enabled);
    }

    #[test]
    fn hdr_launch_request_from_moonlight_qt_preserves_hdr_request() {
        let request = parse(concat!(
            "appid=1&mode=1920x1080x60&rikey=00112233445566778899aabbccddeeff&rikeyid=1",
            "&hdrMode=1&clientHdrCapVersion=0&clientHdrCapSupportedFlagsInUint32=0",
            "&clientHdrCapMetaDataId=NV_STATIC_METADATA_TYPE_1",
            "&clientHdrCapDisplayData=0x0x0x0x0x0x0x0x0x0x0"
        ))
        .unwrap();
        assert!(request.hdr);
    }

    #[test]
    fn continuous_audio_defaults_off_and_accepts_only_zero_or_one() {
        let base = "appid=1&mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=1";
        assert!(!parse(base).unwrap().continuous_audio);
        assert!(
            !parse(&format!("{base}&continuousAudio=0"))
                .unwrap()
                .continuous_audio
        );
        assert!(
            parse(&format!("{base}&continuousAudio=1"))
                .unwrap()
                .continuous_audio
        );

        for value in ["2", "-1", "", "true"] {
            assert!(
                parse(&format!("{base}&continuousAudio={value}")).is_err(),
                "accepted {value:?}"
            );
        }
    }

    #[test]
    fn accepts_bounded_controller_slot_hints_without_authorizing_input() {
        let base = "appid=1&mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=1";
        for field in ["remoteControllersBitmap", "gcmap", "gcpmap", "psmap"] {
            for value in ["0", "1", "65535"] {
                assert!(
                    parse(&format!("{base}&{field}={value}")).is_ok(),
                    "rejected {value}"
                );
            }
            for value in ["65536", "-1", "+1", "1.0", " 1", "1 ", ""] {
                let request = parse(&format!("{base}&{field}={value}")).unwrap();
                assert_eq!(request.advisory_warnings.0[0].reason, field, "{value:?}");
            }
        }
        for value in ["0", "1"] {
            assert!(parse(&format!("{base}&gcpersist={value}")).is_ok());
        }
        for value in ["2", "-1", "", "true"] {
            assert_eq!(
                parse(&format!("{base}&gcpersist={value}"))
                    .unwrap()
                    .advisory_warnings
                    .0[0]
                    .reason,
                "gcpersist"
            );
        }
    }

    #[test]
    fn optional_vrr_request_hint_warns_on_malformed_values() {
        let base = "appid=1&mode=16x16x1&rikey=00000000000000000000000000000000&rikeyid=1";
        assert!(parse(base).is_ok());
        for value in ["0", "1"] {
            assert!(parse(&format!("{base}&clientVrrRequested={value}")).is_ok());
        }
        for value in ["2", "-1", "", "true"] {
            assert_eq!(
                parse(&format!("{base}&clientVrrRequested={value}"))
                    .unwrap()
                    .advisory_warnings
                    .0[0]
                    .reason,
                "clientVrrRequested"
            );
        }
        assert!(matches!(
            parse(&format!("{base}&clientVrrRequested=1&vrrUnsupported=1")),
            Err(StreamQueryError {
                message: "unsupported query field",
                ..
            })
        ));
    }
}
