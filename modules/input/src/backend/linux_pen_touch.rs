//! Linux pen and touch injection through lease-owned UInput devices.
//!
//! The virtual axes use normalized 0..65535 desktop coordinates. Operation
//! requires a writable `/dev/uinput` device (typically a udev rule or uinput
//! group membership) and a desktop compositor that accepts direct virtual
//! touchscreen devices. Compositor calibration and physical pen/touch behavior
//! remain platform qualification gates.

use super::{
    ABS_MT_ORIENTATION, ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_PRESSURE, ABS_MT_SLOT,
    ABS_MT_TOUCH_MAJOR, ABS_MT_TOUCH_MINOR, ABS_MT_TRACKING_ID, ABS_PRESSURE, ABS_TILT_X,
    ABS_TILT_Y, ABS_X, ABS_Y, BTN_STYLUS, BTN_STYLUS2, BTN_TOOL_PEN, BTN_TOOL_RUBBER, BTN_TOUCH,
    Device, EV_ABS, EV_KEY, InputError, InputResult, MAX_TOUCH_SLOTS,
};
use crate::model::{
    Event, PEN_BUTTON_PRIMARY, PEN_BUTTON_SECONDARY, PEN_BUTTON_TERTIARY, PEN_POINTER_ID, PenTool,
    PenTouchDevice, PenTouchPhase, pen_tilt_axes,
};
use std::collections::BTreeMap;

const MAX_PRESSURE: i32 = u16::MAX as i32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Pen,
    Touch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TouchSlot {
    slot: i32,
    tracking_id: i32,
}

#[derive(Clone)]
struct TrackedContact {
    initial_down: Event,
    last_event: Event,
    down_written: bool,
}

#[derive(Clone, Copy)]
struct PenMetadata {
    tool: PenTool,
    buttons: u8,
}

#[derive(Clone)]
struct TrackedPen {
    contact: TrackedContact,
    initial_metadata: PenMetadata,
    metadata: PenMetadata,
}

#[derive(Clone)]
struct TrackedTouch {
    slot: TouchSlot,
    contact: TrackedContact,
}

#[derive(Default)]
struct ContactTracker {
    pen: Option<TrackedPen>,
    touches: BTreeMap<u32, TrackedTouch>,
    next_tracking_id: i32,
}

impl ContactTracker {
    fn inject(
        &mut self,
        event: &Event,
        mut emit: impl FnMut(Target, &[(u16, u16, i32)]) -> InputResult<()>,
    ) -> InputResult<()> {
        event.validate()?;
        let Event::PenTouch {
            pointer_id,
            device,
            phase,
            ..
        } = event
        else {
            return Err(InputError::new(
                "unsupported_event",
                "Linux pen/touch transport received a non pen/touch event",
            ));
        };

        match device {
            PenTouchDevice::Pen => self.inject_pen(*pointer_id, *phase, event, &mut emit),
            PenTouchDevice::Touch => self.inject_touch(*pointer_id, *phase, event, &mut emit),
        }
    }

    fn inject_pen(
        &mut self,
        pointer_id: u32,
        phase: PenTouchPhase,
        event: &Event,
        emit: &mut impl FnMut(Target, &[(u16, u16, i32)]) -> InputResult<()>,
    ) -> InputResult<()> {
        let active_pointer = self
            .pen
            .as_ref()
            .and_then(|pen| pointer_id_of(&pen.contact.initial_down));
        let frame_metadata = match phase {
            PenTouchPhase::Down => match active_pointer {
                Some(active) if active == pointer_id => {
                    let pen = self.pen.as_ref().expect("active pen has a contact");
                    if pen.contact.initial_down != *event {
                        return Err(duplicate_down_mismatch());
                    }
                    if pen.contact.down_written {
                        return Ok(());
                    }
                    pen.initial_metadata
                }
                Some(_) => {
                    return Err(InputError::new(
                        "contact_capacity",
                        "Linux pen uinput supports one active pen pointer",
                    ));
                }
                None => {
                    let metadata = initial_pen_metadata(event);
                    self.pen = Some(TrackedPen {
                        contact: TrackedContact {
                            initial_down: event.clone(),
                            last_event: event.clone(),
                            down_written: false,
                        },
                        initial_metadata: metadata,
                        metadata,
                    });
                    metadata
                }
            },
            PenTouchPhase::Move => {
                ensure_active_pointer(active_pointer, pointer_id)?;
                let pen = self.pen.as_mut().expect("active pen has a contact");
                pen.contact.last_event = event.clone();
                update_pen_metadata(&mut pen.metadata, event);
                pen.metadata
            }
            PenTouchPhase::Up | PenTouchPhase::Cancel => {
                ensure_active_pointer(active_pointer, pointer_id)?;
                self.pen
                    .as_mut()
                    .expect("active pen has a contact")
                    .contact
                    .last_event = event.clone();
                self.pen
                    .as_ref()
                    .expect("active pen has a contact")
                    .metadata
            }
        };

        let frame = pen_frame(event, frame_metadata)?;
        emit(Target::Pen, &frame)?;
        match phase {
            PenTouchPhase::Down => {
                self.pen
                    .as_mut()
                    .expect("successful pen down retains its contact")
                    .contact
                    .down_written = true;
            }
            PenTouchPhase::Move => {}
            PenTouchPhase::Up | PenTouchPhase::Cancel => self.pen = None,
        }
        Ok(())
    }

