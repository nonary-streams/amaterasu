//! Validate the exact FFmpeg conversion ABI embedded in a selected video artifact.
use amaterasu_package::PackageResult;
const MARKER: &[u8] = b"AMATERASU_SWSCALE_ABI=";

pub(crate) fn embedded_abi(artifact: &[u8]) -> PackageResult<Option<(u32, u32)>> {
    let mut matches = artifact
        .windows(MARKER.len())
        .enumerate()
        .filter(|(_, bytes)| *bytes == MARKER);
    let Some((offset, _)) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err("video has duplicate swscale ABI markers".into());
    }
    let suffix = &artifact[offset + MARKER.len()..];
    let end = suffix
        .iter()
        .position(|byte| *byte == 0)
        .filter(|end| *end > 0 && *end <= 24)
        .ok_or("video swscale ABI marker is malformed")?;
    let marker =
        std::str::from_utf8(&suffix[..end]).map_err(|_| "video swscale ABI marker is malformed")?;
    let (scale, utility) = marker
        .split_once(',')
        .ok_or("video swscale ABI marker is malformed")?;
    let parse = |value: &str| -> PackageResult<u32> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("video swscale ABI marker is malformed".into());
        }
        value
            .parse::<u32>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| "video swscale ABI marker is malformed".into())
    };
    Ok(Some((parse(scale)?, parse(utility)?)))
}

pub(crate) fn validate(artifact: &[u8]) -> PackageResult<()> {
    if let Some((scale, utility)) = embedded_abi(artifact)?
        && !runtime_available(scale, utility)
    {
        return Err(format!(
            "required FFmpeg conversion runtime swscale {scale}/avutil {utility} is unavailable"
        ));
    }
    Ok(())
}
fn runtime_available(scale: u32, utility: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        [
            (
                format!("libswscale.so.{scale}"),
                &[
                    "sws_getCachedContext",
                    "sws_getCoefficients",
                    "sws_setColorspaceDetails",
                    "sws_scale",
                    "sws_freeContext",
                ][..],
            ),
            (format!("libavutil.so.{utility}"), &["avutil_version"][..]),
        ]
        .into_iter()
        .all(|(name, symbols)| {
            // SAFETY: only system sonames with validated numeric ABI majors
            // are loaded. Symbol addresses are checked but never invoked.
            let Ok(library) = (unsafe { libloading::Library::new(name) }) else {
                return false;
            };
            symbols.iter().all(|symbol| unsafe {
                library
                    .get::<*mut std::ffi::c_void>(symbol.as_bytes())
                    .is_ok()
            })
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (scale, utility);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_unmarked_or_exact_conversion_abi_and_rejects_corrupt_markers() {
        assert_eq!(embedded_abi(b"unmarked video").unwrap(), None);
        assert_eq!(
            embedded_abi(b"prefixAMATERASU_SWSCALE_ABI=9,60\0suffix").unwrap(),
            Some((9, 60))
        );
        for bytes in [
            b"AMATERASU_SWSCALE_ABI=9,60".as_slice(),
            b"AMATERASU_SWSCALE_ABI=0,60\0",
            b"AMATERASU_SWSCALE_ABI=9,/60\0",
            b"AMATERASU_SWSCALE_ABI=9,60,1\0",
            b"AMATERASU_SWSCALE_ABI=9,60\0AMATERASU_SWSCALE_ABI=9,60\0",
        ] {
            assert!(embedded_abi(bytes).is_err());
        }
    }
}
