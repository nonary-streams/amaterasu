use crate::model::HostSnapshot;
use std::time::Instant;

#[cfg(target_os = "linux")]
mod linux {
    use super::{HostSnapshot, Instant};
    use std::fs;
    use std::path::{Path, PathBuf};

    #[derive(Clone, Copy)]
    struct CpuCounters {
        total: u64,
        idle: u64,
    }

    #[derive(Clone, Copy)]
    struct NetworkCounters {
        received: u64,
        transmitted: u64,
    }

    pub struct Provider {
        root: PathBuf,
        previous_cpu: Option<CpuCounters>,
        previous_network: Option<(NetworkCounters, Instant)>,
    }

    impl Provider {
        pub fn new() -> Self {
            Self::with_root(PathBuf::from("/"))
        }

        fn with_root(root: PathBuf) -> Self {
            Self {
                root,
                previous_cpu: None,
                previous_network: None,
            }
        }

        pub fn sample(&mut self, now: Instant) -> HostSnapshot {
            let mut result = HostSnapshot::unavailable();
            if let Some(contents) = self.read("proc/stat")
                && let Some(current) = parse_cpu_counters(&contents)
            {
                if let Some(previous) = self.previous_cpu
                    && current.total > previous.total
                    && current.idle >= previous.idle
                {
                    let total = (current.total - previous.total) as f64;
                    let idle = (current.idle - previous.idle) as f64;
                    result.cpu_percent = finite_percent(100.0 * (total - idle) / total);
                }
                self.previous_cpu = Some(current);
            }

            if let Some(contents) = self.read("proc/meminfo")
                && let Some((used, total)) = parse_memory_bytes(&contents)
            {
                result.ram_used_bytes = Some(used);
                result.ram_total_bytes = Some(total);
            }

            if let Some(contents) = self.read("proc/net/dev")
                && let Some(current) = parse_network_counters(&contents)
            {
                if let Some((previous, sampled_at)) = self.previous_network
                    && current.received >= previous.received
                    && current.transmitted >= previous.transmitted
                {
                    let elapsed = now.saturating_duration_since(sampled_at).as_secs_f64();
                    if elapsed > 0.0 {
                        result.net_rx_bps = finite_nonnegative(
                            current.received.saturating_sub(previous.received) as f64 * 8.0
                                / elapsed,
                        );
                        result.net_tx_bps = finite_nonnegative(
                            current.transmitted.saturating_sub(previous.transmitted) as f64 * 8.0
                                / elapsed,
                        );
                    }
                }
                self.previous_network = Some((current, now));
            }

            result.cpu_temp_c = self.cpu_temperature();
            self.gpu_metrics(&mut result);
            result
        }

        pub fn reset_rate_baselines(&mut self) {
            self.previous_cpu = None;
            self.previous_network = None;
        }

        fn read(&self, relative: &str) -> Option<String> {
            fs::read_to_string(self.root.join(relative)).ok()
        }

        fn cpu_temperature(&self) -> Option<f64> {
            let thermal_root = self.root.join("sys/class/thermal");
            if let Ok(entries) = fs::read_dir(thermal_root) {
                for entry in entries.flatten().take(128) {
                    let path = entry.path();
                    let Ok(sensor_type) = fs::read_to_string(path.join("type")) else {
                        continue;
                    };
                    let normalized = sensor_type.trim().to_ascii_lowercase();
                    if (normalized.contains("cpu")
                        || normalized.contains("x86_pkg")
                        || normalized.contains("package"))
                        && let Some(value) = read_milli_temperature(&path.join("temp"))
                    {
                        return Some(value);
                    }
                }
            }

            let hwmon_root = self.root.join("sys/class/hwmon");
            let entries = fs::read_dir(hwmon_root).ok()?;
            for entry in entries.flatten().take(128) {
                let path = entry.path();
                let sensor_name = fs::read_to_string(path.join("name")).unwrap_or_default();
                let normalized = sensor_name.trim().to_ascii_lowercase();
                if !(normalized.contains("coretemp")
                    || normalized.contains("k10temp")
                    || normalized.contains("zenpower"))
                {
                    continue;
                }
                let Ok(sensors) = fs::read_dir(path) else {
                    continue;
                };
                for sensor in sensors.flatten().take(64) {
                    let name = sensor.file_name();
                    if name.to_string_lossy().starts_with("temp")
                        && name.to_string_lossy().ends_with("_input")
                        && let Some(value) = read_milli_temperature(&sensor.path())
                    {
                        return Some(value);
                    }
                }
            }
            None
        }

