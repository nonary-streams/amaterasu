use amaterasu_abi::{STATUS_INVALID_ARGUMENT, Status};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

const NORMALIZED_MAXIMUM: u64 = u16::MAX as u64;

/// A half-open rectangle in the platform's virtual-desktop pointer coordinates.
/// Windows uses physical pixels; Wayland uses compositor logical coordinates.
///
/// The origin may be negative. A valid rectangle is nonempty and every physical
/// pixel coordinate through `origin + extent - 1` is representable by `i32`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct DesktopRect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

impl DesktopRect {
    pub fn new(left: i32, top: i32, width: u32, height: u32) -> Result<Self, Status> {
        let rectangle = Self {
            left,
            top,
            width,
            height,
        };
        rectangle.validate()?;
        Ok(rectangle)
    }

    pub fn validate(self) -> Result<(), Status> {
        checked_exclusive_end(self.left, self.width)?;
        checked_exclusive_end(self.top, self.height)?;
        Ok(())
    }

    fn right_exclusive(self) -> Result<i64, Status> {
        checked_exclusive_end(self.left, self.width)
    }

    fn bottom_exclusive(self) -> Result<i64, Status> {
        checked_exclusive_end(self.top, self.height)
    }

    fn contains(self, rectangle: Self) -> Result<bool, Status> {
        self.validate()?;
        rectangle.validate()?;
        Ok(i64::from(rectangle.left) >= i64::from(self.left)
            && i64::from(rectangle.top) >= i64::from(self.top)
            && rectangle.right_exclusive()? <= self.right_exclusive()?
            && rectangle.bottom_exclusive()? <= self.bottom_exclusive()?)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DesktopRectFields {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

impl<'de> Deserialize<'de> for DesktopRect {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let fields = DesktopRectFields::deserialize(deserializer)?;
        Self::new(fields.left, fields.top, fields.width, fields.height)
            .map_err(|_| D::Error::custom("invalid nonempty desktop rectangle"))
    }
}

/// The captured output and complete virtual desktop containing it.
///
/// Both rectangles use the same platform pointer coordinate system. Capture
/// image pixels may use a different scale and must not be substituted here.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct CaptureGeometry {
    pub output: DesktopRect,
    pub virtual_desktop: DesktopRect,
}

impl CaptureGeometry {
    pub fn new(output: DesktopRect, virtual_desktop: DesktopRect) -> Result<Self, Status> {
        let geometry = Self {
            output,
            virtual_desktop,
        };
        geometry.validate()?;
        Ok(geometry)
    }

    /// Construct geometry for one whole output from the bounding rectangle of
    /// every physical display. The selected output must exactly match a member.
    pub fn from_virtual_desktop(
        output: DesktopRect,
        displays: impl IntoIterator<Item = DesktopRect>,
    ) -> Result<Self, Status> {
        output.validate()?;
        let mut displays = displays.into_iter();
        let first = displays.next().ok_or(STATUS_INVALID_ARGUMENT)?;
        first.validate()?;
        let mut left = i64::from(first.left);
        let mut top = i64::from(first.top);
        let mut right = first.right_exclusive()?;
        let mut bottom = first.bottom_exclusive()?;
        let mut output_is_display = output == first;

        for display in displays {
            display.validate()?;
            output_is_display |= output == display;
            left = left.min(i64::from(display.left));
            top = top.min(i64::from(display.top));
            right = right.max(display.right_exclusive()?);
            bottom = bottom.max(display.bottom_exclusive()?);
        }
        if !output_is_display {
            return Err(STATUS_INVALID_ARGUMENT);
        }

        let virtual_desktop = DesktopRect::new(
            i32::try_from(left).map_err(|_| STATUS_INVALID_ARGUMENT)?,
            i32::try_from(top).map_err(|_| STATUS_INVALID_ARGUMENT)?,
            u32::try_from(right - left).map_err(|_| STATUS_INVALID_ARGUMENT)?,
            u32::try_from(bottom - top).map_err(|_| STATUS_INVALID_ARGUMENT)?,
        )?;
        Self::new(output, virtual_desktop)
    }

