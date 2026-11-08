use super::*;
use std::os::unix::fs::symlink;

/// A private fake sysfs tree shaped like `/sys`: `class/drm` entries link into
/// `devices`, and each device links to its driver under `bus`.
struct FakeSysfs {
    base: PathBuf,
}

impl FakeSysfs {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "amaterasu-drm-sysfs-{}",
            amaterasu_storage::random_identifier().unwrap()
        ));
        fs::create_dir_all(base.join("class/drm")).unwrap();
        Self { base }
    }

    fn root(&self) -> PathBuf {
        self.base.join("class/drm")
    }

    /// Adds `card<card>` backed by `device` (relative to `devices`) and bound
    /// to `driver`, with one `card<card>-<connector>` entry per connector.
    fn card(&self, card: u32, device: &str, driver: &str, connectors: &[&str]) {
        let device_path = self.base.join("devices").join(device);
        let card_path = device_path.join(format!("drm/card{card}"));
        let driver_path = self.base.join("bus/drivers").join(driver);
        fs::create_dir_all(&card_path).unwrap();
        fs::create_dir_all(&driver_path).unwrap();
        if !device_path.join("driver").exists() {
            symlink(&driver_path, device_path.join("driver")).unwrap();
        }
        symlink(&device_path, card_path.join("device")).unwrap();
        symlink(&card_path, self.root().join(format!("card{card}"))).unwrap();
        for connector in connectors {
            let connector_path = card_path.join(format!("card{card}-{connector}"));
            fs::create_dir_all(&connector_path).unwrap();
            symlink(
                &connector_path,
                self.root().join(format!("card{card}-{connector}")),
            )
            .unwrap();
        }
    }

    fn connectors(&self) -> Result<Vec<String>, String> {
        SysfsConnectors::new(self.root()).connectors()
    }
}

impl Drop for FakeSysfs {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn reloaded_vibeshine_pool_reports_its_actual_connector_ids_in_order() {
    let sysfs = FakeSysfs::new();
    sysfs.card(0, "pci0000:00/0000:00:02.0", "i915", &["DP-4", "HDMI-A-2"]);
    sysfs.card(
        3,
        "faux/vibeshine",
        "faux_driver",
        &[
            "Virtual-16",
            "Virtual-9",
            "Virtual-10",
            "Virtual-11",
            "Virtual-12",
            "Virtual-13",
            "Virtual-14",
            "Virtual-15",
        ],
    );
    fs::write(sysfs.root().join("version"), "drm 1.1.0").unwrap();
    assert_eq!(
        sysfs.connectors().unwrap(),
        (9..=16)
            .map(|index| format!("Virtual-{index}"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn only_vibeshine_cards_contribute_connectors() {
    let sysfs = FakeSysfs::new();
    // Stock vkms is also a faux device; its device name is not vibeshine.
    sysfs.card(4, "faux/vkms", "faux_driver", &["Virtual-20"]);
    sysfs.card(1, "pci0000:00/0000:01:00.0", "nvidia", &["Virtual-30"]);
    assert!(sysfs.connectors().is_err());

    sysfs.card(
        5,
        "platform/vibeshine_drm.0",
        "vibeshine_drm",
        &["Virtual-2"],
    );
    sysfs.card(3, "faux/vibeshine", "faux_driver", &["Virtual-1"]);
    assert_eq!(sysfs.connectors().unwrap(), ["Virtual-1", "Virtual-2"]);
}

#[test]
fn malformed_entries_are_ignored_and_missing_sysfs_fails_closed() {
    let sysfs = FakeSysfs::new();
    sysfs.card(
        3,
        "faux/vibeshine",
        "faux_driver",
        &["Virtual-0", "Virtual-01", "Virtual-2147483648", "Virtual-7"],
    );
    fs::create_dir(sysfs.root().join("cardX-Virtual-8")).unwrap();
    assert_eq!(sysfs.connectors().unwrap(), ["Virtual-7"]);

    // A syntactically valid connector with no readable card must not be
    // omitted from an inventory later used as proof of native absence.
    fs::create_dir(sysfs.root().join("card9-Virtual-9")).unwrap();
    assert!(sysfs.connectors().unwrap_err().contains("cannot inspect"));

    let missing = SysfsConnectors::new(sysfs.base.join("absent"));
    assert!(missing.connectors().is_err());
}

#[test]
fn duplicate_connector_ids_across_vibeshine_cards_are_ambiguous() {
    let sysfs = FakeSysfs::new();
    sysfs.card(3, "faux/vibeshine", "faux_driver", &["Virtual-9"]);
    sysfs.card(6, "platform/vibeshine.1", "vibeshine", &["Virtual-9"]);
    assert!(sysfs.connectors().unwrap_err().contains("ambiguous"));
}

#[test]
fn connector_ids_match_the_broker_name_rules() {
    assert_eq!(connector_id("Virtual-1"), Some(1));
    assert_eq!(connector_id("Virtual-16"), Some(16));
    assert_eq!(connector_id("Virtual-2147483647"), Some(2_147_483_647));
    for invalid in [
        "Virtual-0",
        "Virtual-01",
        "Virtual-",
        "Virtual-2147483648",
        "Virtual-1a",
        "virtual-1",
        "HDMI-A-1",
    ] {
        assert_eq!(connector_id(invalid), None, "{invalid}");
    }
}
