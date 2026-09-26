//! CPU composition of a separate DXGI pointer into a tight, oriented BGRA frame.

use crate::model::{CaptureError, CaptureResult, frame_layout};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    Color,
    MaskedColor,
    Monochrome,
}

#[derive(Clone, Debug)]
pub struct Shape {
    pub kind: ShapeKind,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Default)]
pub struct CursorState {
    pub shape: Option<Shape>,
    pub position: Option<(i32, i32)>,
}

impl CursorState {
    /// A missing update retains the last shape/position for move-only frames.
    /// `Some(None)` means DXGI says the pointer is in the desktop pixels or hidden.
    pub fn update(&mut self, shape: Option<Shape>, position: Option<Option<(i32, i32)>>) {
        if let Some(shape) = shape {
            self.shape = Some(shape);
        }
        if let Some(position) = position {
            self.position = position;
        }
    }

    pub fn snapshot(&self) -> Option<(Shape, i32, i32)> {
        self.position
            .and_then(|(x, y)| self.shape.clone().map(|shape| (shape, x, y)))
    }
}

impl Shape {
    pub fn new(
        kind: ShapeKind,
        width: u32,
        height: u32,
        pitch: u32,
        bytes: Vec<u8>,
    ) -> CaptureResult<Self> {
        let row_bytes = match kind {
            ShapeKind::Monochrome => width.div_ceil(8),
            _ => width.checked_mul(4).ok_or_else(invalid_shape)?,
        };
        let required = (pitch as usize)
            .checked_mul(height as usize)
            .ok_or_else(invalid_shape)?;
        if width == 0
            || height == 0
            || width > 8192
            || height > 8192
            || pitch < row_bytes
            || required != bytes.len()
            || required > 2 * 1024 * 1024
            || (kind == ShapeKind::Monochrome && !height.is_multiple_of(2))
        {
            return Err(invalid_shape());
        }
        if kind == ShapeKind::MaskedColor
            && (0..height as usize).any(|row| {
                (0..width as usize)
                    .any(|column| !matches!(bytes[row * pitch as usize + column * 4 + 3], 0 | 255))
            })
        {
            return Err(invalid_shape());
        }
        Ok(Self {
            kind,
            width,
            height,
            pitch,
            bytes,
        })
    }

    pub fn compose(
        &self,
        frame: &mut [u8],
        width: u32,
        height: u32,
        left: i32,
        top: i32,
    ) -> CaptureResult<()> {
        let (_, length) = frame_layout(width, height)?;
        if frame.len() != length {
            return Err(invalid_shape());
        }
        let shape_height = if self.kind == ShapeKind::Monochrome {
            self.height / 2
        } else {
            self.height
        };
        let x0 = i64::from(left).max(0);
        let y0 = i64::from(top).max(0);
        let x1 = (i64::from(left) + i64::from(self.width)).min(i64::from(width));
        let y1 = (i64::from(top) + i64::from(shape_height)).min(i64::from(height));
        for y in y0..y1 {
            for x in x0..x1 {
                let sx = (x - i64::from(left)) as usize;
                let sy = (y - i64::from(top)) as usize;
                let dst = ((y as usize * width as usize) + x as usize) * 4;
                let pixel = &mut frame[dst..dst + 4];
                match self.kind {
                    ShapeKind::Color => {
                        let src = sy * self.pitch as usize + sx * 4;
                        let color = &self.bytes[src..src + 4];
                        let alpha = u32::from(color[3]);
                        for channel in 0..3 {
                            pixel[channel] = ((u32::from(color[channel]) * alpha
                                + u32::from(pixel[channel]) * (255 - alpha)
                                + 127)
                                / 255) as u8;
                        }
                    }
                    ShapeKind::MaskedColor => {
                        let src = sy * self.pitch as usize + sx * 4;
                        let color = &self.bytes[src..src + 4];
                        if color[3] == 0 {
                            pixel[..3].copy_from_slice(&color[..3]);
                        } else if color[3] == 255 {
                            for channel in 0..3 {
                                pixel[channel] ^= color[channel];
                            }
                        }
                    }
                    ShapeKind::Monochrome => {
                        let bit = 0x80 >> (sx % 8);
                        let and_mask = self.bytes[sy * self.pitch as usize + sx / 8] & bit != 0;
                        let xor_mask = self.bytes
                            [(sy + shape_height as usize) * self.pitch as usize + sx / 8]
                            & bit
                            != 0;
                        for channel in &mut pixel[..3] {
                            *channel = (if and_mask { *channel } else { 0 })
                                ^ if xor_mask { 255 } else { 0 };
                        }
                    }
                }
                pixel[3] = 255;
            }
        }
        Ok(())
    }
}

