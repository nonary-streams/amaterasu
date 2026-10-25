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
    Event, PEN_BUTTON_PRIMARY, PEN_BUTTON_SECONDARY, PEN_BUTTON_TERTIARY, PenTool, PenTouchDevice,
    PenTouchPhase, pen_tilt_axes,
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
    // A failed write may have sent an input_event prefix. Count this contact
    // until terminal cleanup when the tracking ID reached the device.
    native_write_attempted: bool,
    tracking_id_written: bool,
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
    replay_event: Event,
}

#[derive(Clone)]
struct TrackedTouch {
    slot: TouchSlot,
    contact: TrackedContact,
    replay_event: Event,
}

#[derive(Default)]
struct ContactTracker {
    pen: Option<TrackedPen>,
    touches: BTreeMap<u32, TrackedTouch>,
    next_tracking_id: i32,
}

impl ContactTracker {
    fn validate_pen_lifecycle(
        &self,
        pointer_id: u32,
        phase: PenTouchPhase,
        event: &Event,
    ) -> InputResult<()> {
        let Some(pen) = self.pen.as_ref() else {
            return if phase == PenTouchPhase::Down {
                Ok(())
            } else {
                Err(pointer_not_active())
            };
        };
        match phase {
            PenTouchPhase::Down if pointer_id_of(&pen.contact.initial_down) != Some(pointer_id) => {
                Err(InputError::new(
                    "contact_capacity",
                    "Linux pen uinput supports one active pen pointer",
                ))
            }
            PenTouchPhase::Down if pen.contact.initial_down != *event => {
                Err(duplicate_down_mismatch())
            }
            PenTouchPhase::Down => Ok(()),
            PenTouchPhase::Move | PenTouchPhase::Up | PenTouchPhase::Cancel => {
                ensure_active_pointer(pointer_id_of(&pen.contact.initial_down), pointer_id)
            }
        }
    }

    fn validate_touch_lifecycle(
        &self,
        pointer_id: u32,
        phase: PenTouchPhase,
        event: &Event,
    ) -> InputResult<()> {
        match phase {
            PenTouchPhase::Down => match self.touches.get(&pointer_id) {
                Some(touch) if touch.contact.initial_down != *event => {
                    Err(duplicate_down_mismatch())
                }
                Some(_) => Ok(()),
                None if self.touches.len() >= usize::from(MAX_TOUCH_SLOTS) => Err(InputError::new(
                    "contact_capacity",
                    "Linux touchscreen uinput supports at most 10 simultaneous contacts",
                )),
                None => Ok(()),
            },
            PenTouchPhase::Move | PenTouchPhase::Up | PenTouchPhase::Cancel => self
                .touches
                .contains_key(&pointer_id)
                .then_some(())
                .ok_or_else(pointer_not_active),
        }
    }

