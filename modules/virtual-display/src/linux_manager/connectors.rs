//! Discovers the virtual connectors that exist on the vibeshine DRM card.
//! DRM connector type ids are global and are not reset when the driver
//! reloads, so a reloaded pool may be named `Virtual-9`..`Virtual-16` instead
//! of `Virtual-1`..`Virtual-8`. Discovery reads sysfs metadata only; it never
//! opens a DRM device node. The broker still authorizes every connector.
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

pub(super) const SYSFS_DRM_ROOT: &str = "/sys/class/drm";
/// Bounds directory scanning on an unusual sysfs.
const MAX_DRM_ENTRIES: usize = 4096;
/// More connectors than any vibeshine pool provides indicates a misread tree.
const MAX_CONNECTORS: usize = 64;

/// DRM connectors that currently report a connected display, both sorted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ConnectedDisplays {
    /// Vibeshine virtual connector names, such as `Virtual-9`.
    pub virtual_outputs: Vec<String>,
    /// Every connector outside the vibeshine pool, by its `card<N>-<name>` entry.
    pub other: Vec<String>,
}

pub(super) trait ConnectorInventory: Send {
    /// Every vibeshine virtual connector name, in ascending connector id order.
    fn connectors(&self) -> Result<Vec<String>, String>;
    /// Which connectors have a display attached. Reads sysfs only, so it can be
    /// polled without a broker request or a compositor query.
    fn connected_displays(&self) -> Result<ConnectedDisplays, String>;
}

/// Reads `<root>/card<N>-Virtual-<M>` entries whose card belongs to vibeshine.
pub(super) struct SysfsConnectors {
    root: PathBuf,
}

impl Default for SysfsConnectors {
    fn default() -> Self {
        Self::new(PathBuf::from(SYSFS_DRM_ROOT))
    }
}

impl SysfsConnectors {
    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn card_is_vibeshine(
        &self,
        cards: &mut BTreeMap<String, bool>,
        card: &str,
    ) -> Result<bool, String> {
        if let Some(owned) = cards.get(card) {
            return Ok(*owned);
        }
        let owned = is_vibeshine_card(&self.root.join(card))?;
        cards.insert(card.to_owned(), owned);
        Ok(owned)
    }
}

impl ConnectorInventory for SysfsConnectors {
    fn connectors(&self) -> Result<Vec<String>, String> {
        let entries = fs::read_dir(&self.root)
            .map_err(|error| format!("DRM sysfs is unavailable: {error}"))?;
        let mut cards = BTreeMap::new();
        let mut connectors = BTreeMap::new();
        for (count, entry) in entries.enumerate() {
            if count >= MAX_DRM_ENTRIES {
                return Err("DRM sysfs has too many entries".into());
            }
            let entry = entry.map_err(|error| format!("DRM sysfs read failed: {error}"))?;
            let file_name = entry.file_name();
            let Some((card, connector, id)) = file_name.to_str().and_then(split_connector_entry)
            else {
                continue;
            };
            if !self.card_is_vibeshine(&mut cards, card)? {
                continue;
            }
            if connectors.insert(id, connector.to_owned()).is_some() {
                return Err(format!(
                    "virtual display connector {connector} is ambiguous"
                ));
            }
            if connectors.len() > MAX_CONNECTORS {
                return Err("vibeshine card reports too many connectors".into());
            }
        }
        if connectors.is_empty() {
            return Err("no vibeshine virtual display connectors are present".into());
        }
        Ok(connectors.into_values().collect())
    }

    fn connected_displays(&self) -> Result<ConnectedDisplays, String> {
        let entries = fs::read_dir(&self.root)
            .map_err(|error| format!("DRM sysfs is unavailable: {error}"))?;
        let mut cards = BTreeMap::new();
        let mut displays = ConnectedDisplays::default();
        for (count, entry) in entries.enumerate() {
            if count >= MAX_DRM_ENTRIES {
                return Err("DRM sysfs has too many entries".into());
            }
            let entry = entry.map_err(|error| format!("DRM sysfs read failed: {error}"))?;
            let file_name = entry.file_name();
            let Some((name, card, connector)) = file_name.to_str().and_then(|name| {
                split_card_entry(name).map(|(card, connector)| (name, card, connector))
            }) else {
                continue;
            };
            // A connector without a readable status has no display attached.
            let connected = fs::read_to_string(entry.path().join("status"))
                .is_ok_and(|status| status.trim() == "connected");
            if !connected {
                continue;
            }
            if !self.card_is_vibeshine(&mut cards, card)? {
                displays.other.push(name.to_owned());
            } else if connector_id(connector).is_some() {
                displays.virtual_outputs.push(connector.to_owned());
            }
        }
        displays.virtual_outputs.sort();
        displays.other.sort();
        Ok(displays)
    }
}

/// The DRM connector type id of a public `Virtual-<id>` name: a positive
/// 32-bit id without leading zeros, matching the broker's accepted names.
pub(crate) fn connector_id(name: &str) -> Option<u32> {
    let index = name.strip_prefix("Virtual-")?;
    if index.is_empty()
        || index.len() > 10
        || index.starts_with('0')
        || !index.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let id: i32 = index.parse().ok()?;
    u32::try_from(id).ok().filter(|id| *id > 0)
}

/// Splits `card<N>-Virtual-<M>` into its card entry, connector name and id.
fn split_connector_entry(name: &str) -> Option<(&str, &str, u32)> {
    let (card, connector) = split_card_entry(name)?;
    Some((card, connector, connector_id(connector)?))
}

/// Splits `card<N>-<connector>` into its card entry and connector name.
fn split_card_entry(name: &str) -> Option<(&str, &str)> {
    let (card, connector) = name.split_once('-')?;
    let card_index = card.strip_prefix("card")?;
    if card_index.is_empty()
        || !card_index.bytes().all(|byte| byte.is_ascii_digit())
        || connector.is_empty()
    {
        return None;
    }
    Some((card, connector))
}

/// A card belongs to vibeshine when its device is bound to a vibeshine driver,
/// or is the `vibeshine` faux device. Faux devices all bind the generic
/// `faux_driver`, so the device name distinguishes vibeshine from stock vkms.
fn is_vibeshine_card(card: &Path) -> Result<bool, String> {
    // An unclassifiable virtual connector is not proof that an old owner has
    // vanished: another managed card may be in the middle of a hotplug.
    let device = fs::canonicalize(card.join("device")).map_err(|error| {
        format!(
            "cannot inspect virtual connector card {}: {error}",
            card.display()
        )
    })?;
    let driver = fs::canonicalize(device.join("driver")).map_err(|error| {
        format!(
            "cannot inspect virtual connector driver {}: {error}",
            card.display()
        )
    })?;
    let device_name = device.file_name().and_then(OsStr::to_str);
    Ok(match driver.file_name().and_then(OsStr::to_str) {
        Some("vibeshine" | "vibeshine_drm") => true,
        Some("faux_driver") => device_name == Some("vibeshine"),
        _ => false,
    })
}

#[cfg(test)]
mod tests;