fn invalid_shape() -> CaptureError {
    CaptureError::new("invalid_frame", "DXGI returned an invalid pointer shape")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_alpha_and_clipping() {
        let shape = Shape::new(
            ShapeKind::Color,
            2,
            1,
            12,
            vec![200, 0, 0, 128, 0, 0, 200, 255, 99, 99, 99, 99],
        )
        .unwrap();
        let mut frame = vec![0, 0, 100, 255, 0, 0, 100, 255];
        shape.compose(&mut frame, 2, 1, -1, 0).unwrap();
        assert_eq!(frame, vec![0, 0, 200, 255, 0, 0, 100, 255]);
    }

    #[test]
    fn masked_color_replaces_or_xors() {
        let shape = Shape::new(
            ShapeKind::MaskedColor,
            2,
            1,
            8,
            vec![1, 2, 3, 0, 0xff, 0, 0x55, 255],
        )
        .unwrap();
        let mut frame = vec![10, 20, 30, 255, 0x12, 20, 0x55, 255];
        shape.compose(&mut frame, 2, 1, 0, 0).unwrap();
        assert_eq!(frame, vec![1, 2, 3, 255, 0xed, 20, 0, 255]);
    }

    #[test]
    fn monochrome_mask_halves_are_not_display_rows() {
        let shape =
            Shape::new(ShapeKind::Monochrome, 2, 2, 2, vec![0x80, 0xff, 0x40, 0xff]).unwrap();
        let mut frame = vec![20, 40, 60, 255, 20, 40, 60, 255];
        shape.compose(&mut frame, 2, 1, 0, 0).unwrap();
        assert_eq!(frame, vec![20, 40, 60, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn invalid_layout_never_reads_padding_or_partial_rows() {
        assert!(Shape::new(ShapeKind::Color, 2, 1, 7, vec![0; 7]).is_err());
        assert!(Shape::new(ShapeKind::Color, 2, 1, 8, vec![0; 7]).is_err());
        assert!(Shape::new(ShapeKind::Monochrome, 2, 3, 1, vec![0; 3]).is_err());
        assert!(Shape::new(ShapeKind::MaskedColor, 1, 1, 4, vec![0, 0, 0, 1]).is_err());
    }

    #[test]
    fn cursor_position_is_in_oriented_output_coordinates() {
        // A 90-degree DXGI surface is 3x2; output is 2x3.
        let surface: Vec<u8> = (1..=6).flat_map(|value| [value, 0, 0, 255]).collect();
        let mut output =
            unsafe { crate::model::orient_bgra_rows(3, 2, 12, surface.as_ptr(), 2, 3, 2).unwrap() };
        let shape = Shape::new(ShapeKind::Color, 1, 1, 4, vec![99, 0, 0, 255]).unwrap();
        shape.compose(&mut output, 2, 3, 1, 2).unwrap();
        assert_eq!(&output[0..4], &[4, 0, 0, 255]);
        assert_eq!(&output[20..24], &[99, 0, 0, 255]);
    }

    #[test]
    fn move_only_hidden_and_new_session_do_not_reuse_stale_pixels() {
        let shape = Shape::new(ShapeKind::Color, 1, 1, 4, vec![1, 2, 3, 255]).unwrap();
        let mut state = CursorState::default();
        state.update(None, Some(Some((0, 0))));
        assert!(state.snapshot().is_none()); // Visible before a known shape is safe.
        state.update(Some(shape), Some(Some((0, 0))));
        let pending_snapshot = state.snapshot().unwrap();
        let replacement = Shape::new(ShapeKind::Color, 1, 1, 4, vec![4, 5, 6, 255]).unwrap();
        state.update(Some(replacement), None);
        assert_eq!(state.snapshot().unwrap().0.bytes, vec![4, 5, 6, 255]);
        state.update(None, Some(Some((1, 0))));
        assert_eq!(state.snapshot().unwrap().1, 1);
        assert_eq!(pending_snapshot.1, 0); // Pending Map retry keeps its own position.
        state.update(None, Some(None));
        assert!(state.snapshot().is_none());
        assert!(CursorState::default().snapshot().is_none()); // Reopen clears cache.
    }
}
