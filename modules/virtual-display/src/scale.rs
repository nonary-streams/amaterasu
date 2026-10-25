//! Accepted virtual-display scale request values and validation.

/// Values accepted by `dd_virtual_display_scale`, in ascending order.
pub const SUPPORTED_SCALE_VALUES: [i32; 14] = [
    -1, 0, 100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleSetting {
    Retain,
    Automatic,
    Fixed(u16),
}

impl TryFrom<i32> for ScaleSetting {
    type Error = ();

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        if !SUPPORTED_SCALE_VALUES.contains(&value) {
            return Err(());
        }
        match value {
            0 => Ok(Self::Retain),
            -1 => Ok(Self::Automatic),
            percentage => Ok(Self::Fixed(percentage as u16)),
        }
    }
}

impl ScaleSetting {
    /// The compositor scale factor to apply for a `width`x`height` output, or
    /// `None` to keep the compositor's own (or remembered) choice.
    pub fn factor(self, width: u32, height: u32) -> Option<f64> {
        match self {
            Self::Retain => None,
            Self::Automatic => Some(f64::from(automatic_percent(width, height)) / 100.0),
            Self::Fixed(percent) => Some(f64::from(percent) / 100.0),
        }
    }
}

/// Keeps about 864 logical pixels on the short edge, rounded to the nearest
/// supported percentage, as Vibeshine's automatic virtual-display scale does.
pub fn automatic_percent(width: u32, height: u32) -> u16 {
    let ideal = f64::from(width.min(height)) * 100.0 / 864.0;
    SUPPORTED_SCALE_VALUES
        .iter()
        .filter(|value| **value >= 100)
        .min_by(|left, right| {
            let distance = |value: i32| (f64::from(value) - ideal).abs();
            distance(**left).total_cmp(&distance(**right))
        })
        .map_or(100, |value| *value as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_scale_keeps_about_864_logical_rows() {
        assert_eq!(automatic_percent(3840, 2160), 250);
        assert_eq!(automatic_percent(2560, 1440), 175);
        assert_eq!(automatic_percent(1920, 1080), 125);
        assert_eq!(automatic_percent(1280, 720), 100);
        assert_eq!(automatic_percent(2160, 3840), 250);
        assert_eq!(ScaleSetting::Retain.factor(3840, 2160), None);
        assert_eq!(ScaleSetting::Automatic.factor(3840, 2160), Some(2.5));
        assert_eq!(ScaleSetting::Fixed(150).factor(3840, 2160), Some(1.5));
    }
}
