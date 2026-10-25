use super::*;

pub(super) fn launch(launch: &TrustedLaunch, capabilities: &Capabilities) -> Result<(), Error> {
    let scheme = if launch.encrypted_rtsp {
        "rtspenc://"
    } else {
        "rtsp://"
    };
    if launch.epoch == 0
        || !launch.rtsp_target.starts_with(scheme)
        || launch.rtsp_target.len() <= scheme.len()
        || launch.rtsp_target.len() > 2048
        || !launch
            .rtsp_target
            .bytes()
            .all(|byte| byte.is_ascii_graphic())
        || launch.session_token.is_empty()
        || launch.session_token.len() > 128
        || !launch
            .session_token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
        || [launch.audio_port, launch.video_port, launch.control_port].contains(&0)
        || !(16..=3840).contains(&capabilities.max_width)
        || !(16..=2160).contains(&capabilities.max_height)
        || !(1..=120).contains(&capabilities.max_fps)
        || !(100..=100000).contains(&capabilities.max_bitrate_kbps)
        || capabilities.supported_colors.is_empty()
    {
        return Err(Error::InvalidLaunch);
    }
    if let Some(discovery) = &launch.udp_discovery
        && (!launch.encrypted_rtsp
            || !valid_ping_payload(&discovery.audio_ping_payload)
            || !valid_ping_payload(&discovery.video_ping_payload)
            || discovery.audio_ping_payload == discovery.video_ping_payload)
    {
        return Err(Error::InvalidLaunch);
    }
    let transport = &capabilities.transport;
    if !transport.stereo_audio
        || !transport.enet_control
        || !transport.encrypted_control
        || !transport.control_v2
        || (capabilities.require_audio_encryption && !transport.encrypted_audio)
        || (capabilities.require_video_encryption && !transport.encrypted_video)
    {
        return Err(Error::UnsupportedMedia);
    }
    Ok(())
}

fn valid_ping_payload(payload: &str) -> bool {
    payload.len() == 16
        && payload
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(super) fn request(request: &Request) -> Result<(), Error> {
    if request.target.len() > super::super::MAX_LINE_BYTES
        || request.headers.len() > super::super::MAX_HEADERS
        || request.body.len() > super::super::MAX_BODY_BYTES
    {
        return Err(Error::InvalidHeaders);
    }
    let mut total = 0;
    for (index, (name, value)) in request.headers.iter().enumerate() {
        total += name.len() + value.len() + 4;
        if total > super::super::MAX_HEADER_BYTES
            || name.len() + value.len() > super::super::MAX_LINE_BYTES
            || !super::super::token(name)
            || !super::super::header_value(value)
            || request.headers[..index]
                .iter()
                .any(|(previous, _)| previous.eq_ignore_ascii_case(name))
        {
            return Err(Error::InvalidHeaders);
        }
    }
    if request.method != Method::Announce && !request.body.is_empty() {
        return Err(Error::InvalidBody);
    }
    if request.header("require").is_some()
        || request.header("proxy-require").is_some()
        || request.header("transfer-encoding").is_some()
        || request.header("content-encoding").is_some()
    {
        return Err(Error::InvalidHeaders);
    }
    if request
        .header("cseq")
        .is_some_and(|value| super::super::decimal(value) != Some(request.cseq))
        || request.header("content-length").is_some_and(|value| {
            super::super::decimal(value).map(|length| length as usize) != Some(request.body.len())
        })
    {
        return Err(Error::InvalidHeaders);
    }
    Ok(())
}

pub(super) fn target(request: &Request, expected: &str) -> Result<(), Error> {
    if request.target == expected {
        Ok(())
    } else {
        Err(Error::InvalidTarget)
    }
}

pub(super) fn transport(value: Option<&str>) -> Result<(), Error> {
    let value = value.ok_or(Error::UnsupportedTransport)?;
    let ports = value
        .strip_prefix("unicast;X-GS-ClientPort=")
        .ok_or(Error::UnsupportedTransport)?;
    let (first, last) = ports.split_once('-').ok_or(Error::UnsupportedTransport)?;
    let first = super::super::decimal(first)
        .filter(|port| (1..65535).contains(port))
        .ok_or(Error::UnsupportedTransport)?;
    if super::super::decimal(last) != Some(first + 1) {
        return Err(Error::UnsupportedTransport);
    }
    // These client ports are compatibility placeholders, never media destinations.
    Ok(())
}

pub(super) fn media(value: &sdp::Announce, capabilities: &Capabilities) -> Result<(), Error> {
    value
        .check_support(&capabilities.transport)
        .map_err(|error| {
            eprintln!("amaterasu rtsp announce capability rejection reason={error:?}");
            Error::UnsupportedMedia
        })?;
    if value.control != sdp::ControlProtocol::EncryptedEnet
        || !value.encryption.control_v2
        || !capabilities.supported_colors.contains(&value.video.color)
        || value.video.width > capabilities.max_width
        || value.video.height > capabilities.max_height
        || value.video.fps > capabilities.max_fps
        || value.video.bitrate_kbps > capabilities.max_bitrate_kbps
        || (capabilities.require_video_encryption && !value.encryption.video)
        || (capabilities.require_audio_encryption && !value.encryption.audio)
    {
        eprintln!(
            "amaterasu rtsp announce profile rejection control={} control_v2={} color={} geometry={} fps={} bitrate={} video_encryption={} audio_encryption={}",
            value.control == sdp::ControlProtocol::EncryptedEnet,
            value.encryption.control_v2,
            capabilities.supported_colors.contains(&value.video.color),
            value.video.width <= capabilities.max_width
                && value.video.height <= capabilities.max_height,
            value.video.fps <= capabilities.max_fps,
            value.video.bitrate_kbps <= capabilities.max_bitrate_kbps,
            !capabilities.require_video_encryption || value.encryption.video,
            !capabilities.require_audio_encryption || value.encryption.audio,
        );
        eprintln!(
            "amaterasu rtsp announce color requested={:?} supported={:?}",
            value.video.color, capabilities.supported_colors
        );
        return Err(Error::UnsupportedMedia);
    }
    Ok(())
}

pub(super) fn description(capabilities: &Capabilities) -> String {
    let transport = &capabilities.transport;
    let supported = 1
        | if transport.encrypted_video { 2 } else { 0 }
        | if transport.encrypted_audio { 4 } else { 0 };
    let requested =
        1 | if capabilities.require_video_encryption {
            2
        } else {
            0
        } | if capabilities.require_audio_encryption {
            4
        } else {
            0
        };
    // Moonlight's HEVC selection uses this legacy SDP marker even though the
    // video rtpmap remains H264. Emit it only for a ticket whose end-to-end
    // capability was enabled by the host; ordinary H.264 tickets stay exact.
    let hevc_marker = if transport.hevc {
        "a=fmtp:96 sprop-parameter-sets=AAAAAU\r\n"
    } else {
        ""
    };
    // Moonlight selects AV1 when this payload contains "AV1/90000" and both
    // sides support it, preferring it over HEVC. Like the HEVC marker, it is
    // emitted only for a ticket whose AV1 path is enabled end to end.
    let av1_marker = if transport.av1 {
        "a=rtpmap:98 AV1/90000\r\n"
    } else {
        ""
    };
    format!(
        "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=Amaterasu\r\nt=0 0\r\na=x-ss-general.featureFlags:0\r\na=x-ss-general.encryptionSupported:{supported}\r\na=x-ss-general.encryptionRequested:{requested}\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n{hevc_marker}{av1_marker}m=audio 0 RTP/AVP 97\r\na=rtpmap:97 opus/48000/2\r\n"
    )
}
