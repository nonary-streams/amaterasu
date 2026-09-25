//! Pure policy for virtual-display compositor scale requests.

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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScaleResolution {
    /// `None` means to retain the compositor's current scale.
    pub percentage: Option<u16>,
    /// The compositor factor corresponding to the resolved setting.
    pub compositor_factor: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleResolutionError {
    ZeroShortEdge,
}

/// Resolves fixed, automatic, and retained scale semantics without platform effects.
pub fn resolve_scale(
    setting: ScaleSetting,
    short_edge: u32,
    retained_factor: Option<f64>,
    current_factor: f64,
) -> Result<ScaleResolution, ScaleResolutionError> {
    match setting {
        ScaleSetting::Retain => Ok(ScaleResolution {
            percentage: None,
            compositor_factor: retained_factor.unwrap_or(current_factor),
        }),
        ScaleSetting::Fixed(percentage) => Ok(percentage_resolution(percentage)),
        ScaleSetting::Automatic => {
            if short_edge == 0 {
                return Err(ScaleResolutionError::ZeroShortEdge);
            }
            Ok(percentage_resolution(nearest_supported_percentage(
                short_edge,
            )))
        }
    }
}

fn percentage_resolution(percentage: u16) -> ScaleResolution {
    ScaleResolution {
        percentage: Some(percentage),
        compositor_factor: f64::from(percentage) / 100.0,
    }
}

fn nearest_supported_percentage(short_edge: u32) -> u16 {
    let target_numerator = u64::from(short_edge) * 100;
    SUPPORTED_SCALE_VALUES
        .into_iter()
        .filter_map(|percentage| u16::try_from(percentage).ok())
        .min_by_key(|percentage| {
            (
                (u64::from(*percentage) * 864).abs_diff(target_numerator),
                *percentage,
            )
        })
        .expect("the supported percentage list is non-empty")
}