    fn inject_touch(
        &mut self,
        pointer_id: u32,
        phase: PenTouchPhase,
        event: &Event,
        emit: &mut impl FnMut(Target, &[(u16, u16, i32)]) -> InputResult<()>,
    ) -> InputResult<()> {
        match phase {
            PenTouchPhase::Down => {
                if let Some(touch) = self.touches.get(&pointer_id) {
                    if touch.contact.initial_down != *event {
                        return Err(duplicate_down_mismatch());
                    }
                    if touch.contact.down_written {
                        return Ok(());
                    }
                } else {
                    if self.touches.len() >= usize::from(MAX_TOUCH_SLOTS) {
                        return Err(InputError::new(
                            "contact_capacity",
                            "Linux touchscreen uinput supports at most 10 simultaneous contacts",
                        ));
                    }
                    let slot = self.allocate_touch_slot()?;
                    // Reserve the exact client identity before writing: a partial
                    // uinput frame can reach the kernel before write reports failure.
                    self.touches.insert(
                        pointer_id,
                        TrackedTouch {
                            slot,
                            contact: TrackedContact {
                                initial_down: event.clone(),
                                last_event: event.clone(),
                                down_written: false,
                            },
                        },
                    );
                }
            }
            PenTouchPhase::Move => {
                let Some(touch) = self.touches.get_mut(&pointer_id) else {
                    return Err(pointer_not_active());
                };
                touch.contact.last_event = event.clone();
            }
            PenTouchPhase::Up | PenTouchPhase::Cancel => {
                let Some(touch) = self.touches.get_mut(&pointer_id) else {
                    return Err(pointer_not_active());
                };
                touch.contact.last_event = event.clone();
            }
        }

        let slot = self
            .touches
            .get(&pointer_id)
            .map(|touch| touch.slot)
            .ok_or_else(pointer_not_active)?;
        let frame = touch_frame(event, slot, self.touches.len())?;
        emit(Target::Touch, &frame)?;
        match phase {
            PenTouchPhase::Down => {
                self.touches
                    .get_mut(&pointer_id)
                    .expect("successful touch down retains its contact")
                    .contact
                    .down_written = true;
            }
            PenTouchPhase::Move => {}
            PenTouchPhase::Up | PenTouchPhase::Cancel => {
                self.touches.remove(&pointer_id);
            }
        }
        Ok(())
    }

    fn allocate_touch_slot(&mut self) -> InputResult<TouchSlot> {
        let occupied_slots = self
            .touches
            .values()
            .map(|touch| touch.slot.slot)
            .collect::<Vec<_>>();
        let slot = (0..i32::from(MAX_TOUCH_SLOTS))
            .find(|slot| !occupied_slots.contains(slot))
            .ok_or_else(|| {
                InputError::new(
                    "contact_capacity",
                    "Linux touchscreen uinput has no free contact slot",
                )
            })?;

        let tracking_id = self.allocate_tracking_id();
        Ok(TouchSlot { slot, tracking_id })
    }

    fn allocate_tracking_id(&mut self) -> i32 {
        let mut candidate = self.next_tracking_id.max(0);
        loop {
            self.next_tracking_id = if candidate == i32::MAX {
                0
            } else {
                candidate + 1
            };
            if self
                .touches
                .values()
                .all(|touch| touch.slot.tracking_id != candidate)
            {
                return candidate;
            }
            candidate = self.next_tracking_id;
        }
    }

