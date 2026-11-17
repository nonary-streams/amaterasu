//! Explicit Windows streaming policy for a host alongside another server.
use super::{installer_policy, remote_sessions};
use amaterasu_contracts::RuntimeConfig;
use amaterasu_package::PackageResult;
use serde_json::json;
use std::collections::BTreeMap;

/// Construct a signed-installer policy with isolated Moonlight and management ports.
/// The output must be an enumerated DXGI identifier, not a guessed display.
pub fn policy(
    web_directory: &str,
    trusted_keys: BTreeMap<String, String>,
    base_port: u16,
    output: &str,
) -> PackageResult<RuntimeConfig> {
    if !(1029..=65514).contains(&base_port) {
        return Err("Moonlight base port must be between 1029 and 65514".into());
    }
    if output.is_empty()
        || output.len() > 512
        || output.trim() != output
        || output.chars().any(char::is_control)
        || output == "SELECT_OUTPUT_ID"
    {
        return Err("a valid enumerated DXGI output identifier is required".into());
    }
    let mut policy = installer_policy(web_directory, trusted_keys);
    policy.settings.get_mut("amaterasu.management").unwrap()["bind"] =
        json!(format!("127.0.0.1:{}", base_port + 1));
    let nvhttp = policy.settings.get_mut("amaterasu.nvhttp").unwrap();
    nvhttp["http_bind"] = json!(format!("0.0.0.0:{base_port}"));
    nvhttp["https_bind"] = json!(format!("0.0.0.0:{}", base_port - 5));
    nvhttp["allow_remote"] = json!(true);
    nvhttp["address_family"] = json!("ipv4");
    nvhttp["bind_address"] = json!("");
    nvhttp["streaming"] = json!({
        "desktop": {"enabled": true},
        "capture": {"capture_provider":"dxgi", "output":output, "encoder":"auto", "audio_endpoint":"default"},
        "profile": {"max_width":3840, "max_height":2160, "max_fps":120, "bitrate_kbps":100000,
            "max_reference_frames":1, "packet_size":1024, "fec_percentage":20,
            "min_parity_shards":2, "max_fec_blocks":1, "packet_ms":10,
            "audio_bitrate":128000, "encrypt_video":true, "encrypt_audio":true,
            "permit_legacy_audio_iv_wrap":true}
    });
    let rtsp = policy.settings.get_mut("amaterasu.rtsp").unwrap();
    rtsp["enabled"] = json!(true);
    rtsp["allow_remote"] = json!(true);
    rtsp["bind"] = json!(format!("0.0.0.0:{}", base_port + 21));
    rtsp["address_family"] = json!("ipv4");
    rtsp["bind_address"] = json!("");
    rtsp["allowed_principals"] = json!(["amaterasu.nvhttp"]);
    let stream = policy.settings.get_mut("amaterasu.stream-session").unwrap();
    stream["enabled"] = json!(true);
    stream["allow_remote"] = json!(true);
    stream["allowed_principals"] = json!(["amaterasu.rtsp"]);
    stream["enable_input"] = json!(true);
    for id in ["amaterasu.media-transport", "amaterasu.control-transport"] {
        let transport = policy.settings.get_mut(id).unwrap();
        transport["enabled"] = json!(true);
        transport["allow_remote"] = json!(true);
        transport["allowed_principals"] = json!(["amaterasu.stream-session"]);
    }
    for id in [
        "amaterasu.capture-dxgi",
        "amaterasu.audio",
        "amaterasu.audio-encode",
        "amaterasu.encode",
        "amaterasu.video",
    ] {
        policy.settings.get_mut(id).unwrap()["enabled"] = json!(true);
    }
    policy.settings.get_mut("amaterasu.input").unwrap()["allow_injection"] = json!(true);
    remote_sessions::install(&mut policy)?;
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_by_side_policy_keeps_all_listeners_on_the_requested_port_family() {
        let result = policy("web", BTreeMap::new(), 48989, "display-1").unwrap();
        assert_eq!(
            result.settings["amaterasu.management"]["bind"],
            "127.0.0.1:48990"
        );
        assert_eq!(
            result.settings["amaterasu.nvhttp"]["http_bind"],
            "0.0.0.0:48989"
        );
        assert_eq!(
            result.settings["amaterasu.nvhttp"]["https_bind"],
            "0.0.0.0:48984"
        );
        assert_eq!(result.settings["amaterasu.rtsp"]["bind"], "0.0.0.0:49010");
        assert_eq!(
            result.settings["amaterasu.nvhttp"]["streaming"]["capture"]["output"],
            "display-1"
        );
        assert_eq!(
            result.settings["amaterasu.rtsp"]["retirement_status_owners"]["amaterasu.remote-session"],
            "amaterasu.nvhttp"
        );
        let nvhttp: amaterasu_nvhttp::Settings =
            serde_json::from_value(result.settings["amaterasu.nvhttp"].clone()).unwrap();
        nvhttp.validate().unwrap();
        amaterasu_runtime::Broker::new(result.clone()).unwrap();
        assert_eq!(
            nvhttp.resolved_http_binds().unwrap(),
            vec!["0.0.0.0:48989".parse().unwrap()]
        );
        assert!(policy("web", BTreeMap::new(), 1028, "display-1").is_err());
        assert!(policy("web", BTreeMap::new(), 48989, "SELECT_OUTPUT_ID").is_err());
    }
}
