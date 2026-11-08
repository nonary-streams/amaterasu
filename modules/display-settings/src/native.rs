//! The default Linux display-settings backend. All selectors remain inside this provider.
use crate::engine::{Backend, Observation, Result};
use amaterasu_module_sdk::display_settings::{
    Adjustment, HdrMode, Position, Resolution, Scale, Target,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub struct NativeBackend;
fn run(arguments: &[String]) -> Result<Vec<u8>> {
    let mut child = Command::new("kscreen-doctor")
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "display_backend_unavailable")?;
    let mut pipe = child.stdout.take().ok_or("display_backend_unavailable")?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.by_ref()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| "display_backend_failed")?
        .map_err(|_| "display_backend_failed")?;
    if !status.is_some_and(|s| s.success()) || bytes.len() > 1024 * 1024 {
        return Err("display_backend_failed".into());
    }
    Ok(bytes)
}
fn query() -> Result<Value> {
    serde_json::from_slice(&run(&["-j".into()])?).map_err(|_| "display_inventory_invalid".into())
}
fn target(output: &Value) -> Result<Target> {
    let name = output["name"].as_str().ok_or("display_inventory_invalid")?;
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err("display_inventory_invalid".into());
    }
    let id = format!("{:x}", Sha256::digest(name));
    // A compositor restart invalidates every old target, even if KScreen
    // reuses the same connector number. Never restore onto a new Wayland seat.
    #[cfg(unix)]
    let compositor = {
        use std::os::unix::fs::MetadataExt;
        let socket = std::env::var_os("WAYLAND_DISPLAY").ok_or("wayland_session_unavailable")?;
        let path = std::path::PathBuf::from(socket);
        let path = if path.is_absolute() {
            path
        } else {
            std::path::PathBuf::from(
                std::env::var_os("XDG_RUNTIME_DIR").ok_or("wayland_session_unavailable")?,
            )
            .join(path)
        };
        let metadata = std::fs::metadata(path).map_err(|_| "wayland_session_unavailable")?;
        (
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    };
    #[cfg(not(unix))]
    let compositor = ();
    let generation = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(&output["id"], &output["uuid"], &output["edid"], compositor))
                .map_err(|_| "display_inventory_invalid")?
        )
    );
    Ok(Target { id, generation })
}
fn modes(output: &Value) -> Result<&Vec<Value>> {
    output["modes"]
        .as_array()
        .ok_or("display_modes_unavailable".into())
}
fn resolution(mode: &Value) -> Result<Resolution> {
    Ok(Resolution {
        width: mode["size"]["width"]
            .as_u64()
            .and_then(|n| n.try_into().ok())
            .ok_or("display_mode_invalid")?,
        height: mode["size"]["height"]
            .as_u64()
            .and_then(|n| n.try_into().ok())
            .ok_or("display_mode_invalid")?,
    })
}
fn refresh(mode: &Value) -> Result<u32> {
    let hz = mode["refreshRate"].as_f64().ok_or("display_mode_invalid")?;
    if !hz.is_finite() || !(1.0..=1000.0).contains(&hz) {
        return Err("display_mode_invalid".into());
    }
    Ok((hz * 1000.0).round() as u32)
}
fn exact_scale(scale: f64) -> Result<Scale> {
    let text = scale.to_string();
    let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let denominator = 10u64
        .checked_pow(fraction.len() as u32)
        .ok_or("display_scale_unavailable")?;
    let numerator = format!("{whole}{fraction}")
        .parse()
        .map_err(|_| "display_scale_unavailable")?;
    Ok(Scale {
        numerator,
        denominator,
    })
}
fn observe(output: &Value) -> Result<Observation> {
    let current = modes(output)?
        .iter()
        .find(|m| m["id"] == output["currentModeId"])
        .ok_or("display_mode_unavailable")?;
    let scale = output["scale"]
        .as_f64()
        .ok_or("display_scale_unavailable")?;
    if !scale.is_finite() || !(0.5..=5.0).contains(&scale) {
        return Err("display_scale_unavailable".into());
    }
    let hdr = output["hdr"].as_bool().map(|enabled| {
        if enabled {
            HdrMode::Enabled
        } else {
            HdrMode::Disabled
        }
    });
    Ok(Observation {
        target: target(output)?,
        source: {
            let name = output["name"].as_str().unwrap();
            if name.starts_with("Virtual-") {
                format!("kwin:{name}")
            } else {
                name.into()
            }
        },
        settings: Adjustment {
            hdr,
            wide_color_gamut: output["wcg"].as_bool(),
            resolution: Some(resolution(current)?),
            refresh_millihz: Some(refresh(current)?),
            scale: Some(exact_scale(scale)?),
            position: Some(Position {
                x: output["pos"]["x"]
                    .as_i64()
                    .and_then(|n| n.try_into().ok())
                    .ok_or("display_position_invalid")?,
                y: output["pos"]["y"]
                    .as_i64()
                    .and_then(|n| n.try_into().ok())
                    .ok_or("display_position_invalid")?,
            }),
        },
    })
}
impl Backend for NativeBackend {
    fn enumerate(&mut self) -> Result<Vec<Observation>> {
        let value = query()?;
        let outputs = value["outputs"]
            .as_array()
            .ok_or("display_inventory_invalid")?;
        outputs
            .iter()
            .filter(|o| o["connected"] == true && o["enabled"] == true)
            .map(observe)
            .collect()
    }
    fn apply(&mut self, expected: &Target, patch: &Adjustment) -> Result<()> {
        patch.validate()?;
        let value = query()?;
        let output = value["outputs"]
            .as_array()
            .ok_or("display_inventory_invalid")?
            .iter()
            .find(|o| {
                target(o).as_ref() == Ok(expected) && o["connected"] == true && o["enabled"] == true
            })
            .ok_or("display_target_unavailable")?;
        let current = observe(output)?;
        let name = output["id"].as_u64().ok_or("display_inventory_invalid")?;
        let prefix = format!("output.{name}");
        let mut args = Vec::new();
        if let Some(hdr) = patch.hdr {
            if current.settings.hdr.is_none() {
                return Err("unsupported_display_setting".into());
            }
            args.push(format!(
                "{prefix}.hdr.{}",
                if hdr == HdrMode::Enabled {
                    "enable"
                } else {
                    "disable"
                }
            ));
        }
        if let Some(enabled) = patch.wide_color_gamut {
            if current.settings.wide_color_gamut.is_none() {
                return Err("unsupported_display_setting".into());
            }
            args.push(format!(
                "{prefix}.wcg.{}",
                if enabled { "enable" } else { "disable" }
            ));
        }
        if patch.resolution.is_some() || patch.refresh_millihz.is_some() {
            let size = patch.resolution.or(current.settings.resolution).unwrap();
            let rate = patch
                .refresh_millihz
                .or(current.settings.refresh_millihz)
                .unwrap();
            let selected = modes(output)?
                .iter()
                .find(|mode| {
                    resolution(mode).ok() == Some(size) && refresh(mode).is_ok_and(|r| r == rate)
                })
                .ok_or("unsupported_display_mode")?;
            let mode = selected["id"].as_str().ok_or("display_mode_invalid")?;
            args.push(format!("{prefix}.mode.{mode}"));
        }
        if let Some(scale) = patch.scale {
            args.push(format!("{prefix}.scale.{}", scale.as_f64()));
        }
        if let Some(pos) = patch.position {
            args.push(format!("{prefix}.position.{},{}", pos.x, pos.y));
        }
        run(&args)?;
        let after = self
            .enumerate()?
            .into_iter()
            .find(|o| &o.target == expected)
            .ok_or("display_target_unavailable")?;
        if !crate::engine::matches_patch(&after.settings, patch) {
            return Err("display_adjustment_unverified".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires a running Wayland KScreen session; reads inventory only"]
    fn current_wayland_inventory_is_readable() {
        let observations = NativeBackend.enumerate().unwrap();
        assert!(!observations.is_empty());
        for observation in observations {
            assert!(observation.settings.resolution.is_some());
            assert!(!observation.target.generation.is_empty());
        }
    }
}
