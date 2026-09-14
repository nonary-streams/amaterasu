use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const MAX_DISPLAYS: usize = 16;
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Status,
    Enumerate {
        display: Option<String>,
        offset: Option<usize>,
        limit: Option<usize>,
    },
    Apply {
        configuration: Configuration,
    },
    Restore {
        lease: String,
    },
    Recover {
        lease: String,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub displays: Vec<DisplaySetting>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DisplaySetting {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub refresh: Rational,
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub rotation: Rotation,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rotation {
    #[default]
    Identity,
    Rotate90,
    Rotate180,
    Rotate270,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rational {
    pub numerator: u32,
    pub denominator: u32,
}
impl Rational {
    #[cfg(any(windows, test))]
    pub fn equivalent(self, other: Self) -> bool {
        u64::from(self.numerator) * u64::from(other.denominator)
            == u64::from(other.numerator) * u64::from(self.denominator)
    }
}
impl Configuration {
    pub fn validate(&self) -> Result<(), DisplayError> {
        if self.displays.is_empty() || self.displays.len() > MAX_DISPLAYS {
            return Err(DisplayError::new(
                "invalid_request",
                "configuration must contain 1..16 displays",
            ));
        }
        let mut ids = BTreeSet::new();
        let mut primary = false;
        for display in &self.displays {
            if display.id.is_empty()
                || display.id.len() > 1024
                || display.id.chars().any(char::is_control)
                || !ids.insert(display.id.to_lowercase())
                || !(320..=16_384).contains(&display.width)
                || !(200..=16_384).contains(&display.height)
                || display.refresh.denominator == 0
                || display.refresh.numerator == 0
                || display.refresh.numerator > 1_000_000
                || display.refresh.denominator > 100_000
                || display.x.abs_diff(0) > 65_536
                || display.y.abs_diff(0) > 65_536
            {
                return Err(DisplayError::new(
                    "invalid_request",
                    "invalid display identity, size, rate, position or duplicate display",
                ));
            }
            let hz = f64::from(display.refresh.numerator) / f64::from(display.refresh.denominator);
            if !(1.0..=1000.0).contains(&hz) {
                return Err(DisplayError::new(
                    "invalid_request",
                    "refresh rate must be 1..1000 Hz",
                ));
            }
            primary |= display.x == 0 && display.y == 0;
        }
        if !primary {
            return Err(DisplayError::new(
                "invalid_request",
                "one display must begin at the primary origin (0,0)",
            ));
        }
        for (index, left) in self.displays.iter().enumerate() {
            for right in self.displays.iter().skip(index + 1) {
                if i64::from(left.x) < i64::from(right.x) + i64::from(right.width)
                    && i64::from(right.x) < i64::from(left.x) + i64::from(left.width)
                    && i64::from(left.y) < i64::from(right.y) + i64::from(right.height)
                    && i64::from(right.y) < i64::from(left.y) + i64::from(left.height)
                {
                    return Err(DisplayError::new(
                        "unsupported_topology",
                        "requested extended display rectangles must not overlap; clone requests are not supported",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Address {
    pub adapter_low: u32,
    pub adapter_high: i32,
    pub id: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceMode {
    pub width: u32,
    pub height: u32,
    pub pixel_format: i32,
    pub x: i32,
    pub y: i32,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Signal {
    pub pixel_rate: u64,
    pub horizontal_sync: Rational,
    pub vertical_sync: Rational,
    pub active_width: u32,
    pub active_height: u32,
    pub total_width: u32,
    pub total_height: u32,
    pub video_standard: u32,
    pub scan_line_ordering: i32,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayPath {
    pub id: String,
    pub source_name: String,
    pub source_address: Address,
    pub target_address: Address,
    pub source: SourceMode,
    pub signal: Signal,
    pub rotation: i32,
    pub scaling: i32,
    pub refresh: Rational,
    pub scan_line_ordering: i32,
    pub output_technology: i32,
    pub source_flags: u32,
    pub target_flags: u32,
    pub path_flags: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub format_version: u32,
    pub paths: Vec<DisplayPath>,
}
impl Snapshot {
    pub fn validate(&self) -> Result<(), DisplayError> {
        if self.format_version != 1 || self.paths.is_empty() || self.paths.len() > MAX_DISPLAYS {
            return Err(DisplayError::new(
                "invalid_snapshot",
                "unsupported or oversized display snapshot",
            ));
        }
        let mut ids = BTreeSet::new();
        for path in &self.paths {
            if path.id.is_empty()
                || path.id.len() > 1024
                || !ids.insert(&path.id)
                || path.source_name.is_empty()
                || path.source_name.len() > 128
                || path.source.width == 0
                || path.source.height == 0
                || path.refresh.denominator == 0
                || path.signal.vertical_sync.denominator == 0
                || path.signal.horizontal_sync.denominator == 0
                || !(1..=4).contains(&path.rotation)
            {
                return Err(DisplayError::new(
                    "invalid_snapshot",
                    "invalid display snapshot path",
                ));
            }
        }
        Ok(())
    }
    #[cfg(any(windows, test))]
    pub fn equivalent(&self, other: &Self) -> bool {
        self.paths.len() == other.paths.len()
            && self.paths.iter().zip(&other.paths).all(|(left, right)| {
                left.id == right.id
                    && left.source_name == right.source_name
                    && left.source == right.source
                    && left.signal == right.signal
                    && left.rotation == right.rotation
                    && left.scaling == right.scaling
                    && left.refresh.equivalent(right.refresh)
                    && left.scan_line_ordering == right.scan_line_ordering
            })
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub owner: String,
    pub lease: String,
    pub snapshot: Snapshot,
}
impl Journal {
    pub fn validate(&self) -> Result<(), DisplayError> {
        if self.owner.is_empty()
            || self.owner.len() > 128
            || self.lease.is_empty()
            || self.lease.len() > 128
        {
            return Err(DisplayError::new(
                "invalid_journal",
                "invalid display journal owner or lease",
            ));
        }
        self.snapshot.validate()
    }
}
#[derive(Debug)]
pub struct DisplayError {
    pub may_have_changed: bool,
    pub code: &'static str,
    pub message: String,
}
impl DisplayError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            may_have_changed: false,
            code,
            message: message.into(),
        }
    }
    #[cfg(any(windows, test))]
    pub fn after_mutation(mut self) -> Self {
        self.may_have_changed = true;
        self
    }
    pub fn payload(self) -> Value {
        json!({"ok":false,"error":{"code":self.code,"message":self.message.chars().take(512).collect::<String>()}})
    }
}
pub type DisplayResult<T> = Result<T, DisplayError>;