    fn inject(
        &mut self,
        event: &Event,
        mut emit: impl FnMut(Target, &[(u16, u16, i32)], &mut bool, &mut usize) -> InputResult<()>,
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
        emit: &mut impl FnMut(Target, &[(u16, u16, i32)], &mut bool, &mut usize) -> InputResult<()>,
    ) -> InputResult<()> {
        self.validate_pen_lifecycle(pointer_id, phase, event)?;
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
                            native_write_attempted: false,
                            tracking_id_written: false,
                        },
                        initial_metadata: metadata,
                        metadata,
                        replay_event: event.clone(),
                    });
                    metadata
                }
            },
            PenTouchPhase::Move => {
                ensure_active_pointer(active_pointer, pointer_id)?;
                let pen = self.pen.as_mut().expect("active pen has a contact");
                pen.contact.last_event = event.clone();
                pen.replay_event = pen_replay_event(&pen.replay_event, event);
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

        // A failed Down can leave a reserved pen without a native report.
        // The first successful Move must send a complete initial report.
        let recovering_down = phase == PenTouchPhase::Move
            && !self
                .pen
                .as_ref()
                .expect("active pen has a contact")
                .contact
                .down_written;
        let frame_event = if recovering_down {
            with_phase(
                &self.pen.as_ref().unwrap().replay_event,
                PenTouchPhase::Down,
            )
        } else {
            event.clone()
        };
        if matches!(phase, PenTouchPhase::Up | PenTouchPhase::Cancel)
            && !self.pen.as_ref().unwrap().contact.native_write_attempted
        {
            self.pen = None;
            return Ok(());
        }
        let frame = pen_frame(&frame_event, frame_metadata)?;
        let mut attempted = false;
        let mut events_written = 0;
        let result = emit(Target::Pen, &frame, &mut attempted, &mut events_written);
        self.pen.as_mut().unwrap().contact.native_write_attempted |= attempted;
        result?;
        match phase {
            PenTouchPhase::Down => {
                self.pen
                    .as_mut()
                    .expect("successful pen down retains its contact")
                    .contact
                    .down_written = true;
            }
            PenTouchPhase::Move => {
                if recovering_down {
                    self.pen
                        .as_mut()
                        .expect("successful recovery retains its pen")
                        .contact
                        .down_written = true;
                }
            }
            PenTouchPhase::Up | PenTouchPhase::Cancel => self.pen = None,
        }
        Ok(())
    }

    fn inject_touch(
        &mut self,
        pointer_id: u32,
        phase: PenTouchPhase,
        event: &Event,
        emit: &mut impl FnMut(Target, &[(u16, u16, i32)], &mut bool, &mut usize) -> InputResult<()>,
    ) -> InputResult<()> {
        self.validate_touch_lifecycle(pointer_id, phase, event)?;
        match phase {
            PenTouchPhase::Down => {
                if let Some(touch) = self.touches.get(&pointer_id) {
                    if touch.contact.down_written {
                        return Ok(());
                    }
                } else {
                    let slot = self.allocate_touch_slot()?;
                    // Reserve the exact client identity before writing: a partial
                    // uinput frame can reach the kernel before write reports failure.
                    self.touches.insert(
                        pointer_id,
                        TrackedTouch {
                            slot,
                            replay_event: event.clone(),
                            contact: TrackedContact {
                                initial_down: event.clone(),
                                last_event: event.clone(),
                                down_written: false,
                                native_write_attempted: false,
                                tracking_id_written: false,
                            },
                        },
                    );
                }
            }
            PenTouchPhase::Move => {
                let Some(touch) = self.touches.get_mut(&pointer_id) else {
                    return Err(pointer_not_active());
                };
                touch.replay_event = touch_replay_event(&touch.replay_event, event);
                touch.contact.last_event = event.clone();
            }
            PenTouchPhase::Up | PenTouchPhase::Cancel => {
                let Some(touch) = self.touches.get_mut(&pointer_id) else {
                    return Err(pointer_not_active());
                };
                touch.contact.last_event = event.clone();
            }
        }

        let touch = self
            .touches
            .get(&pointer_id)
            .ok_or_else(pointer_not_active)?;
        let slot = touch.slot;
        let down_written = touch.contact.down_written;
        // A failed Down may have reserved a slot without creating a uinput
        // device. The first successful Move must establish its tracking ID.
        let recovering_down = phase == PenTouchPhase::Move && !down_written;
        let frame_event = event.clone();
        if matches!(phase, PenTouchPhase::Up | PenTouchPhase::Cancel)
            && !touch.contact.native_write_attempted
        {
            self.touches.remove(&pointer_id);
            return Ok(());
        }
        let active_contacts = self.terminal_contact_count();
        let frame = touch_frame(&frame_event, slot, active_contacts, recovering_down)?;
        let mut attempted = false;
        let mut events_written = 0;
        let result = emit(Target::Touch, &frame, &mut attempted, &mut events_written);
        self.touches
            .get_mut(&pointer_id)
            .unwrap()
            .contact
            .native_write_attempted |= attempted;
        if (phase == PenTouchPhase::Down || recovering_down)
            && tracking_id_in_written_prefix(&frame, events_written)
        {
            self.touches
                .get_mut(&pointer_id)
                .expect("attempted touch retains its contact")
                .contact
                .tracking_id_written = true;
        }
        result?;
        match phase {
            PenTouchPhase::Down => {
                self.touches
                    .get_mut(&pointer_id)
                    .expect("successful touch down retains its contact")
                    .contact
                    .down_written = true;
                self.touches
                    .get_mut(&pointer_id)
                    .expect("successful touch down retains its contact")
                    .contact
                    .tracking_id_written = true;
            }
            PenTouchPhase::Move => {
                if recovering_down {
                    self.touches
                        .get_mut(&pointer_id)
                        .expect("successful recovery retains its contact")
                        .contact
                        .down_written = true;
                    self.touches
                        .get_mut(&pointer_id)
                        .expect("successful recovery retains its contact")
                        .contact
                        .tracking_id_written = true;
                }
            }
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

    /// Counts contacts whose positive tracking ID was accepted by uinput.
    /// A reservation or failed write prefix before ABS_MT_TRACKING_ID cannot
    /// keep the shared BTN_TOUCH key held.
    fn terminal_contact_count(&self) -> usize {
        self.touches
            .values()
            .filter(|touch| touch.contact.tracking_id_written)
            .count()
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
        mut emit: impl FnMut(Target, &[(u16, u16, i32)], &mut bool, &mut usize) -> InputResult<()>,
    ) -> InputResult<()> {
        let mut first_error = None;
        if let Some(pen) = self.pen.clone() {
            if !pen.contact.native_write_attempted {
                self.pen = None;
            } else {
                let cancel_event = with_phase(&pen.contact.last_event, PenTouchPhase::Cancel);
                let frame = pen_frame(&cancel_event, pen.metadata)?;
                let mut attempted = false;
                let mut events_written = 0;
                match emit(Target::Pen, &frame, &mut attempted, &mut events_written) {
                    Ok(()) => self.pen = None,
                    Err(error) => first_error = Some(error),
                }
            }
        }

        for pointer_id in self.touches.keys().copied().collect::<Vec<_>>() {
            let Some(touch) = self.touches.get(&pointer_id).cloned() else {
                continue;
            };
            if !touch.contact.native_write_attempted {
                self.touches.remove(&pointer_id);
                continue;
            }
            let cancel_event = with_phase(&touch.contact.last_event, PenTouchPhase::Cancel);
            let active_contacts = self.terminal_contact_count();
            let frame = touch_frame(&cancel_event, touch.slot, active_contacts, false)?;
            let mut attempted = false;
            let mut events_written = 0;
            match emit(Target::Touch, &frame, &mut attempted, &mut events_written) {
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

    fn reset_touch_device_state(&mut self) {
        for touch in self.touches.values_mut() {
            touch.contact.down_written = false;
            touch.contact.native_write_attempted = false;
            touch.contact.tracking_id_written = false;
        }
    }

    fn reset_pen_device_state(&mut self) {
        if let Some(pen) = self.pen.as_mut() {
            pen.contact.down_written = false;
            pen.contact.native_write_attempted = false;
        }
    }

    fn replay_active_touches(
        &mut self,
        terminal_pointer: Option<u32>,
        mut emit: impl FnMut(Target, &[(u16, u16, i32)], &mut bool, &mut usize) -> InputResult<()>,
    ) -> InputResult<()> {
        let mut active_contacts = 0;
        for (pointer_id, touch) in &mut self.touches {
            if Some(*pointer_id) == terminal_pointer
                || matches!(
                    touch.contact.last_event,
                    Event::PenTouch {
                        phase: PenTouchPhase::Up | PenTouchPhase::Cancel,
                        ..
                    }
                )
            {
                // A failed terminal report is still tracked for its retry,
                // but no contact needs to be recreated on the new device.
                continue;
            }
            let down = with_phase(&touch.replay_event, PenTouchPhase::Down);
            let frame = touch_frame(&down, touch.slot, active_contacts + 1, false)?;
            let mut attempted = false;
            let mut events_written = 0;
            let result = emit(Target::Touch, &frame, &mut attempted, &mut events_written);
            touch.contact.native_write_attempted |= attempted;
            touch.contact.tracking_id_written |=
                tracking_id_in_written_prefix(&frame, events_written);
            result?;
            touch.contact.down_written = true;
            touch.contact.tracking_id_written = true;
            active_contacts += 1;
        }
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.pen.is_none() && self.touches.is_empty()
    }
}

pub(super) struct LinuxPenTouch {
    tracker: ContactTracker,
    pen_device: Option<Device>,
    touch_device: Option<Device>,
    pen_write_failed: bool,
    touch_write_failed: bool,
}

impl LinuxPenTouch {
    pub(super) fn new() -> Self {
        Self {
            tracker: ContactTracker::default(),
            pen_device: None,
            touch_device: None,
            pen_write_failed: false,
            touch_write_failed: false,
        }
    }

    pub(super) fn is_idle(&self) -> bool {
        self.tracker.is_empty() && self.pen_device.is_none() && self.touch_device.is_none()
    }

    pub(super) fn has_device(&self) -> bool {
        self.pen_device.is_some() || self.touch_device.is_some()
    }

    fn recover_touch_device(
        &mut self,
        terminal_pointer: Option<u32>,
        mut create: impl FnMut() -> InputResult<Device>,
    ) -> InputResult<()> {
        destroy_device(&mut self.touch_device)?;
        self.tracker.reset_touch_device_state();
        self.tracker.replay_active_touches(
            terminal_pointer,
            |_, frame, attempted, events_written| {
                if self.touch_device.is_none() {
                    self.touch_device = Some(create()?);
                }
                *attempted = true;
                self.touch_device
                    .as_ref()
                    .unwrap()
                    .emit_with_progress(frame, events_written)
            },
        )?;
        self.touch_write_failed = false;
        Ok(())
    }

    pub(super) fn inject(&mut self, path: &str, event: &Event) -> InputResult<()> {
        event.validate()?;
        if self.pen_write_failed
            && matches!(
                event,
                Event::PenTouch {
                    device: PenTouchDevice::Pen,
                    ..
                }
            )
        {
            if let Event::PenTouch {
                pointer_id, phase, ..
            } = event
            {
                self.tracker
                    .validate_pen_lifecycle(*pointer_id, *phase, event)?;
            }
            // A failed frame can leave an arbitrary prefix in the kernel. The
            // device must be removed before any more reports reach that pen.
            destroy_device(&mut self.pen_device)?;
            self.pen_write_failed = false;
            self.tracker.reset_pen_device_state();
        }
        if self.touch_write_failed
            && let Event::PenTouch {
                device: PenTouchDevice::Touch,
                pointer_id,
                phase,
                ..
            } = event
        {
            self.tracker
                .validate_touch_lifecycle(*pointer_id, *phase, event)?;
            // A partial frame can corrupt any slot on this shared device.
            // Detach it before replaying every still-active pointer.
            let terminal_pointer = match event {
                Event::PenTouch {
                    pointer_id,
                    phase: PenTouchPhase::Up | PenTouchPhase::Cancel,
                    ..
                } => Some(*pointer_id),
                _ => None,
            };
            self.recover_touch_device(terminal_pointer, || Device::create_touchscreen(path))?;
        }
        let Self {
            tracker,
            pen_device,
            touch_device,
            pen_write_failed,
            touch_write_failed,
        } = self;
        tracker.inject(event, |target, frame, attempted, events_written| {
            let device = match target {
                Target::Pen => &mut *pen_device,
                Target::Touch => &mut *touch_device,
            };
            if device.is_none()
                && matches!(
                    event,
                    Event::PenTouch {
                        phase: PenTouchPhase::Up | PenTouchPhase::Cancel,
                        ..
                    }
                )
            {
                // A failed Down may have reserved a contact before opening
                // uinput. No native device exists to receive its terminal event.
                return Ok(());
            }
            if device.is_none() {
                *device = Some(match target {
                    Target::Pen => Device::create_pen(path)?,
                    Target::Touch => Device::create_touchscreen(path)?,
                });
            }
            let device = device.as_ref().ok_or_else(|| {
                InputError::new("backend_inactive", "uinput device is not active")
            })?;
            *attempted = true;
            let result = device.emit_with_progress(frame, events_written);
            if target == Target::Pen && result.is_err() {
                *pen_write_failed = true;
            }
            if target == Target::Touch && result.is_err() {
                *touch_write_failed = true;
            }
            result
        })
    }

    pub(super) fn stop(&mut self) -> InputResult<()> {
        let Self {
            tracker,
            pen_device,
            touch_device,
            pen_write_failed,
            touch_write_failed,
        } = self;
        // Send per-pointer terminal frames first. If a write fails, destroying
        // our owned devices still removes all contacts from those devices.
        let _release_result = tracker.release_all(|target, frame, attempted, events_written| {
            let device = match target {
                Target::Pen => pen_device.as_ref(),
                Target::Touch => touch_device.as_ref(),
            };
            match device {
                Some(device) => {
                    *attempted = true;
                    device.emit_with_progress(frame, events_written)
                }
                // A Down can fail while opening UInput before any device
                // exists; there is then no kernel contact to release.
                None => Ok(()),
            }
        });

        let pen_destroy = destroy_device(pen_device);
        if pen_destroy.is_ok() {
            tracker.clear_target(Target::Pen);
            *pen_write_failed = false;
        }
        let touch_destroy = destroy_device(touch_device);
        if touch_destroy.is_ok() {
            tracker.clear_target(Target::Touch);
            *touch_write_failed = false;
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

fn touch_replay_event(previous: &Event, current: &Event) -> Event {
    let mut replay = current.clone();
    if let (
        Event::PenTouch {
            pressure: previous_pressure,
            contact_area_major: previous_major,
            contact_area_minor: previous_minor,
            ..
        },
        Event::PenTouch {
            pressure,
            contact_area_major,
            contact_area_minor,
            ..
        },
    ) = (previous, &mut replay)
    {
        // Omitted pressure and area samples leave those axes unchanged on a
        // live uinput device. Carry their last values onto a fresh device.
        if pressure.is_none() {
            *pressure = *previous_pressure;
        }
        if contact_area_major.is_none() {
            *contact_area_major = *previous_major;
            *contact_area_minor = *previous_minor;
        }
    }
    replay
}

fn pen_replay_event(previous: &Event, current: &Event) -> Event {
    let mut replay = current.clone();
    if let (
        Event::PenTouch {
            pressure: previous_pressure,
            ..
        },
        Event::PenTouch { pressure, .. },
    ) = (previous, &mut replay)
        && pressure.is_none()
    {
        // Sparse moves retain the pressure already applied to the old device.
        // Reopening after a partial write needs that value in its initial frame.
        *pressure = *previous_pressure;
    }
    replay
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

fn tracking_id_in_written_prefix(frame: &[(u16, u16, i32)], events_written: usize) -> bool {
    frame
        .iter()
        .enumerate()
        .any(|(index, (event_type, code, value))| {
            index < events_written
                && *event_type == EV_ABS
                && *code == ABS_MT_TRACKING_ID
                && *value >= 0
        })
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
    recovering_down: bool,
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
        PenTouchPhase::Move if recovering_down => {
            frame.push((EV_ABS, ABS_MT_TRACKING_ID, slot.tracking_id));
        }
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
        if *phase == PenTouchPhase::Down || recovering_down {
            // A prior frame may have stopped after TRACKING_ID but before the
            // shared touch key. Reassert it for Down and recovered Move frames.
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
    use crate::model::PEN_POINTER_ID;

    fn fake_device(path: &str) -> Device {
        Device {
            file: std::fs::OpenOptions::new().write(true).open(path).unwrap(),
            created: false,
            fake: None,
        }
    }

    #[test]
    fn invalid_touch_lifecycle_does_not_drain_pending_device_recovery() {
        let mut pen_touch = LinuxPenTouch::new();
        pen_touch.touch_device = Some(fake_device("/dev/full"));
        let down = event(11, PenTouchDevice::Touch, PenTouchPhase::Down, 10, 20);
        assert!(pen_touch.inject("\0", &down).is_err());
        let original_contact = pen_touch.tracker.touches.get(&11).unwrap().clone();
        pen_touch.touch_device.as_mut().unwrap().created = true;

        let mismatched_down = event(11, PenTouchDevice::Touch, PenTouchPhase::Down, 50, 60);
        for invalid in [
            event(22, PenTouchDevice::Touch, PenTouchPhase::Move, 10, 20),
            event(22, PenTouchDevice::Touch, PenTouchPhase::Up, 10, 20),
            event(22, PenTouchDevice::Touch, PenTouchPhase::Cancel, 10, 20),
            mismatched_down,
        ] {
            assert_eq!(
                pen_touch.inject("\0", &invalid).unwrap_err().code,
                "invalid_sequence"
            );
            assert!(pen_touch.touch_write_failed);
            assert!(pen_touch.touch_device.as_ref().unwrap().created);
            assert_eq!(pen_touch.tracker.touches.len(), 1);
            let contact = pen_touch.tracker.touches.get(&11).unwrap();
            assert_eq!(contact.slot, original_contact.slot);
            assert_eq!(
                contact.contact.last_event,
                original_contact.contact.last_event
            );
            assert_eq!(
                contact.contact.down_written,
                original_contact.contact.down_written
            );
            assert_eq!(
                contact.contact.native_write_attempted,
                original_contact.contact.native_write_attempted
            );
        }

        assert!(
            pen_touch
                .tracker
                .validate_touch_lifecycle(11, PenTouchPhase::Down, &down)
                .is_ok()
        );
        let new_down = event(22, PenTouchDevice::Touch, PenTouchPhase::Down, 30, 40);
        assert!(
            pen_touch
                .tracker
                .validate_touch_lifecycle(22, PenTouchPhase::Down, &new_down)
                .is_ok()
        );
        pen_touch.touch_device.as_mut().unwrap().created = false;
        pen_touch.stop().unwrap();
    }

    #[test]
    fn failed_touch_down_replays_on_replacement_and_stop_allows_reacquisition() {
        let mut pen_touch = LinuxPenTouch::new();
        pen_touch.touch_device = Some(fake_device("/dev/full"));
        let down = event(11, PenTouchDevice::Touch, PenTouchPhase::Down, 10, 20);
        assert!(pen_touch.inject("\0", &down).is_err());
        assert!(pen_touch.touch_write_failed);
        let slot = pen_touch.tracker.touches.get(&11).unwrap().slot;

        pen_touch.touch_device.as_mut().unwrap().created = true;
        assert!(
            pen_touch
                .recover_touch_device(None, || Ok(fake_device("/dev/null")))
                .is_err()
        );
        assert!(pen_touch.touch_write_failed);
        assert!(pen_touch.touch_device.is_some());

        pen_touch.pen_device = Some(fake_device("/dev/null"));
        let pen_down = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Down,
            10,
            20,
        );
        pen_touch.inject("\0", &pen_down).unwrap();
        pen_touch
            .inject("\0", &with_phase(&pen_down, PenTouchPhase::Cancel))
            .unwrap();
        assert!(pen_touch.touch_device.is_some());
        pen_touch.touch_device.as_mut().unwrap().created = false;
        pen_touch
            .recover_touch_device(None, || Ok(fake_device("/dev/null")))
            .unwrap();
        let contact = pen_touch.tracker.touches.get(&11).unwrap();
        assert_eq!(contact.slot, slot);
        assert!(contact.contact.down_written);
        pen_touch.inject("\0", &down).unwrap();
        pen_touch.touch_device = Some(fake_device("/dev/full"));
        let moved = event(11, PenTouchDevice::Touch, PenTouchPhase::Move, 50, 60);
        assert!(pen_touch.inject("\0", &moved).is_err());
        assert!(pen_touch.touch_write_failed);
        pen_touch
            .recover_touch_device(None, || Ok(fake_device("/dev/null")))
            .unwrap();
        let contact = pen_touch.tracker.touches.get(&11).unwrap();
        assert_eq!(contact.slot, slot);
        assert_eq!(contact.replay_event, moved);
        assert!(contact.contact.down_written);
        pen_touch
            .inject("\0", &with_phase(&down, PenTouchPhase::Up))
            .unwrap();

        pen_touch.touch_device = Some(fake_device("/dev/full"));
        assert!(pen_touch.inject("\0", &down).is_err());
        pen_touch.stop().unwrap();
        assert!(pen_touch.is_idle());
        pen_touch.touch_device = Some(fake_device("/dev/null"));
        pen_touch.inject("\0", &down).unwrap();
        pen_touch.stop().unwrap();
        assert!(pen_touch.is_idle());
    }

    #[test]
    fn terminal_retry_after_failed_touch_write_needs_no_new_uinput_path() {
        for phase in [PenTouchPhase::Up, PenTouchPhase::Cancel] {
            let mut pen_touch = LinuxPenTouch::new();
            pen_touch.touch_device = Some(fake_device("/dev/full"));
            let down = event(11, PenTouchDevice::Touch, PenTouchPhase::Down, 10, 20);
            assert!(pen_touch.inject("\0", &down).is_err());
            pen_touch.inject("\0", &with_phase(&down, phase)).unwrap();
            assert!(pen_touch.is_idle());
        }
    }

    #[test]
    fn failed_touch_terminal_replays_other_contacts_and_retries_release() {
        for phase in [PenTouchPhase::Up, PenTouchPhase::Cancel] {
            let mut pen_touch = LinuxPenTouch::new();
            pen_touch.touch_device = Some(fake_device("/dev/null"));
            let first = event(11, PenTouchDevice::Touch, PenTouchPhase::Down, 10, 20);
            let second = event(22, PenTouchDevice::Touch, PenTouchPhase::Down, 30, 40);
            pen_touch.inject("\0", &first).unwrap();
            pen_touch.inject("\0", &second).unwrap();
            let first_slot = pen_touch.tracker.touches.get(&11).unwrap().slot;
            let second_slot = pen_touch.tracker.touches.get(&22).unwrap().slot;
            pen_touch.touch_device = Some(fake_device("/dev/full"));
            let terminal = with_phase(&first, phase);
            assert!(pen_touch.inject("\0", &terminal).is_err());
            assert!(pen_touch.touch_write_failed);

            pen_touch
                .recover_touch_device(None, || Ok(fake_device("/dev/null")))
                .unwrap();
            assert_eq!(pen_touch.tracker.touches.get(&11).unwrap().slot, first_slot);
            assert!(
                !pen_touch
                    .tracker
                    .touches
                    .get(&11)
                    .unwrap()
                    .contact
                    .down_written
            );
            assert_eq!(
                pen_touch.tracker.touches.get(&22).unwrap().slot,
                second_slot
            );
            assert!(
                pen_touch
                    .tracker
                    .touches
                    .get(&22)
                    .unwrap()
                    .contact
                    .down_written
            );
            pen_touch.inject("\0", &terminal).unwrap();
            assert!(!pen_touch.tracker.touches.contains_key(&11));
            pen_touch
                .inject("\0", &with_phase(&second, PenTouchPhase::Cancel))
                .unwrap();
            pen_touch.stop().unwrap();
            assert!(pen_touch.is_idle());
        }
    }

    #[test]
    fn up_after_failed_device_creation_releases_only_its_reserved_contact() {
        let mut pen_touch = LinuxPenTouch::new();
        let unavailable_path = "\0";
        let first_down = event(11, PenTouchDevice::Touch, PenTouchPhase::Down, 10, 20);
        let second_down = event(22, PenTouchDevice::Touch, PenTouchPhase::Down, 30, 40);
        for down in [&first_down, &second_down] {
            assert!(pen_touch.inject(unavailable_path, down).is_err());
        }
        assert!(pen_touch.touch_device.is_none());
        assert_eq!(pen_touch.tracker.touches.len(), 2);

        pen_touch
            .inject(
                unavailable_path,
                &with_phase(&first_down, PenTouchPhase::Up),
            )
            .unwrap();
        assert!(pen_touch.touch_device.is_none());
        assert!(!pen_touch.tracker.touches.contains_key(&11));
        assert!(pen_touch.tracker.touches.contains_key(&22));
        pen_touch
            .inject(
                unavailable_path,
                &with_phase(&second_down, PenTouchPhase::Cancel),
            )
            .unwrap();
        assert!(pen_touch.is_idle());

        let pen_down = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Down,
            50,
            60,
        );
        assert!(pen_touch.inject(unavailable_path, &pen_down).is_err());
        pen_touch
            .inject(unavailable_path, &with_phase(&pen_down, PenTouchPhase::Up))
            .unwrap();
        assert!(pen_touch.is_idle());

        // A later acquire can use a new owned device with the same pointer ID.
        // /dev/null receives fake frames; this test never opens uinput.
        pen_touch.touch_device = Some(Device {
            file: std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/null")
                .unwrap(),
            created: false,
            fake: None,
        });
        pen_touch.inject(unavailable_path, &first_down).unwrap();
        assert!(pen_touch.tracker.touches.contains_key(&11));
        pen_touch
            .inject(
                unavailable_path,
                &with_phase(&first_down, PenTouchPhase::Up),
            )
            .unwrap();
        pen_touch.stop().unwrap();
        assert!(pen_touch.is_idle());
    }

    #[test]
    fn owned_device_stays_available_when_uinput_path_disappears() {
        let mut backend = super::super::LinuxBackend::new("\0".into());
        backend.pen_touch.pen_device = Some(Device {
            file: std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/null")
                .unwrap(),
            created: false,
            fake: None,
        });
        assert!(super::super::Backend::pen_touch_supported(&backend));
        let pen_down = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Down,
            10,
            20,
        );
        super::super::Backend::inject(&mut backend, &pen_down).unwrap();
        let pen_move = with_phase(&pen_down, PenTouchPhase::Move);
        super::super::Backend::inject(&mut backend, &pen_move).unwrap();
        let pen_cancel = with_phase(&pen_down, PenTouchPhase::Cancel);
        super::super::Backend::inject(&mut backend, &pen_cancel).unwrap();
        assert!(backend.pen_touch.tracker.is_empty());

        let touch_down = event(42, PenTouchDevice::Touch, PenTouchPhase::Down, 30, 40);
        assert!(super::super::Backend::validate(&backend, &touch_down).is_ok());
        assert!(super::super::Backend::inject(&mut backend, &touch_down).is_err());
        assert!(backend.pen_touch.tracker.touches.contains_key(&42));
        super::super::Backend::inject(
            &mut backend,
            &with_phase(&touch_down, PenTouchPhase::Cancel),
        )
        .unwrap();
        assert!(backend.pen_touch.tracker.is_empty());

        backend.pen_touch.pen_device = None;
        assert!(!super::super::Backend::pen_touch_supported(&backend));
    }

    #[test]
    fn failed_pen_report_discards_the_device_before_reusing_its_pointer() {
        let mut pen_touch = LinuxPenTouch::new();
        pen_touch.pen_device = Some(Device {
            file: std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap(),
            created: false,
            fake: None,
        });
        let down = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Down,
            10,
            20,
        );
        assert!(pen_touch.inject("\0", &down).is_err());
        assert!(pen_touch.pen_write_failed);
        assert!(pen_touch.pen_device.is_some());
        assert!(
            pen_touch
                .tracker
                .pen
                .as_ref()
                .unwrap()
                .contact
                .native_write_attempted
        );

        let mismatched_down = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Down,
            11,
            20,
        );
        assert_eq!(
            pen_touch.inject("\0", &mismatched_down).unwrap_err().code,
            "invalid_sequence"
        );
        assert!(
            pen_touch.pen_device.is_some(),
            "a rejected duplicate must not detach the failed device"
        );
        assert!(pen_touch.pen_write_failed);
        assert_eq!(
            pen_touch.tracker.pen.as_ref().unwrap().contact.initial_down,
            down
        );

        let other_pointer_down = event(2, PenTouchDevice::Pen, PenTouchPhase::Down, 70, 80);
        assert_eq!(
            pen_touch
                .tracker
                .validate_pen_lifecycle(2, PenTouchPhase::Down, &other_pointer_down)
                .unwrap_err()
                .code,
            "contact_capacity"
        );
        assert_eq!(
            pen_touch
                .inject("\0", &other_pointer_down)
                .unwrap_err()
                .code,
            "invalid_event",
            "the public pen identity rule rejects this before native recovery"
        );
        assert!(pen_touch.pen_device.is_some());
        assert!(pen_touch.pen_write_failed);

        let malformed_pen = event(2, PenTouchDevice::Pen, PenTouchPhase::Move, 10, 20);
        assert!(pen_touch.inject("\0", &malformed_pen).is_err());
        assert!(pen_touch.pen_device.is_some());
        assert!(pen_touch.pen_write_failed);

        pen_touch.touch_device = Some(Device {
            file: std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/null")
                .unwrap(),
            created: false,
            fake: None,
        });
        let touch_down = event(42, PenTouchDevice::Touch, PenTouchPhase::Down, 30, 40);
        pen_touch.inject("\0", &touch_down).unwrap();
        assert!(pen_touch.pen_write_failed);
        assert!(pen_touch.pen_device.is_some());
        pen_touch
            .inject("\0", &with_phase(&touch_down, PenTouchPhase::Up))
            .unwrap();

        // Simulate a failed UI_DEV_DESTROY. Pen cleanup retains its descriptor,
        // while another touch report can still use the independent device.
        pen_touch.pen_device.as_mut().unwrap().created = true;
        assert!(
            pen_touch
                .inject("\0", &with_phase(&down, PenTouchPhase::Cancel))
                .is_err()
        );
        assert!(pen_touch.pen_write_failed);
        pen_touch.inject("\0", &touch_down).unwrap();
        pen_touch
            .inject("\0", &with_phase(&touch_down, PenTouchPhase::Up))
            .unwrap();
        pen_touch.pen_device.as_mut().unwrap().created = false;

        // The terminal report needs no replacement device once the failed
        // device has been removed. Its pointer can then be acquired again.
        pen_touch
            .inject("\0", &with_phase(&down, PenTouchPhase::Cancel))
            .unwrap();
        assert!(!pen_touch.pen_write_failed);
        assert!(pen_touch.pen_device.is_none());
        assert!(pen_touch.tracker.pen.is_none());

        pen_touch.pen_device = Some(Device {
            file: std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/null")
                .unwrap(),
            created: false,
            fake: None,
        });
        pen_touch.inject("\0", &down).unwrap();
        assert!(pen_touch.tracker.pen.as_ref().unwrap().contact.down_written);
        pen_touch
            .inject("\0", &with_phase(&down, PenTouchPhase::Move))
            .unwrap();
        pen_touch
            .inject("\0", &with_phase(&down, PenTouchPhase::Up))
            .unwrap();
        assert!(pen_touch.tracker.pen.is_none());
        pen_touch.stop().unwrap();
        assert!(pen_touch.is_idle());
    }

    type RecordedFrame = (Target, Vec<(u16, u16, i32)>);

    #[derive(Default)]
    struct FakeWriter {
        frames: Vec<RecordedFrame>,
        accepted_events: Vec<usize>,
        fail_next: bool,
        partial_next: Option<usize>,
    }

    impl FakeWriter {
        fn emit(
            &mut self,
            target: Target,
            frame: &[(u16, u16, i32)],
            events_written: &mut usize,
        ) -> InputResult<()> {
            if let Some(event_count) = self.partial_next.take() {
                self.frames.push((target, frame[..event_count].to_vec()));
                self.accepted_events.push(event_count);
                *events_written = event_count;
                return Err(InputError::new("injection_failed", "partial fake write"));
            }
            self.frames.push((target, frame.to_vec()));
            if self.fail_next {
                self.fail_next = false;
                // This mode models failure before the first event is accepted.
                // Keep the attempted frame for retry assertions, but do not
                // confuse it with an event prefix delivered to uinput.
                self.accepted_events.push(0);
                Err(InputError::new("injection_failed", "fake device loss"))
            } else {
                self.accepted_events.push(frame.len());
                *events_written = frame.len();
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
        tracker.inject(event, |target, frame, attempted, events_written| {
            *attempted = true;
            writer.emit(target, frame, events_written)
        })
    }

    fn event_at(frame: &[(u16, u16, i32)], kind: u16, code: u16) -> Option<i32> {
        frame.iter().find_map(|(event_kind, event_code, value)| {
            (*event_kind == kind && *event_code == code).then_some(*value)
        })
    }

    #[test]
    fn partial_touch_move_replays_all_live_slots_with_stable_tracking_ids() {
        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter::default();
        let mut first = event(11, PenTouchDevice::Touch, PenTouchPhase::Down, 10, 20);
        if let Event::PenTouch {
            contact_area_major,
            contact_area_minor,
            ..
        } = &mut first
        {
            *contact_area_major = Some(0.8);
            *contact_area_minor = Some(0.3);
        }
        let second = event(22, PenTouchDevice::Touch, PenTouchPhase::Down, 30, 40);
        emit(&mut tracker, &mut writer, &first).unwrap();
        emit(&mut tracker, &mut writer, &second).unwrap();
        let first_slot = tracker.touches.get(&11).unwrap().slot;
        let second_slot = tracker.touches.get(&22).unwrap().slot;
        writer.partial_next = Some(2);
        let mut moved = event(11, PenTouchDevice::Touch, PenTouchPhase::Move, 50, 60);
        if let Event::PenTouch { pressure, .. } = &mut moved {
            *pressure = None;
        }
        assert!(emit(&mut tracker, &mut writer, &moved).is_err());

        tracker.reset_touch_device_state();
        let frame_count = writer.frames.len();
        tracker
            .replay_active_touches(None, |target, frame, attempted, events_written| {
                *attempted = true;
                writer.emit(target, frame, events_written)
            })
            .unwrap();
        assert_eq!(writer.frames.len(), frame_count + 2);
        for (frame, slot, x) in [
            (&writer.frames[frame_count].1, first_slot, 50),
            (&writer.frames[frame_count + 1].1, second_slot, 30),
        ] {
            assert_eq!(event_at(frame, EV_ABS, ABS_MT_SLOT), Some(slot.slot));
            assert_eq!(
                event_at(frame, EV_ABS, ABS_MT_TRACKING_ID),
                Some(slot.tracking_id)
            );
            assert_eq!(event_at(frame, EV_ABS, ABS_MT_POSITION_X), Some(x));
            assert_eq!(event_at(frame, EV_KEY, BTN_TOUCH), Some(1));
        }
        assert!(
            tracker
                .touches
                .values()
                .all(|touch| touch.contact.down_written)
        );
        let first_replay = &writer.frames[frame_count].1;
        assert_eq!(
            event_at(first_replay, EV_ABS, ABS_MT_PRESSURE),
            Some(pressure_value(0.5))
        );
        assert_eq!(
            event_at(first_replay, EV_ABS, ABS_MT_TOUCH_MAJOR),
            Some(normalized_axis_value(0.8))
        );
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
    fn partial_touch_down_keeps_shared_tip_pressed_until_uncertain_slot_releases() {
        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter {
            partial_next: Some(2),
            ..FakeWriter::default()
        };
        let first_down = event(7, PenTouchDevice::Touch, PenTouchPhase::Down, 10, 20);
        let second_down = event(8, PenTouchDevice::Touch, PenTouchPhase::Down, 30, 40);
        assert!(emit(&mut tracker, &mut writer, &first_down).is_err());
        assert_eq!(
            event_at(&writer.frames[0].1, EV_ABS, ABS_MT_TRACKING_ID),
            Some(0)
        );
        assert_eq!(writer.accepted_events[0], 2);
        assert_eq!(event_at(&writer.frames[0].1, EV_KEY, BTN_TOUCH), None);
        assert!(
            tracker
                .touches
                .get(&7)
                .unwrap()
                .contact
                .native_write_attempted
        );
        assert!(!tracker.touches.get(&7).unwrap().contact.down_written);

        emit(&mut tracker, &mut writer, &second_down).unwrap();
        assert_eq!(event_at(&writer.frames[1].1, EV_KEY, BTN_TOUCH), Some(1));
        emit(
            &mut tracker,
            &mut writer,
            &with_phase(&second_down, PenTouchPhase::Up),
        )
        .unwrap();
        assert_eq!(event_at(&writer.frames[2].1, EV_KEY, BTN_TOUCH), None);
        emit(
            &mut tracker,
            &mut writer,
            &with_phase(&first_down, PenTouchPhase::Up),
        )
        .unwrap();
        assert_eq!(event_at(&writer.frames[3].1, EV_KEY, BTN_TOUCH), Some(0));
        assert!(tracker.touches.is_empty());
    }

    #[test]
    fn failed_device_creation_never_counts_as_a_native_touch() {
        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter::default();
        let first_down = event(7, PenTouchDevice::Touch, PenTouchPhase::Down, 10, 20);
        let second_down = event(8, PenTouchDevice::Touch, PenTouchPhase::Down, 30, 40);
        assert!(
            tracker
                .inject(&first_down, |_, _, _, _| Err(InputError::new(
                    "injection_failed",
                    "open failed"
                )))
                .is_err()
        );
        assert!(
            !tracker
                .touches
                .get(&7)
                .unwrap()
                .contact
                .native_write_attempted
        );
        emit(&mut tracker, &mut writer, &second_down).unwrap();
        assert_eq!(event_at(&writer.frames[0].1, EV_KEY, BTN_TOUCH), Some(1));
        tracker
            .inject(&with_phase(&first_down, PenTouchPhase::Up), |_, _, _, _| {
                panic!("uncreated contact must not emit a native release")
            })
            .unwrap();
        emit(
            &mut tracker,
            &mut writer,
            &with_phase(&second_down, PenTouchPhase::Up),
        )
        .unwrap();
        assert_eq!(event_at(&writer.frames[1].1, EV_KEY, BTN_TOUCH), Some(0));
    }

    #[test]
    fn move_after_failed_pen_down_emits_a_complete_initial_report() {
        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter {
            fail_next: true,
            ..FakeWriter::default()
        };
        let mut down = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Down,
            10,
            20,
        );
        if let Event::PenTouch { pressure, .. } = &mut down {
            *pressure = None;
        }
        assert!(emit(&mut tracker, &mut writer, &down).is_err());
        let mut moved = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Move,
            11,
            21,
        );
        if let Event::PenTouch { pressure, .. } = &mut moved {
            *pressure = None;
        }
        emit(&mut tracker, &mut writer, &moved).unwrap();
        let recovered = &writer.frames[1].1;
        assert_eq!(event_at(recovered, EV_ABS, ABS_X), Some(11));
        assert_eq!(event_at(recovered, EV_ABS, ABS_PRESSURE), Some(0));
        assert_eq!(event_at(recovered, EV_KEY, BTN_TOUCH), Some(1));
        assert_eq!(event_at(recovered, EV_KEY, BTN_TOOL_PEN), Some(1));
        assert!(tracker.pen.as_ref().unwrap().contact.down_written);
    }

    #[test]
    fn sparse_pen_pressure_survives_failed_update_and_device_recreation() {
        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter::default();
        let mut down = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Down,
            10,
            20,
        );
        if let Event::PenTouch { pressure, .. } = &mut down {
            *pressure = Some(0.7);
        }
        emit(&mut tracker, &mut writer, &down).unwrap();

        let mut moved = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Move,
            11,
            21,
        );
        if let Event::PenTouch { pressure, .. } = &mut moved {
            *pressure = None;
        }
        emit(&mut tracker, &mut writer, &moved).unwrap();
        assert_eq!(event_at(&writer.frames[1].1, EV_ABS, ABS_PRESSURE), None);

        writer.partial_next = Some(2);
        let mut failed_move = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Move,
            12,
            22,
        );
        if let Event::PenTouch { pressure, .. } = &mut failed_move {
            *pressure = None;
        }
        assert!(emit(&mut tracker, &mut writer, &failed_move).is_err());

        // The native device is destroyed after a partial frame. The next
        // sparse move opens a fresh device and must restore its held pressure.
        tracker.reset_pen_device_state();
        let mut recovered_move = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Move,
            13,
            23,
        );
        if let Event::PenTouch { pressure, .. } = &mut recovered_move {
            *pressure = None;
        }
        emit(&mut tracker, &mut writer, &recovered_move).unwrap();
        let recovered = &writer.frames[3].1;
        assert_eq!(event_at(recovered, EV_ABS, ABS_X), Some(13));
        assert_eq!(
            event_at(recovered, EV_ABS, ABS_PRESSURE),
            Some(pressure_value(0.7))
        );
        assert_eq!(event_at(recovered, EV_KEY, BTN_TOUCH), Some(1));

        emit(
            &mut tracker,
            &mut writer,
            &with_phase(&recovered_move, PenTouchPhase::Up),
        )
        .unwrap();
        assert_eq!(event_at(&writer.frames[4].1, EV_KEY, BTN_TOUCH), Some(0));
        assert!(tracker.is_empty());
        emit(&mut tracker, &mut writer, &down).unwrap();
        tracker
            .release_all(|target, frame, attempted, events_written| {
                *attempted = true;
                writer.emit(target, frame, events_written)
            })
            .unwrap();
        assert!(tracker.is_empty());
        assert_eq!(event_at(&writer.frames[6].1, EV_KEY, BTN_TOUCH), Some(0));
    }

    #[test]
    fn move_after_failed_down_starts_touch_and_reserved_contacts_do_not_hold_tip() {
        let mut tracker = ContactTracker::default();
        let mut writer = FakeWriter {
            fail_next: true,
            ..FakeWriter::default()
        };
        let first_down = event(7, PenTouchDevice::Touch, PenTouchPhase::Down, 10, 20);
        let second_down = event(8, PenTouchDevice::Touch, PenTouchPhase::Down, 30, 40);
        assert!(emit(&mut tracker, &mut writer, &first_down).is_err());
        assert_eq!(writer.accepted_events[0], 0);
        writer.fail_next = true;
        assert!(emit(&mut tracker, &mut writer, &second_down).is_err());
        assert_eq!(writer.accepted_events[1], 0);

        let first_move = event(7, PenTouchDevice::Touch, PenTouchPhase::Move, 11, 21);
        writer.fail_next = true;
        assert!(emit(&mut tracker, &mut writer, &first_move).is_err());
        assert_eq!(writer.accepted_events[2], 0);
        assert!(!tracker.touches.get(&7).unwrap().contact.down_written);
        emit(&mut tracker, &mut writer, &first_move).unwrap();
        let recovered = &writer.frames[3].1;
        assert_eq!(event_at(recovered, EV_ABS, ABS_MT_TRACKING_ID), Some(0));
        assert_eq!(event_at(recovered, EV_ABS, ABS_MT_POSITION_X), Some(11));
        assert_eq!(event_at(recovered, EV_KEY, BTN_TOUCH), Some(1));
        assert!(tracker.touches.get(&7).unwrap().contact.down_written);

        emit(
            &mut tracker,
            &mut writer,
            &event(7, PenTouchDevice::Touch, PenTouchPhase::Up, 11, 21),
        )
        .unwrap();
        // No bytes reached the second reserved contact, so it cannot hold the native tip.
        assert_eq!(event_at(&writer.frames[4].1, EV_KEY, BTN_TOUCH), Some(0));

        let second_move = event(8, PenTouchDevice::Touch, PenTouchPhase::Move, 31, 41);
        emit(&mut tracker, &mut writer, &second_move).unwrap();
        assert_eq!(
            event_at(&writer.frames[5].1, EV_ABS, ABS_MT_TRACKING_ID),
            Some(1)
        );
        assert_eq!(event_at(&writer.frames[5].1, EV_KEY, BTN_TOUCH), Some(1));
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
            .release_all(|target, frame, attempted, events_written| {
                *attempted = true;
                writer.emit(target, frame, events_written)
            })
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
            .release_all(|target, frame, attempted, events_written| {
                *attempted = true;
                writer.emit(target, frame, events_written)
            })
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
