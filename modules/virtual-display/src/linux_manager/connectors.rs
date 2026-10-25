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

pub(super) trait ConnectorInventory: Send {
    /// Every vibeshine virtual connector name, in ascending connector id order.
    fn connectors(&self) -> Result<Vec<String>, String>;
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

    fn card_is_vibeshine(&self, cards: &mut BTreeMap<String, bool>, card: &str) -> bool {
        if let Some(owned) = cards.get(card) {
            return *owned;
        }
        let owned = is_vibeshine_card(&self.root.join(card));
        cards.insert(card.to_owned(), owned);
        owned
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
            if !self.card_is_vibeshine(&mut cards, card) {
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
}

/// The DRM connector type id of a public `Virtual-<id>` name: a positive
/// 32-bit id without leading zeros, matching the broker's accepted names.
pub(super) fn connector_id(name: &str) -> Option<u32> {
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
    let (card, connector) = name.split_once('-')?;
    let card_index = card.strip_prefix("card")?;
    if card_index.is_empty() || !card_index.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((card, connector, connector_id(connector)?))
}

/// A card belongs to vibeshine when its device is bound to a vibeshine driver,
/// or is the `vibeshine` faux device. Faux devices all bind the generic
/// `faux_driver`, so the device name distinguishes vibeshine from stock vkms.
fn is_vibeshine_card(card: &Path) -> bool {
    let Ok(device) = fs::canonicalize(card.join("device")) else {
        return false;
    };
    let Ok(driver) = fs::canonicalize(device.join("driver")) else {
        return false;
    };
    let device_name = device.file_name().and_then(OsStr::to_str);
    match driver.file_name().and_then(OsStr::to_str) {
        Some("vibeshine" | "vibeshine_drm") => true,
        Some("faux_driver") => device_name == Some("vibeshine"),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