    fn release_all(
        &mut self,
        mut emit: impl FnMut(Target, &[(u16, u16, i32)]) -> InputResult<()>,
    ) -> InputResult<()> {
        let mut first_error = None;
        if let Some(pen) = self.pen.clone() {
            let cancel_event = with_phase(&pen.contact.last_event, PenTouchPhase::Cancel);
            let frame = pen_frame(&cancel_event, pen.metadata)?;
            match emit(Target::Pen, &frame) {
                Ok(()) => self.pen = None,
                Err(error) => first_error = Some(error),
            }
        }

        for pointer_id in self.touches.keys().copied().collect::<Vec<_>>() {
            let Some(touch) = self.touches.get(&pointer_id).cloned() else {
                continue;
            };
            let cancel_event = with_phase(&touch.contact.last_event, PenTouchPhase::Cancel);
            let frame = touch_frame(&cancel_event, touch.slot, self.touches.len())?;
            match emit(Target::Touch, &frame) {
                Ok(()) => {
                    self.touches.remove(&pointer_id);
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }

        first_error.map_or(Ok(()), Err)
    }

    fn clear_target(&mut self, target: Target) {
        match target {
            Target::Pen => self.pen = None,
            Target::Touch => self.touches.clear(),
        }
    }

    fn is_empty(&self) -> bool {
        self.pen.is_none() && self.touches.is_empty()
    }
}

pub(super) struct LinuxPenTouch {
    tracker: ContactTracker,
    pen_device: Option<Device>,
    touch_device: Option<Device>,
}

impl LinuxPenTouch {
    pub(super) fn new() -> Self {
        Self {
            tracker: ContactTracker::default(),
            pen_device: None,
            touch_device: None,
        }
    }

    pub(super) fn is_idle(&self) -> bool {
        self.tracker.is_empty() && self.pen_device.is_none() && self.touch_device.is_none()
    }

    pub(super) fn inject(&mut self, path: &str, event: &Event) -> InputResult<()> {
        let Self {
            tracker,
            pen_device,
            touch_device,
        } = self;
        tracker.inject(event, |target, frame| {
            let device = match target {
                Target::Pen => &mut *pen_device,
                Target::Touch => &mut *touch_device,
            };
            if device.is_none() {
                *device = Some(match target {
                    Target::Pen => Device::create_pen(path)?,
                    Target::Touch => Device::create_touchscreen(path)?,
                });
            }
            device
                .as_ref()
                .ok_or_else(|| InputError::new("backend_inactive", "uinput device is not active"))?
                .emit(frame)
        })
    }

    pub(super) fn stop(&mut self) -> InputResult<()> {
        let Self {
            tracker,
            pen_device,
            touch_device,
        } = self;
        // Send per-pointer terminal frames first. If a write fails, destroying
        // our owned devices still removes all contacts from those devices.
        let _release_result = tracker.release_all(|target, frame| {
            let device = match target {
                Target::Pen => pen_device.as_ref(),
                Target::Touch => touch_device.as_ref(),
            };
            match device {
                Some(device) => device.emit(frame),
                // A Down can fail while opening UInput before any device
                // exists; there is then no kernel contact to release.
                None => Ok(()),
            }
        });

        let pen_destroy = destroy_device(pen_device);
        if pen_destroy.is_ok() {
            tracker.clear_target(Target::Pen);
        }
        let touch_destroy = destroy_device(touch_device);
        if touch_destroy.is_ok() {
            tracker.clear_target(Target::Touch);
        }

        pen_destroy?;
        touch_destroy
    }
}

fn destroy_device(device: &mut Option<Device>) -> InputResult<()> {
    if let Some(device) = device {
        device.destroy()?;
    }
    *device = None;
    Ok(())
}

fn duplicate_down_mismatch() -> InputError {
    InputError::new(
        "invalid_sequence",
        "duplicate pen/touch down must match the original pointer event",
    )
}

fn pointer_id_of(event: &Event) -> Option<u32> {
    match event {
        Event::PenTouch { pointer_id, .. } => Some(*pointer_id),
        _ => None,
    }
}

fn ensure_active_pointer(active_pointer: Option<u32>, pointer_id: u32) -> InputResult<()> {
    if active_pointer == Some(pointer_id) {
        Ok(())
    } else {
        Err(pointer_not_active())
    }
}

fn pointer_not_active() -> InputError {
    InputError::new(
        "invalid_sequence",
        "pen/touch event requires an active pointer with the same identity",
    )
}

fn initial_pen_metadata(event: &Event) -> PenMetadata {
    let Event::PenTouch {
        pen_tool,
        pen_buttons,
        ..
    } = event
    else {
        return PenMetadata {
            tool: PenTool::Pen,
            buttons: 0,
        };
    };
    PenMetadata {
        tool: match pen_tool {
            Some(PenTool::Eraser) => PenTool::Eraser,
            Some(PenTool::Pen | PenTool::Unknown) | None => PenTool::Pen,
        },
        buttons: pen_buttons.unwrap_or(0),
    }
}

fn update_pen_metadata(metadata: &mut PenMetadata, event: &Event) {
    let Event::PenTouch {
        pen_tool,
        pen_buttons,
        ..
    } = event
    else {
        return;
    };
    match pen_tool {
        Some(PenTool::Pen) => metadata.tool = PenTool::Pen,
        Some(PenTool::Eraser) => metadata.tool = PenTool::Eraser,
        Some(PenTool::Unknown) | None => {}
    }
    if let Some(buttons) = pen_buttons {
        metadata.buttons = *buttons;
    }
}

fn pen_frame(event: &Event, metadata: PenMetadata) -> InputResult<Vec<(u16, u16, i32)>> {
    let Event::PenTouch {
        phase,
        x,
        y,
        pressure,
        tilt,
        rotation_degrees,
        ..
    } = event
    else {
        return Err(InputError::new(
            "unsupported_event",
            "Linux pen transport received a non pen/touch event",
        ));
    };
    let mut frame = vec![
        (EV_ABS, ABS_X, i32::from(*x)),
        (EV_ABS, ABS_Y, i32::from(*y)),
    ];
    match phase {
        PenTouchPhase::Up | PenTouchPhase::Cancel => frame.push((EV_ABS, ABS_PRESSURE, 0)),
        PenTouchPhase::Down | PenTouchPhase::Move => {
            if let Some(pressure) = pressure {
                frame.push((EV_ABS, ABS_PRESSURE, pressure_value(*pressure)));
            } else if *phase == PenTouchPhase::Down {
                frame.push((EV_ABS, ABS_PRESSURE, 0));
            }
        }
    }
    if let Some((tilt_x, tilt_y)) = pen_tilt_axes(*tilt, *rotation_degrees) {
        frame.push((EV_ABS, ABS_TILT_X, tilt_x.round() as i32));
        frame.push((EV_ABS, ABS_TILT_Y, tilt_y.round() as i32));
    } else {
        // Linux has no unknown-tilt sentinel; neutralize both axes so a missing
        // sample cannot leave a previous move's tilt stuck in the device.
        frame.push((EV_ABS, ABS_TILT_X, 0));
        frame.push((EV_ABS, ABS_TILT_Y, 0));
    }
    if matches!(phase, PenTouchPhase::Down | PenTouchPhase::Move) {
        frame.push((
            EV_KEY,
            BTN_TOOL_PEN,
            i32::from(metadata.tool != PenTool::Eraser),
        ));
        frame.push((
            EV_KEY,
            BTN_TOOL_RUBBER,
            i32::from(metadata.tool == PenTool::Eraser),
        ));
        // The contact lifecycle presses the tip; the protocol primary bit maps
        // to the same Linux tip key. The two barrel buttons have distinct keys.
        let tip_pressed = metadata.buttons & PEN_BUTTON_PRIMARY != 0
            || *phase == PenTouchPhase::Down
            || *phase == PenTouchPhase::Move;
        frame.push((EV_KEY, BTN_TOUCH, i32::from(tip_pressed)));
        frame.push((
            EV_KEY,
            BTN_STYLUS,
            i32::from(metadata.buttons & PEN_BUTTON_SECONDARY != 0),
        ));
        frame.push((
            EV_KEY,
            BTN_STYLUS2,
            i32::from(metadata.buttons & PEN_BUTTON_TERTIARY != 0),
        ));
    } else {
        for code in [
            BTN_TOUCH,
            BTN_TOOL_PEN,
            BTN_TOOL_RUBBER,
            BTN_STYLUS,
            BTN_STYLUS2,
        ] {
            frame.push((EV_KEY, code, 0));
        }
    }
    Ok(frame)
}

fn touch_frame(
    event: &Event,
    slot: TouchSlot,
    active_contacts: usize,
) -> InputResult<Vec<(u16, u16, i32)>> {
    let Event::PenTouch {
        device: PenTouchDevice::Touch,
        phase,
        x,
        y,
        pressure,
        contact_area_major,
        contact_area_minor,
        rotation_degrees,
        ..
    } = event
    else {
        return Err(InputError::new(
            "unsupported_event",
            "Linux touchscreen transport received a non touch event",
        ));
    };
    let mut frame = vec![(EV_ABS, ABS_MT_SLOT, slot.slot)];
    match phase {
        PenTouchPhase::Down => frame.push((EV_ABS, ABS_MT_TRACKING_ID, slot.tracking_id)),
        PenTouchPhase::Move => {}
        PenTouchPhase::Up | PenTouchPhase::Cancel => {
            frame.push((EV_ABS, ABS_MT_TRACKING_ID, -1));
        }
    }
    if matches!(phase, PenTouchPhase::Down | PenTouchPhase::Move) {
        frame.push((EV_ABS, ABS_MT_POSITION_X, i32::from(*x)));
        frame.push((EV_ABS, ABS_MT_POSITION_Y, i32::from(*y)));
        if let Some(pressure) = pressure {
            frame.push((EV_ABS, ABS_MT_PRESSURE, pressure_value(*pressure)));
        } else if *phase == PenTouchPhase::Down {
            frame.push((EV_ABS, ABS_MT_PRESSURE, 0));
        }
        if let Some(area) = contact_area_major {
            frame.push((EV_ABS, ABS_MT_TOUCH_MAJOR, normalized_axis_value(*area)));
            frame.push((
                EV_ABS,
                ABS_MT_TOUCH_MINOR,
                normalized_axis_value(contact_area_minor.unwrap_or_default()),
            ));
        } else if *phase == PenTouchPhase::Down {
            frame.push((EV_ABS, ABS_MT_TOUCH_MAJOR, 0));
            frame.push((EV_ABS, ABS_MT_TOUCH_MINOR, 0));
        }
        if let Some(rotation) = rotation_degrees {
            frame.push((EV_ABS, ABS_MT_ORIENTATION, touch_orientation(*rotation)));
        } else {
            // ABS_MT_ORIENTATION has no unknown value; zero is the neutral angle.
            frame.push((EV_ABS, ABS_MT_ORIENTATION, 0));
        }
        if *phase == PenTouchPhase::Down && active_contacts == 1 {
            frame.push((EV_KEY, BTN_TOUCH, 1));
        }
    } else {
        frame.extend([
            (EV_ABS, ABS_MT_PRESSURE, 0),
            (EV_ABS, ABS_MT_TOUCH_MAJOR, 0),
            (EV_ABS, ABS_MT_TOUCH_MINOR, 0),
            (EV_ABS, ABS_MT_ORIENTATION, 0),
        ]);
        if active_contacts == 1 {
            // A touch key is shared by the touchscreen device, so only clear it
            // when the last tracked contact is being terminated.
            frame.push((EV_KEY, BTN_TOUCH, 0));
        }
    }
    Ok(frame)
}

fn normalized_axis_value(value: f32) -> i32 {
    (value * MAX_PRESSURE as f32).round() as i32
}

fn pressure_value(pressure: f32) -> i32 {
    normalized_axis_value(pressure)
}

fn touch_orientation(rotation_degrees: u16) -> i32 {
    // Match the pinned Linux inputtino convention: fold the ellipse's 0..360
    // rotation into its equivalent signed angle in the [-90, 90] range.
    let mut orientation = i32::from(rotation_degrees);
    if orientation > 90 && orientation < 270 {
        orientation = 180 - orientation;
    }
    if orientation > 90 {
        orientation -= 360;
    } else if orientation < -90 {
        orientation += 360;
    }
    orientation
}

fn with_phase(event: &Event, phase: PenTouchPhase) -> Event {
    match event {
        Event::PenTouch {
            pointer_id,
            device,
            x,
            y,
            pressure,
            contact_area_major,
            contact_area_minor,
            rotation_degrees,
            pen_tool,
            pen_buttons,
            tilt,
            ..
        } => Event::PenTouch {
            pointer_id: *pointer_id,
            device: *device,
            phase,
            x: *x,
            y: *y,
            pressure: *pressure,
            contact_area_major: *contact_area_major,
            contact_area_minor: *contact_area_minor,
            rotation_degrees: *rotation_degrees,
            pen_tool: *pen_tool,
            pen_buttons: *pen_buttons,
            tilt: *tilt,
        },
        _ => event.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type RecordedFrame = (Target, Vec<(u16, u16, i32)>);

    #[derive(Default)]
    struct FakeWriter {
        frames: Vec<RecordedFrame>,
        fail_next: bool,
    }

    impl FakeWriter {
        fn emit(&mut self, target: Target, frame: &[(u16, u16, i32)]) -> InputResult<()> {
            self.frames.push((target, frame.to_vec()));
            if self.fail_next {
                self.fail_next = false;
                Err(InputError::new("injection_failed", "fake device loss"))
            } else {
                Ok(())
            }
        }
    }

    fn event(
        pointer_id: u32,
        device: PenTouchDevice,
        phase: PenTouchPhase,
        x: u16,
        y: u16,
    ) -> Event {
        Event::PenTouch {
            pointer_id,
            device,
            phase,
            x,
            y,
            pressure: Some(0.5),
            contact_area_major: None,
            contact_area_minor: None,
            rotation_degrees: None,
            pen_tool: None,
            pen_buttons: None,
            tilt: None,
        }
    }

    fn emit(
        tracker: &mut ContactTracker,
        writer: &mut FakeWriter,
        event: &Event,
    ) -> InputResult<()> {
        tracker.inject(event, |target, frame| writer.emit(target, frame))
    }

    fn event_at(frame: &[(u16, u16, i32)], kind: u16, code: u16) -> Option<i32> {
        frame.iter().find_map(|(event_kind, event_code, value)| {
            (*event_kind == kind && *event_code == code).then_some(*value)
        })
    }

    #[test]
    fn touch_ids_retain_slots_and_terminal_events_only_release_the_requested_contact() {
        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter::default();
        emit(
            &mut tracker,
            &mut writer,
            &event(41, PenTouchDevice::Touch, PenTouchPhase::Down, 10, 20),
        )
        .unwrap();
        emit(
            &mut tracker,
            &mut writer,
            &event(99, PenTouchDevice::Touch, PenTouchPhase::Down, 30, 40),
        )
        .unwrap();
        assert_eq!(event_at(&writer.frames[0].1, EV_ABS, ABS_MT_SLOT), Some(0));
        assert_eq!(event_at(&writer.frames[1].1, EV_ABS, ABS_MT_SLOT), Some(1));

        emit(
            &mut tracker,
            &mut writer,
            &event(41, PenTouchDevice::Touch, PenTouchPhase::Cancel, 11, 21),
        )
        .unwrap();
        let cancel = &writer.frames[2].1;
        assert_eq!(event_at(cancel, EV_ABS, ABS_MT_SLOT), Some(0));
        assert_eq!(event_at(cancel, EV_ABS, ABS_MT_TRACKING_ID), Some(-1));
        assert_eq!(event_at(cancel, EV_KEY, BTN_TOUCH), None);
        assert!(!tracker.touches.contains_key(&41));
        assert_eq!(
            tracker.touches.get(&99).map(|touch| touch.slot.slot),
            Some(1)
        );

        emit(
            &mut tracker,
            &mut writer,
            &event(99, PenTouchDevice::Touch, PenTouchPhase::Move, 31, 41),
        )
        .unwrap();
        let moved = &writer.frames[3].1;
        assert_eq!(event_at(moved, EV_ABS, ABS_MT_SLOT), Some(1));
        assert_eq!(event_at(moved, EV_ABS, ABS_MT_POSITION_X), Some(31));
        emit(
            &mut tracker,
            &mut writer,
            &event(99, PenTouchDevice::Touch, PenTouchPhase::Up, 31, 41),
        )
        .unwrap();
        assert_eq!(event_at(&writer.frames[4].1, EV_KEY, BTN_TOUCH), Some(0));
        assert!(tracker.touches.is_empty());
    }

    #[test]
    fn duplicate_down_and_terminal_records_are_idempotent_for_other_contacts() {
        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter::default();
        let first = event(1, PenTouchDevice::Touch, PenTouchPhase::Down, 0, 0);
        let second = event(2, PenTouchDevice::Touch, PenTouchPhase::Down, 1, 1);
        emit(&mut tracker, &mut writer, &first).unwrap();
        emit(&mut tracker, &mut writer, &second).unwrap();
        emit(&mut tracker, &mut writer, &second).unwrap();
        assert_eq!(writer.frames.len(), 2);

        let up = event(1, PenTouchDevice::Touch, PenTouchPhase::Up, 0, 0);
        emit(&mut tracker, &mut writer, &up).unwrap();
        assert_eq!(
            emit(&mut tracker, &mut writer, &up).unwrap_err().code,
            "invalid_sequence"
        );
        assert_eq!(writer.frames.len(), 3);
        assert_eq!(event_at(&writer.frames[2].1, EV_ABS, ABS_MT_SLOT), Some(0));
        assert_eq!(
            event_at(&writer.frames[2].1, EV_ABS, ABS_MT_TRACKING_ID),
            Some(-1)
        );
        assert_eq!(
            tracker.touches.get(&2).map(|touch| touch.slot.slot),
            Some(1)
        );
    }

    #[test]
    fn failed_touch_down_retries_the_exact_frame_then_ignores_successful_duplicate() {
        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter {
            fail_next: true,
            ..FakeWriter::default()
        };
        let down = event(7, PenTouchDevice::Touch, PenTouchPhase::Down, 120, 240);
        assert_eq!(
            emit(&mut tracker, &mut writer, &down).unwrap_err().code,
            "injection_failed"
        );
        let original_tracking_id = event_at(&writer.frames[0].1, EV_ABS, ABS_MT_TRACKING_ID);
        assert!(tracker.touches.contains_key(&7));

        emit(&mut tracker, &mut writer, &down).unwrap();
        assert_eq!(writer.frames[1].1, writer.frames[0].1);
        emit(&mut tracker, &mut writer, &down).unwrap();
        assert_eq!(writer.frames.len(), 2, "a successful duplicate is a no-op");

        let mismatched_down = event(7, PenTouchDevice::Touch, PenTouchPhase::Down, 121, 240);
        assert_eq!(
            emit(&mut tracker, &mut writer, &mismatched_down)
                .unwrap_err()
                .code,
            "invalid_sequence"
        );
        assert_eq!(writer.frames.len(), 2);

        emit(
            &mut tracker,
            &mut writer,
            &event(7, PenTouchDevice::Touch, PenTouchPhase::Cancel, 120, 240),
        )
        .unwrap();
        assert_eq!(event_at(&writer.frames[2].1, EV_ABS, ABS_MT_SLOT), Some(0));
        assert_eq!(
            event_at(&writer.frames[2].1, EV_ABS, ABS_MT_TRACKING_ID),
            Some(-1)
        );
        assert!(!tracker.touches.contains_key(&7));
        assert_ne!(original_tracking_id, Some(-1));
    }

    #[test]
    fn failed_pen_down_retries_the_exact_frame_then_ignores_successful_duplicate() {
        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter {
            fail_next: true,
            ..FakeWriter::default()
        };
        let down = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Down,
            301,
            702,
        );
        assert_eq!(
            emit(&mut tracker, &mut writer, &down).unwrap_err().code,
            "injection_failed"
        );
        assert!(tracker.pen.is_some());

        emit(&mut tracker, &mut writer, &down).unwrap();
        assert_eq!(writer.frames[1].1, writer.frames[0].1);
        emit(&mut tracker, &mut writer, &down).unwrap();
        assert_eq!(writer.frames.len(), 2, "a successful duplicate is a no-op");

        let mismatched_down = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Down,
            302,
            702,
        );
        assert_eq!(
            emit(&mut tracker, &mut writer, &mismatched_down)
                .unwrap_err()
                .code,
            "invalid_sequence"
        );
        assert_eq!(writer.frames.len(), 2);
    }

    #[test]
    fn failed_up_keeps_its_slot_mapped_for_cancel_retry() {
        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter::default();
        emit(
            &mut tracker,
            &mut writer,
            &event(18, PenTouchDevice::Touch, PenTouchPhase::Down, 120, 240),
        )
        .unwrap();
        writer.fail_next = true;
        assert_eq!(
            emit(
                &mut tracker,
                &mut writer,
                &event(18, PenTouchDevice::Touch, PenTouchPhase::Up, 120, 240),
            )
            .unwrap_err()
            .code,
            "injection_failed"
        );
        assert_eq!(
            tracker.touches.get(&18).map(|touch| touch.slot.slot),
            Some(0)
        );

        emit(
            &mut tracker,
            &mut writer,
            &event(18, PenTouchDevice::Touch, PenTouchPhase::Cancel, 120, 240),
        )
        .unwrap();
        assert_eq!(event_at(&writer.frames[2].1, EV_ABS, ABS_MT_SLOT), Some(0));
        assert_eq!(
            event_at(&writer.frames[2].1, EV_ABS, ABS_MT_TRACKING_ID),
            Some(-1)
        );
        assert!(tracker.touches.is_empty());
    }

    #[test]
    fn pen_events_reject_unrelated_pointer_ids_without_releasing_the_owned_pointer() {
        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter::default();
        emit(
            &mut tracker,
            &mut writer,
            &event(
                PEN_POINTER_ID,
                PenTouchDevice::Pen,
                PenTouchPhase::Down,
                50,
                60,
            ),
        )
        .unwrap();
        let event_count = writer.frames.len();
        assert_eq!(
            emit(
                &mut tracker,
                &mut writer,
                &event(11, PenTouchDevice::Pen, PenTouchPhase::Down, 70, 80),
            )
            .unwrap_err()
            .code,
            "invalid_event"
        );
        assert_eq!(
            emit(
                &mut tracker,
                &mut writer,
                &event(11, PenTouchDevice::Pen, PenTouchPhase::Move, 70, 80),
            )
            .unwrap_err()
            .code,
            "invalid_event"
        );
        assert_eq!(
            emit(
                &mut tracker,
                &mut writer,
                &event(11, PenTouchDevice::Pen, PenTouchPhase::Up, 70, 80),
            )
            .unwrap_err()
            .code,
            "invalid_event"
        );
        assert_eq!(writer.frames.len(), event_count);
        assert_eq!(
            pointer_id_of(
                &tracker
                    .pen
                    .as_ref()
                    .expect("pen down retained its identity")
                    .contact
                    .initial_down,
            ),
            Some(PEN_POINTER_ID)
        );
    }

    #[test]
    fn touch_capacity_and_mismatched_terminal_ids_do_not_release_owned_slots() {
        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter::default();
        for pointer_id in 0..u32::from(MAX_TOUCH_SLOTS) {
            emit(
                &mut tracker,
                &mut writer,
                &event(
                    pointer_id,
                    PenTouchDevice::Touch,
                    PenTouchPhase::Down,
                    u16::try_from(pointer_id).expect("test pointer ID fits in u16"),
                    0,
                ),
            )
            .unwrap();
        }
        let frame_count = writer.frames.len();
        assert_eq!(
            emit(
                &mut tracker,
                &mut writer,
                &event(99, PenTouchDevice::Touch, PenTouchPhase::Down, 0, 0),
            )
            .unwrap_err()
            .code,
            "contact_capacity"
        );
        assert_eq!(
            emit(
                &mut tracker,
                &mut writer,
                &event(99, PenTouchDevice::Touch, PenTouchPhase::Up, 0, 0),
            )
            .unwrap_err()
            .code,
            "invalid_sequence"
        );
        assert_eq!(writer.frames.len(), frame_count);
        assert_eq!(tracker.touches.len(), usize::from(MAX_TOUCH_SLOTS));
        assert_eq!(
            tracker.touches.get(&0).map(|touch| touch.slot.slot),
            Some(0)
        );
    }

    #[test]
    fn malformed_pressure_is_rejected_before_a_frame_is_written() {
        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter::default();
        let malformed = Event::PenTouch {
            pointer_id: PEN_POINTER_ID,
            device: PenTouchDevice::Pen,
            phase: PenTouchPhase::Down,
            x: 0,
            y: 0,
            pressure: Some(f32::NAN),
            contact_area_major: None,
            contact_area_minor: None,
            rotation_degrees: None,
            pen_tool: None,
            pen_buttons: None,
            tilt: None,
        };
        assert_eq!(
            emit(&mut tracker, &mut writer, &malformed)
                .unwrap_err()
                .code,
            "invalid_event"
        );
        assert!(writer.frames.is_empty());
        assert!(tracker.is_empty());
    }

    #[test]
    fn touch_area_orientation_and_pen_metadata_map_and_clear_with_contact_lifecycle() {
        for (rotation, expected) in [(0, 0), (90, 90), (180, 0), (270, -90), (359, -1)] {
            assert_eq!(touch_orientation(rotation), expected);
        }

        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter::default();
        let touch_down = Event::PenTouch {
            pointer_id: 402,
            device: PenTouchDevice::Touch,
            phase: PenTouchPhase::Down,
            x: 2100,
            y: 3200,
            pressure: Some(0.5),
            contact_area_major: Some(0.8),
            contact_area_minor: Some(0.3),
            rotation_degrees: Some(270),
            pen_tool: None,
            pen_buttons: None,
            tilt: None,
        };
        emit(&mut tracker, &mut writer, &touch_down).unwrap();
        let touch_frame = &writer.frames[0].1;
        assert_eq!(
            event_at(touch_frame, EV_ABS, ABS_MT_TOUCH_MAJOR),
            Some(52_428)
        );
        assert_eq!(
            event_at(touch_frame, EV_ABS, ABS_MT_TOUCH_MINOR),
            Some(19_661)
        );
        assert_eq!(event_at(touch_frame, EV_ABS, ABS_MT_ORIENTATION), Some(-90));

        let mut touch_move = with_phase(&touch_down, PenTouchPhase::Move);
        if let Event::PenTouch {
            rotation_degrees, ..
        } = &mut touch_move
        {
            *rotation_degrees = None;
        }
        emit(&mut tracker, &mut writer, &touch_move).unwrap();
        assert_eq!(
            event_at(&writer.frames[1].1, EV_ABS, ABS_MT_ORIENTATION),
            Some(0),
            "unknown rotation must neutralize the prior orientation"
        );

        emit(
            &mut tracker,
            &mut writer,
            &with_phase(&touch_move, PenTouchPhase::Cancel),
        )
        .unwrap();
        let touch_cancel = &writer.frames[2].1;
        assert_eq!(event_at(touch_cancel, EV_ABS, ABS_MT_TRACKING_ID), Some(-1));
        for code in [
            ABS_MT_PRESSURE,
            ABS_MT_TOUCH_MAJOR,
            ABS_MT_TOUCH_MINOR,
            ABS_MT_ORIENTATION,
        ] {
            assert_eq!(event_at(touch_cancel, EV_ABS, code), Some(0));
        }
        assert_eq!(event_at(touch_cancel, EV_KEY, BTN_TOUCH), Some(0));

        let pen_down = Event::PenTouch {
            pointer_id: PEN_POINTER_ID,
            device: PenTouchDevice::Pen,
            phase: PenTouchPhase::Down,
            x: 100,
            y: 200,
            pressure: Some(0.7),
            contact_area_major: None,
            contact_area_minor: None,
            rotation_degrees: Some(90),
            pen_tool: Some(PenTool::Eraser),
            pen_buttons: Some(PEN_BUTTON_PRIMARY | PEN_BUTTON_SECONDARY | PEN_BUTTON_TERTIARY),
            tilt: Some(45),
        };
        emit(&mut tracker, &mut writer, &pen_down).unwrap();
        let pen_frame = &writer.frames[3].1;
        assert_eq!(event_at(pen_frame, EV_ABS, ABS_TILT_X), Some(-45));
        assert_eq!(event_at(pen_frame, EV_ABS, ABS_TILT_Y), Some(0));
        assert_eq!(event_at(pen_frame, EV_KEY, BTN_TOOL_PEN), Some(0));
        assert_eq!(event_at(pen_frame, EV_KEY, BTN_TOOL_RUBBER), Some(1));
        assert_eq!(event_at(pen_frame, EV_KEY, BTN_TOUCH), Some(1));
        assert_eq!(event_at(pen_frame, EV_KEY, BTN_STYLUS), Some(1));
        assert_eq!(event_at(pen_frame, EV_KEY, BTN_STYLUS2), Some(1));

        let pen_move = Event::PenTouch {
            pointer_id: PEN_POINTER_ID,
            device: PenTouchDevice::Pen,
            phase: PenTouchPhase::Move,
            x: 101,
            y: 201,
            pressure: Some(0.8),
            contact_area_major: None,
            contact_area_minor: None,
            rotation_degrees: None,
            pen_tool: Some(PenTool::Unknown),
            pen_buttons: None,
            tilt: None,
        };
        emit(&mut tracker, &mut writer, &pen_move).unwrap();
        let pen_move_frame = &writer.frames[4].1;
        assert_eq!(
            event_at(pen_move_frame, EV_ABS, ABS_TILT_X),
            Some(0),
            "unknown tilt must neutralize the prior X tilt"
        );
        assert_eq!(
            event_at(pen_move_frame, EV_ABS, ABS_TILT_Y),
            Some(0),
            "unknown tilt must neutralize the prior Y tilt"
        );
        assert_eq!(event_at(pen_move_frame, EV_KEY, BTN_TOOL_PEN), Some(0));
        assert_eq!(event_at(pen_move_frame, EV_KEY, BTN_TOOL_RUBBER), Some(1));
        assert_eq!(event_at(pen_move_frame, EV_KEY, BTN_STYLUS), Some(1));
        assert_eq!(event_at(pen_move_frame, EV_KEY, BTN_STYLUS2), Some(1));

        emit(
            &mut tracker,
            &mut writer,
            &with_phase(&pen_move, PenTouchPhase::Cancel),
        )
        .unwrap();
        let pen_cancel = &writer.frames[5].1;
        assert_eq!(event_at(pen_cancel, EV_ABS, ABS_PRESSURE), Some(0));
        assert_eq!(event_at(pen_cancel, EV_ABS, ABS_TILT_X), Some(0));
        assert_eq!(event_at(pen_cancel, EV_ABS, ABS_TILT_Y), Some(0));
        for code in [
            BTN_TOUCH,
            BTN_TOOL_PEN,
            BTN_TOOL_RUBBER,
            BTN_STYLUS,
            BTN_STYLUS2,
        ] {
            assert_eq!(event_at(pen_cancel, EV_KEY, code), Some(0));
        }
    }

    #[test]
    fn shutdown_cancels_every_touch_identity_separately_and_survives_device_loss() {
        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter::default();
        for pointer_id in [3, 8, 13] {
            emit(
                &mut tracker,
                &mut writer,
                &event(pointer_id, PenTouchDevice::Touch, PenTouchPhase::Down, 1, 2),
            )
            .unwrap();
        }
        writer.fail_next = true;
        assert_eq!(
            emit(
                &mut tracker,
                &mut writer,
                &event(8, PenTouchDevice::Touch, PenTouchPhase::Move, 9, 10),
            )
            .unwrap_err()
            .code,
            "injection_failed"
        );
        writer.fail_next = true;
        let move_frame_count = writer.frames.len();
        let first_release = tracker
            .release_all(|target, frame| writer.emit(target, frame))
            .unwrap_err();
        assert_eq!(first_release.code, "injection_failed");
        assert_eq!(tracker.touches.len(), 1, "failed contact remains retryable");
        assert_eq!(writer.frames.len(), move_frame_count + 3);
        assert!(
            writer.frames[move_frame_count..]
                .iter()
                .all(|(target, frame)| *target == Target::Touch
                    && event_at(frame, EV_ABS, ABS_MT_TRACKING_ID) == Some(-1))
        );
        tracker
            .release_all(|target, frame| writer.emit(target, frame))
            .unwrap();
        assert!(tracker.is_empty());
    }

    #[test]
    fn pen_pressure_and_tilt_map_to_the_configured_uinput_ranges() {
        let event = Event::PenTouch {
            pointer_id: PEN_POINTER_ID,
            device: PenTouchDevice::Pen,
            phase: PenTouchPhase::Down,
            x: u16::MAX,
            y: 0,
            pressure: Some(0.5),
            contact_area_major: None,
            contact_area_minor: None,
            rotation_degrees: Some(90),
            pen_tool: Some(PenTool::Pen),
            pen_buttons: Some(PEN_BUTTON_PRIMARY),
            tilt: Some(45),
        };
        let frame = pen_frame(&event, initial_pen_metadata(&event)).unwrap();
        assert_eq!(event_at(&frame, EV_ABS, ABS_X), Some(i32::from(u16::MAX)));
        assert_eq!(event_at(&frame, EV_ABS, ABS_Y), Some(0));
        assert_eq!(event_at(&frame, EV_ABS, ABS_PRESSURE), Some(32768));
        assert_eq!(event_at(&frame, EV_ABS, ABS_TILT_X), Some(-45));
        assert_eq!(event_at(&frame, EV_ABS, ABS_TILT_Y), Some(0));
        assert_eq!(event_at(&frame, EV_KEY, BTN_TOUCH), Some(1));
    }
}