        fn gpu_metrics(&self, result: &mut HostSnapshot) {
            let drm_root = self.root.join("sys/class/drm");
            let Ok(cards) = fs::read_dir(drm_root) else {
                return;
            };
            for card in cards.flatten().take(64) {
                let name = card.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("card") || !name[4..].bytes().all(|byte| byte.is_ascii_digit())
                {
                    continue;
                }
                let device = card.path().join("device");
                if result.gpu_percent.is_none() {
                    result.gpu_percent = read_percentage(&device.join("gpu_busy_percent"));
                }
                if result.vram_used_bytes.is_none() {
                    result.vram_used_bytes = read_u64(&device.join("mem_info_vram_used"));
                }
                if result.vram_total_bytes.is_none() {
                    result.vram_total_bytes = read_u64(&device.join("mem_info_vram_total"));
                }
                if result.gpu_temp_c.is_none() {
                    result.gpu_temp_c = gpu_temperature(&device);
                }
                if result.gpu_percent.is_some()
                    || result.vram_used_bytes.is_some()
                    || result.vram_total_bytes.is_some()
                    || result.gpu_temp_c.is_some()
                {
                    break;
                }
            }
        }
    }

    fn parse_cpu_counters(contents: &str) -> Option<CpuCounters> {
        let line = contents.lines().find(|line| line.starts_with("cpu "))?;
        let mut values = line.split_whitespace().skip(1).map(str::parse::<u64>);
        let values = values
            .by_ref()
            // Linux reports guest and guest_nice separately even though they
            // are already included in user and nice. Summing those fields a
            // second time distorts the aggregate CPU delta.
            .take(8)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        if values.len() < 4 {
            return None;
        }
        let total = values
            .iter()
            .try_fold(0u64, |total, value| total.checked_add(*value))?;
        let idle = values[3].saturating_add(values.get(4).copied().unwrap_or(0));
        (total >= idle).then_some(CpuCounters { total, idle })
    }

    fn parse_memory_bytes(contents: &str) -> Option<(u64, u64)> {
        let mut total_kib = None;
        let mut available_kib = None;
        for line in contents.lines() {
            let (key, value) = line.split_once(':')?;
            let value = value.split_whitespace().next()?.parse::<u64>().ok()?;
            match key {
                "MemTotal" => total_kib = Some(value),
                "MemAvailable" => available_kib = Some(value),
                _ => {}
            }
        }
        let total = total_kib?.checked_mul(1024)?;
        let available = available_kib?.checked_mul(1024)?.min(total);
        Some((total - available, total))
    }

    fn parse_network_counters(contents: &str) -> Option<NetworkCounters> {
        let mut received = 0u64;
        let mut transmitted = 0u64;
        let mut interfaces = 0usize;
        for line in contents.lines().skip(2) {
            let (name, counters) = line.split_once(':')?;
            if name.trim() == "lo" {
                continue;
            }
            let values = counters
                .split_whitespace()
                .map(str::parse::<u64>)
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            if values.len() < 9 {
                return None;
            }
            received = received.checked_add(values[0])?;
            transmitted = transmitted.checked_add(values[8])?;
            interfaces += 1;
        }
        (interfaces > 0).then_some(NetworkCounters {
            received,
            transmitted,
        })
    }

    fn read_milli_temperature(path: &Path) -> Option<f64> {
        let value = fs::read_to_string(path).ok()?.trim().parse::<f64>().ok()? / 1_000.0;
        (value.is_finite() && (-50.0..=200.0).contains(&value)).then_some(value)
    }

    fn read_percentage(path: &Path) -> Option<f64> {
        let value = fs::read_to_string(path).ok()?.trim().parse::<f64>().ok()?;
        finite_percent(value)
    }