    pub fn validate(self) -> Result<(), Status> {
        self.output.validate()?;
        self.virtual_desktop.validate()?;
        if !self.virtual_desktop.contains(self.output)? {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }

    /// Convert the output's first and last physical pixels into inclusive
    /// Windows virtual-desktop absolute-pointer coordinates.
    pub fn normalized_output(self) -> Result<NormalizedDesktopRect, Status> {
        self.validate()?;
        let horizontal_span = self
            .virtual_desktop
            .width
            .checked_sub(1)
            .filter(|span| *span != 0)
            .ok_or(STATUS_INVALID_ARGUMENT)?;
        let vertical_span = self
            .virtual_desktop
            .height
            .checked_sub(1)
            .filter(|span| *span != 0)
            .ok_or(STATUS_INVALID_ARGUMENT)?;
        let output_right = self.output.right_exclusive()? - 1;
        let output_bottom = self.output.bottom_exclusive()? - 1;

        NormalizedDesktopRect::new(
            normalize_axis(
                i64::from(self.output.left),
                self.virtual_desktop.left,
                horizontal_span,
            )?,
            normalize_axis(
                i64::from(self.output.top),
                self.virtual_desktop.top,
                vertical_span,
            )?,
            normalize_axis(output_right, self.virtual_desktop.left, horizontal_span)?,
            normalize_axis(output_bottom, self.virtual_desktop.top, vertical_span)?,
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureGeometryFields {
    output: DesktopRect,
    virtual_desktop: DesktopRect,
}

impl<'de> Deserialize<'de> for CaptureGeometry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let fields = CaptureGeometryFields::deserialize(deserializer)?;
        Self::new(fields.output, fields.virtual_desktop)
            .map_err(|_| D::Error::custom("capture output is outside the virtual desktop"))
    }
}

/// Inclusive `0..=65535` endpoints for Windows virtual-desktop absolute input.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct NormalizedDesktopRect {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
}

impl NormalizedDesktopRect {
    pub fn new(left: u16, top: u16, right: u16, bottom: u16) -> Result<Self, Status> {
        if left >= right || top >= bottom {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(Self {
            left,
            top,
            right,
            bottom,
        })
    }

    pub fn validate(self) -> Result<(), Status> {
        Self::new(self.left, self.top, self.right, self.bottom).map(|_| ())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NormalizedDesktopRectFields {
    left: u16,
    top: u16,
    right: u16,
    bottom: u16,
}

impl<'de> Deserialize<'de> for NormalizedDesktopRect {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let fields = NormalizedDesktopRectFields::deserialize(deserializer)?;
        Self::new(fields.left, fields.top, fields.right, fields.bottom)
            .map_err(|_| D::Error::custom("normalized rectangle endpoints must not collapse"))
    }
}

fn checked_exclusive_end(origin: i32, extent: u32) -> Result<i64, Status> {
    if extent == 0 {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let end = i64::from(origin) + i64::from(extent);
    if end > i64::from(i32::MAX) + 1 {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    Ok(end)
}

fn normalize_axis(
    physical_pixel: i64,
    virtual_origin: i32,
    virtual_span: u32,
) -> Result<u16, Status> {
    let offset = physical_pixel
        .checked_sub(i64::from(virtual_origin))
        .and_then(|offset| u64::try_from(offset).ok())
        .filter(|offset| *offset <= u64::from(virtual_span))
        .ok_or(STATUS_INVALID_ARGUMENT)?;
    let denominator = u64::from(virtual_span);
    let normalized = offset
        .checked_mul(NORMALIZED_MAXIMUM)
        .and_then(|value| value.checked_add(denominator / 2))
        .map(|value| value / denominator)
        .and_then(|value| u16::try_from(value).ok())
        .ok_or(STATUS_INVALID_ARGUMENT)?;
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rectangle(left: i32, top: i32, width: u32, height: u32) -> DesktopRect {
        DesktopRect::new(left, top, width, height).unwrap()
    }

    #[test]
    fn c_layouts_are_fixed_width_without_hidden_pointers() {
        assert_eq!(std::mem::size_of::<DesktopRect>(), 16);
        assert_eq!(std::mem::align_of::<DesktopRect>(), 4);
        assert_eq!(std::mem::size_of::<CaptureGeometry>(), 32);
        assert_eq!(std::mem::align_of::<CaptureGeometry>(), 4);
        assert_eq!(std::mem::size_of::<NormalizedDesktopRect>(), 8);
        assert_eq!(std::mem::align_of::<NormalizedDesktopRect>(), 2);
    }

    #[test]
    fn bounds_negative_origins_and_contains_the_capture() {
        let captured = rectangle(-1920, 0, 1920, 1080);
        let geometry = CaptureGeometry::from_virtual_desktop(
            captured,
            [captured, rectangle(0, -200, 2560, 1440)],
        )
        .unwrap();
        assert_eq!(geometry.virtual_desktop, rectangle(-1920, -200, 4480, 1440));
        assert_eq!(
            CaptureGeometry::new(captured, geometry.virtual_desktop),
            Ok(geometry)
        );
    }

    #[test]
    fn constructors_reject_empty_overflow_and_outside_rectangles() {
        assert!(DesktopRect::new(0, 0, 0, 1).is_err());
        assert!(DesktopRect::new(i32::MAX, 0, 1, 1).is_ok());
        assert!(DesktopRect::new(i32::MAX, 0, 2, 1).is_err());
        assert!(DesktopRect::new(0, i32::MAX, 1, 2).is_err());
        let lowest = rectangle(i32::MIN, 0, 1, 1);
        let highest = rectangle(i32::MAX, 0, 1, 1);
        assert!(CaptureGeometry::from_virtual_desktop(lowest, [lowest, highest]).is_err());

        let output = rectangle(0, 0, 1920, 1080);
        assert!(CaptureGeometry::from_virtual_desktop(output, []).is_err());
        assert!(CaptureGeometry::new(output, rectangle(1, 0, 1920, 1080)).is_err());
        assert!(
            CaptureGeometry::from_virtual_desktop(
                rectangle(0, 0, 10, 100),
                [rectangle(-100, 0, 50, 100), rectangle(50, 0, 50, 100)]
            )
            .is_err()
        );
        assert!(
            CaptureGeometry::from_virtual_desktop(
                output,
                [DesktopRect {
                    left: 0,
                    top: 0,
                    width: 1,
                    height: 0,
                }]
            )
            .is_err()
        );
    }

    #[test]
    fn normalized_output_uses_physical_last_pixels_and_negative_origins() {
        let geometry = CaptureGeometry::new(
            rectangle(-50, -100, 101, 201),
            rectangle(-100, -200, 201, 401),
        )
        .unwrap();
        assert_eq!(
            geometry.normalized_output(),
            Ok(NormalizedDesktopRect {
                left: 16384,
                top: 16384,
                right: 49151,
                bottom: 49151,
            })
        );
        assert_eq!(
            CaptureGeometry::new(
                rectangle(-100, -200, 201, 401),
                rectangle(-100, -200, 201, 401),
            )
            .unwrap()
            .normalized_output(),
            NormalizedDesktopRect::new(0, 0, u16::MAX, u16::MAX)
        );
    }

    #[test]
    fn normalized_output_rejects_degenerate_or_precision_collapsed_axes() {
        let one_pixel_axis =
            CaptureGeometry::new(rectangle(0, 0, 1, 2), rectangle(0, 0, 1, 2)).unwrap();
        assert!(one_pixel_axis.normalized_output().is_err());

        let collapsed_output =
            CaptureGeometry::new(rectangle(0, 0, 2, 2), rectangle(0, 0, 200_000, 200_000)).unwrap();
        assert!(collapsed_output.normalized_output().is_err());
        assert!(NormalizedDesktopRect::new(1, 2, 1, 3).is_err());
    }

    #[test]
    fn json_round_trip_is_strict_and_validated() {
        let geometry = CaptureGeometry::new(
            rectangle(-1920, 0, 1920, 1080),
            rectangle(-1920, -200, 4480, 1440),
        )
        .unwrap();
        let value = serde_json::to_value(geometry).unwrap();
        assert_eq!(
            serde_json::from_value::<CaptureGeometry>(value).unwrap(),
            geometry
        );

        for invalid in [
            json!({
                "output":{"left":0,"top":0,"width":0,"height":1},
                "virtual_desktop":{"left":0,"top":0,"width":1,"height":1}
            }),
            json!({
                "output":{"left":0,"top":0,"width":1,"height":1,"unknown":true},
                "virtual_desktop":{"left":0,"top":0,"width":1,"height":1}
            }),
            json!({
                "output":{"left":-1,"top":0,"width":2,"height":1},
                "virtual_desktop":{"left":0,"top":0,"width":1,"height":1}
            }),
        ] {
            assert!(serde_json::from_value::<CaptureGeometry>(invalid).is_err());
        }
    }
}
