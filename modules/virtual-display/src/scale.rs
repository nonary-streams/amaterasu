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