    fn read_u64(path: &Path) -> Option<u64> {
        fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    fn gpu_temperature(device: &Path) -> Option<f64> {
        let entries = fs::read_dir(device.join("hwmon")).ok()?;
        for hwmon in entries.flatten().take(32) {
            let Ok(sensors) = fs::read_dir(hwmon.path()) else {
                continue;
            };
            for sensor in sensors.flatten().take(64) {
                let name = sensor.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("temp")
                    && name.ends_with("_input")
                    && let Some(value) = read_milli_temperature(&sensor.path())
                {
                    return Some(value);
                }
            }
        }
        None
    }

    fn finite_percent(value: f64) -> Option<f64> {
        (value.is_finite() && (0.0..=100.0).contains(&value)).then_some(value)
    }

    fn finite_nonnegative(value: f64) -> Option<f64> {
        (value.is_finite() && value >= 0.0).then_some(value)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::fs;
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{Duration, Instant};

        fn fixture() -> PathBuf {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            std::env::temp_dir().join(format!(
                "amaterasu-host-metrics-{}-{unique}",
                std::process::id()
            ))
        }

        fn write(root: &Path, path: &str, contents: &str) {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        #[test]
        fn parses_cpu_memory_and_non_loopback_network_rates_from_linux_sources() {
            let root = fixture();
            let _ = fs::remove_dir_all(&root);
            write(
                &root,
                "proc/stat",
                "cpu 100 0 50 800 50 0 0 0 50000 30000\n",
            );
            write(
                &root,
                "proc/meminfo",
                "MemTotal: 1000 kB\nMemAvailable: 250 kB\n",
            );
            write(
                &root,
                "proc/net/dev",
                "header\nheader\n lo: 100 0 0 0 0 0 0 0 100 0 0 0 0 0 0 0\neth0: 1000 0 0 0 0 0 0 0 2000 0 0 0 0 0 0 0\n",
            );
            let mut provider = Provider::with_root(root.clone());
            let first_time = Instant::now();
            let first = provider.sample(first_time);
            assert_eq!(first.cpu_percent, None);
            assert_eq!(first.ram_total_bytes, Some(1_024_000));
            assert_eq!(first.ram_used_bytes, Some(768_000));
            assert_eq!(first.net_rx_bps, None);

            write(
                &root,
                "proc/stat",
                "cpu 150 0 75 875 50 0 0 0 90000 60000\n",
            );
            write(
                &root,
                "proc/net/dev",
                "header\nheader\n lo: 5000 0 0 0 0 0 0 0 7000 0 0 0 0 0 0 0\neth0: 1500 0 0 0 0 0 0 0 2500 0 0 0 0 0 0 0\n",
            );
            let second = provider.sample(first_time + Duration::from_secs(2));
            assert_eq!(second.cpu_percent, Some(50.0));
            assert_eq!(second.net_rx_bps, Some(2_000.0));
            assert_eq!(second.net_tx_bps, Some(2_000.0));
            let _ = fs::remove_dir_all(root);
        }

        #[test]
        fn parses_amd_drm_gpu_metrics_and_leaves_encoder_usage_unavailable() {
            let root = fixture();
            let _ = fs::remove_dir_all(&root);
            write(&root, "sys/class/drm/card0/device/gpu_busy_percent", "42\n");
            write(
                &root,
                "sys/class/drm/card0/device/mem_info_vram_used",
                "4096\n",
            );
            write(
                &root,
                "sys/class/drm/card0/device/mem_info_vram_total",
                "8192\n",
            );
            write(
                &root,
                "sys/class/drm/card0/device/hwmon/hwmon0/temp1_input",
                "65000\n",
            );
            let mut provider = Provider::with_root(root.clone());
            let sample = provider.sample(Instant::now());
            assert_eq!(sample.gpu_percent, Some(42.0));
            assert_eq!(sample.vram_used_bytes, Some(4096));
            assert_eq!(sample.vram_total_bytes, Some(8192));
            assert_eq!(sample.gpu_temp_c, Some(65.0));
            assert_eq!(sample.gpu_encoder_percent, None);
            let _ = fs::remove_dir_all(root);
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::Provider;

#[cfg(target_os = "linux")]
pub const PLATFORM: &str = "linux";

#[cfg(target_os = "linux")]
pub const AVAILABLE: bool = true;

#[cfg(not(target_os = "linux"))]
pub struct Provider;

#[cfg(not(target_os = "linux"))]
impl Provider {
    pub fn new() -> Self {
        Self
    }

    pub fn sample(&mut self, _now: Instant) -> HostSnapshot {
        HostSnapshot::unavailable()
    }

    pub fn reset_rate_baselines(&mut self) {}
}

#[cfg(not(target_os = "linux"))]
pub const PLATFORM: &str = "unsupported";

#[cfg(not(target_os = "linux"))]
pub const AVAILABLE: bool = false;
