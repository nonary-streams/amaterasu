use crate::model::{InputError, InputResult};
use std::time::{Duration, Instant};

pub(crate) const GAMEPAD_SLOT_COUNT: usize = 16;
pub(crate) const BACK_BUTTON_MASK: u32 = 1 << 5;
pub(crate) const GUIDE_BUTTON_MASK: u32 = 1 << 10;
const GUIDE_PULSE: Duration = Duration::from_millis(100);

#[cfg(any(target_os = "linux", test))]
const EV_KEY: u16 = 1;
#[cfg(any(target_os = "linux", test))]
const EV_ABS: u16 = 3;

#[cfg(any(target_os = "linux", test))]
const BTN_SOUTH: u16 = 0x130;
#[cfg(any(target_os = "linux", test))]
const BTN_EAST: u16 = 0x131;
#[cfg(any(target_os = "linux", test))]
const BTN_NORTH: u16 = 0x133;
#[cfg(any(target_os = "linux", test))]
const BTN_WEST: u16 = 0x134;
#[cfg(any(target_os = "linux", test))]
const BTN_TL: u16 = 0x136;
#[cfg(any(target_os = "linux", test))]
const BTN_TR: u16 = 0x137;
#[cfg(any(target_os = "linux", test))]
const BTN_TL2: u16 = 0x138;
#[cfg(any(target_os = "linux", test))]
const BTN_TR2: u16 = 0x139;
#[cfg(any(target_os = "linux", test))]
const BTN_SELECT: u16 = 0x13a;
#[cfg(any(target_os = "linux", test))]
const BTN_START: u16 = 0x13b;
#[cfg(any(target_os = "linux", test))]
const BTN_MODE: u16 = 0x13c;
#[cfg(any(target_os = "linux", test))]
const BTN_THUMBL: u16 = 0x13d;
#[cfg(any(target_os = "linux", test))]
const BTN_THUMBR: u16 = 0x13e;
#[cfg(any(target_os = "linux", test))]
const BTN_DPAD_UP: u16 = 0x220;
#[cfg(any(target_os = "linux", test))]
const BTN_DPAD_DOWN: u16 = 0x221;
#[cfg(any(target_os = "linux", test))]
const BTN_DPAD_LEFT: u16 = 0x222;
#[cfg(any(target_os = "linux", test))]
const BTN_DPAD_RIGHT: u16 = 0x223;
#[cfg(any(target_os = "linux", test))]
const BTN_TRIGGER_HAPPY1: u16 = 0x2c0;

#[cfg(any(target_os = "linux", test))]
const ABS_X: u16 = 0x00;
#[cfg(any(target_os = "linux", test))]
const ABS_Y: u16 = 0x01;
#[cfg(any(target_os = "linux", test))]
const ABS_Z: u16 = 0x02;
#[cfg(any(target_os = "linux", test))]
const ABS_RX: u16 = 0x03;
#[cfg(any(target_os = "linux", test))]
const ABS_RY: u16 = 0x04;
#[cfg(any(target_os = "linux", test))]
const ABS_RZ: u16 = 0x05;

#[cfg(any(target_os = "linux", test))]
pub(crate) const GAMEPAD_ABSOLUTE_AXES: [(u16, i32, i32); 6] = [
    (ABS_X, i16::MIN as i32, i16::MAX as i32),
    (ABS_Y, i16::MIN as i32, i16::MAX as i32),
    (ABS_Z, 0, u8::MAX as i32),
    (ABS_RX, i16::MIN as i32, i16::MAX as i32),
    (ABS_RY, i16::MIN as i32, i16::MAX as i32),
    (ABS_RZ, 0, u8::MAX as i32),
];

// These bit values follow Vibeshine's pinned controller packet constants.
// Bit 11 and bits 22..31 have no assigned button in that protocol and are
// ignored just as Vibeshine's native button projections ignore them.
#[cfg(any(target_os = "linux", test))]
const BUTTON_MAP: [(u32, u16); 21] = [
    (1 << 0, BTN_DPAD_UP),
    (1 << 1, BTN_DPAD_DOWN),
    (1 << 2, BTN_DPAD_LEFT),
    (1 << 3, BTN_DPAD_RIGHT),
    (1 << 4, BTN_START),
    (1 << 5, BTN_SELECT),
    (1 << 6, BTN_THUMBL),
    (1 << 7, BTN_THUMBR),
    (1 << 8, BTN_TL),
    (1 << 9, BTN_TR),
    (1 << 10, BTN_MODE),
    (1 << 12, BTN_SOUTH),
    (1 << 13, BTN_EAST),
    (1 << 14, BTN_WEST),
    (1 << 15, BTN_NORTH),
    (1 << 16, BTN_TRIGGER_HAPPY1),
    (1 << 17, BTN_TRIGGER_HAPPY1 + 1),
    (1 << 18, BTN_TRIGGER_HAPPY1 + 2),
    (1 << 19, BTN_TRIGGER_HAPPY1 + 3),
    (1 << 20, BTN_TRIGGER_HAPPY1 + 4),
    (1 << 21, BTN_TRIGGER_HAPPY1 + 5),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ControllerSnapshot {
    pub buttons: u32,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GamepadMotionType {
    Accel,
    Gyro,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GamepadTouchPhase {
    Hover,
    Down,
    Up,
    Move,
    Cancel,
    ButtonOnly,
    HoverLeave,
    CancelAll,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GamepadTouchReport {
    pub touchpad_index: u8,
    pub pointer_id: u32,
    pub phase: GamepadTouchPhase,
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GamepadBatteryState {
    Unknown,
    NotPresent,
    Charging,
    Discharging,
    NotCharging,
    Full,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NativeGamepadEvent {
    pub event_type: u16,
    pub code: u16,
    pub value: i32,
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn possible_controller_state_after_write_failure(
    previous: ControllerSnapshot,
    next: ControllerSnapshot,
) -> ControllerSnapshot {
    ControllerSnapshot {
        buttons: previous.buttons | next.buttons,
        left_trigger: previous.left_trigger.max(next.left_trigger),
        right_trigger: previous.right_trigger.max(next.right_trigger),
        left_x: possible_axis(previous.left_x, next.left_x),
        left_y: possible_axis(previous.left_y, next.left_y),
        right_x: possible_axis(previous.right_x, next.right_x),
        right_y: possible_axis(previous.right_y, next.right_y),
    }
}

#[cfg(any(target_os = "linux", test))]
fn possible_axis(previous: i16, next: i16) -> i16 {
    if next == 0 { previous } else { next }
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn controller_state_events(
    previous: ControllerSnapshot,
    next: ControllerSnapshot,
) -> Vec<NativeGamepadEvent> {
    let mut events = Vec::with_capacity(BUTTON_MAP.len() + 8);
    for (mask, code) in BUTTON_MAP {
        let was_pressed = previous.buttons & mask != 0;
        let is_pressed = next.buttons & mask != 0;
        if was_pressed != is_pressed {
            events.push(NativeGamepadEvent {
                event_type: EV_KEY,
                code,
                value: i32::from(is_pressed),
            });
        }
    }

    for (was_pressed, is_pressed, code) in [
        (previous.left_trigger != 0, next.left_trigger != 0, BTN_TL2),
        (
            previous.right_trigger != 0,
            next.right_trigger != 0,
            BTN_TR2,
        ),
    ] {
        if was_pressed != is_pressed {
            events.push(NativeGamepadEvent {
                event_type: EV_KEY,
                code,
                value: i32::from(is_pressed),
            });
        }
    }

    for (previous, next, code) in [
        (previous.left_x, next.left_x, ABS_X),
        (previous.left_y, next.left_y, ABS_Y),
        (
            i16::from(previous.left_trigger),
            i16::from(next.left_trigger),
            ABS_Z,
        ),
        (previous.right_x, next.right_x, ABS_RX),
        (previous.right_y, next.right_y, ABS_RY),
        (
            i16::from(previous.right_trigger),
            i16::from(next.right_trigger),
            ABS_RZ,
        ),
    ] {
        if previous != next {
            events.push(NativeGamepadEvent {
                event_type: EV_ABS,
                code,
                value: i32::from(next),
            });
        }
    }

    events
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn gamepad_key_codes() -> impl Iterator<Item = u16> {
    BUTTON_MAP
        .into_iter()
        .map(|(_, code)| code)
        .chain([BTN_TL2, BTN_TR2])
}

pub(crate) trait GamepadDevice {
    fn update(&mut self, state: ControllerSnapshot) -> InputResult<()>;

    fn set_motion(
        &mut self,
        _motion_type: GamepadMotionType,
        _values: [f32; 3],
    ) -> InputResult<()> {
        Err(InputError::new(
            "unsupported_event",
            "the selected gamepad profile does not accept motion reports",
        ))
    }

    fn set_touch(&mut self, _touch: GamepadTouchReport) -> InputResult<()> {
        Err(InputError::new(
            "unsupported_event",
            "the selected gamepad profile does not accept touch reports",
        ))
    }

    fn set_battery(
        &mut self,
        _state: GamepadBatteryState,
        _percentage: Option<u8>,
    ) -> InputResult<()> {
        Err(InputError::new(
            "unsupported_event",
            "the selected gamepad profile does not accept battery reports",
        ))
    }

    /// A failed native write may have partially applied. Implementations that
    /// can detach the old device may replace it before the state is sent again.
    fn recover_after_update_failure(&mut self, error: InputError) -> InputResult<()> {
        Err(error)
    }

    /// Returns whether the active native report needs to be projected again.
    #[cfg(any(windows, target_os = "linux"))]
    fn set_ds4_back_as_touchpad_click(&mut self, _enabled: bool) -> bool {
        false
    }

    fn release(&mut self) -> InputResult<()> {
        self.update(ControllerSnapshot::default())
    }
}

/// Replaces a native device only after its old instance is known detached.
/// A failed detach retains ownership for cleanup; a failed new allocation
/// leaves the slot empty so a later tick can retry without using a stale fd.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn replace_failed_device<Device>(
    device: &mut Option<Device>,
    mut detach: impl FnMut(&mut Device) -> InputResult<()>,
    mut allocate: impl FnMut() -> InputResult<Device>,
) -> InputResult<()> {
    if let Some(existing) = device.as_mut() {
        detach(existing)?;
    }
    *device = None;
    *device = Some(allocate()?);
    Ok(())
}

/// Neutral delivery is best effort during teardown. Detaching the device is
/// the decisive cleanup step because it removes any partially applied state.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn release_owned_device<Device>(
    device: &mut Option<Device>,
    report_uncertain: bool,
    mut neutralize: impl FnMut(&mut Device) -> InputResult<()>,
    mut detach: impl FnMut(&mut Device) -> InputResult<()>,
) -> InputResult<()> {
    if let Some(existing) = device.as_mut() {
        // Another frame could complete a partial report if the previous
        // write failed before SYN_REPORT. Detach that device directly.
        if !report_uncertain {
            let _ = neutralize(existing);
        }
        detach(existing)?;
        *device = None;
    }
    Ok(())
}

struct GamepadSlot<Device> {
    device: Device,
    client_state: ControllerSnapshot,
    back_deadline: Option<Instant>,
    guide_pulse_pending: bool,
    guide_until: Option<Instant>,
    suppress_back: bool,
    dirty: bool,
    pending_removal: bool,
}

impl<Device: GamepadDevice> GamepadSlot<Device> {
    fn new(device: Device) -> Self {
        Self {
            device,
            client_state: ControllerSnapshot::default(),
            back_deadline: None,
            guide_pulse_pending: false,
            guide_until: None,
            suppress_back: false,
            dirty: false,
            pending_removal: false,
        }
    }

    fn update(
        &mut self,
        state: ControllerSnapshot,
        timeout_ms: i32,
        now: Instant,
    ) -> InputResult<()> {
        let back_was_pressed = self.client_state.buttons & BACK_BUTTON_MASK != 0;
        let back_is_pressed = state.buttons & BACK_BUTTON_MASK != 0;
        let defer_zero_timeout = !back_was_pressed && back_is_pressed && timeout_ms == 0;
        if !back_is_pressed {
            self.back_deadline = None;
            self.suppress_back = false;
        } else if timeout_ms < 0 {
            self.back_deadline = None;
        } else if !back_was_pressed && timeout_ms >= 0 {
            self.back_deadline = now.checked_add(Duration::from_millis(timeout_ms as u64));
        }
        self.client_state = state;
        self.expire(now, !defer_zero_timeout);
        self.write_effective_state(now)
    }

    fn advance(&mut self, now: Instant) -> InputResult<()> {
        self.expire(now, true);
        if self.dirty {
            self.write_effective_state(now)?;
        }
        Ok(())
    }

    fn expire(&mut self, now: Instant, expire_back_deadline: bool) {
        if expire_back_deadline && self.back_deadline.is_some_and(|deadline| now >= deadline) {
            self.back_deadline = None;
            if self.client_state.buttons & BACK_BUTTON_MASK != 0 {
                self.suppress_back = true;
                self.guide_pulse_pending = true;
                self.dirty = true;
            }
        }
        if self.guide_until.is_some_and(|deadline| now >= deadline) {
            self.guide_until = None;
            self.dirty = true;
        }
    }

    fn write_effective_state(&mut self, now: Instant) -> InputResult<()> {
        self.dirty = true;
        let state = self.effective_state(now);
        if let Err(error) = self.device.update(state) {
            self.device.recover_after_update_failure(error)?;
            if let Err(replay_error) = self.device.update(state) {
                // The replay target may now contain a partially applied report
                // too. Retire it before a later tick can resend this state.
                self.device
                    .recover_after_update_failure(replay_error.clone())?;
                return Err(replay_error);
            }
        }
        self.dirty = false;
        if self.guide_pulse_pending {
            self.guide_pulse_pending = false;
            self.guide_until = now.checked_add(GUIDE_PULSE);
        }
        Ok(())
    }

    fn effective_state(&self, now: Instant) -> ControllerSnapshot {
        let mut state = self.client_state;
        if self.suppress_back {
            state.buttons &= !BACK_BUTTON_MASK;
        }
        if self.guide_pulse_pending || self.guide_until.is_some_and(|deadline| now < deadline) {
            state.buttons |= GUIDE_BUTTON_MASK;
        }
        state
    }

    fn cancel_timeout(&mut self) {
        self.client_state = ControllerSnapshot::default();
        self.back_deadline = None;
        self.guide_pulse_pending = false;
        self.guide_until = None;
        self.suppress_back = false;
        self.dirty = true;
    }

    fn disable_pending_back_timeout(&mut self) {
        self.back_deadline = None;
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn set_ds4_back_as_touchpad_click(&mut self, enabled: bool) {
        // The next tick must project the new mapping even if the client keeps
        // holding Back and sends no further controller snapshot.
        self.dirty |= self.device.set_ds4_back_as_touchpad_click(enabled);
    }

    fn apply_auxiliary_report(
        &mut self,
        mut send: impl FnMut(&mut Device) -> InputResult<()>,
    ) -> InputResult<()> {
        if let Err(error) = send(&mut self.device) {
            if error.code == "invalid_event" || error.code == "unsupported_event" {
                return Err(error);
            }
            self.device.recover_after_update_failure(error.clone())?;
            if let Err(replay_error) = send(&mut self.device) {
                self.device
                    .recover_after_update_failure(replay_error.clone())?;
                return Err(replay_error);
            }
        }
        Ok(())
    }
}

/// Owns the client-relative controller slots for one input lease.
pub(crate) struct GamepadSlots<Device> {
    devices: [Option<GamepadSlot<Device>>; GAMEPAD_SLOT_COUNT],
}

impl<Device: GamepadDevice> GamepadSlots<Device> {
    pub(crate) fn new() -> Self {
        Self {
            devices: std::array::from_fn(|_| None),
        }
    }

    pub(crate) fn apply(
        &mut self,
        number: u16,
        active_mask: u16,
        state: ControllerSnapshot,
        back_button_timeout_ms: i32,
        now: Instant,
        mut create: impl FnMut(u8) -> InputResult<Device>,
    ) -> InputResult<()> {
        if usize::from(number) >= GAMEPAD_SLOT_COUNT {
            return Err(InputError::new(
                "invalid_event",
                "controller number must be in the range 0..16",
            ));
        }

        let mut first_error = None;
        for (slot, slot_device) in self.devices.iter_mut().enumerate() {
            if active_mask & (1 << slot) == 0
                && let Some(device) = slot_device.as_mut()
            {
                device.cancel_timeout();
                device.pending_removal = true;
                if let Err(error) = Self::release_slot(slot_device)
                    && first_error.is_none()
                {
                    first_error = Some(error);
                }
            }
        }

        let slot = usize::from(number);
        if active_mask & (1 << slot) == 0 {
            return first_error.map_or(Ok(()), Err);
        }
        if self.devices[slot]
            .as_ref()
            .is_some_and(|device| device.pending_removal)
        {
            Self::release_slot(&mut self.devices[slot])?;
        }
        // Existing live slots may still be updated while cleanup retries, but
        // failed removal must not allocate another native target.
        if self.devices[slot].is_none()
            && let Some(error) = first_error
        {
            return Err(error);
        }
        if self.devices[slot].is_none() {
            self.devices[slot] = Some(GamepadSlot::new(create(number as u8)?));
        }
        let update = self.devices[slot]
            .as_mut()
            .expect("active gamepad slot was created")
            .update(state, back_button_timeout_ms, now);
        first_error.map_or(update, Err)
    }

    pub(crate) fn ensure(
        &mut self,
        number: u16,
        mut create: impl FnMut(u8) -> InputResult<Device>,
    ) -> InputResult<()> {
        let Some(slot) = self.devices.get_mut(usize::from(number)) else {
            return Err(InputError::new(
                "invalid_event",
                "controller number must be in the range 0..16",
            ));
        };
        if slot.as_ref().is_some_and(|device| device.pending_removal) {
            Self::release_slot(slot)?;
        }
        if slot.is_none() {
            *slot = Some(GamepadSlot::new(create(number as u8)?));
        }
        Ok(())
    }

    pub(crate) fn remove(&mut self, number: u16) -> InputResult<()> {
        let Some(slot) = self.devices.get_mut(usize::from(number)) else {
            return Err(InputError::new(
                "invalid_event",
                "controller number must be in the range 0..16",
            ));
        };
        if let Some(device) = slot.as_mut() {
            device.cancel_timeout();
            device.pending_removal = true;
            Self::release_slot(slot)?;
        }
        Ok(())
    }

    pub(crate) fn set_motion(
        &mut self,
        number: u16,
        motion_type: GamepadMotionType,
        values: [f32; 3],
    ) -> InputResult<()> {
        self.with_slot(number, |slot| {
            slot.apply_auxiliary_report(|device| device.set_motion(motion_type, values))
        })
    }

    pub(crate) fn set_touch(&mut self, number: u16, touch: GamepadTouchReport) -> InputResult<()> {
        self.with_slot(number, |slot| {
            slot.apply_auxiliary_report(|device| device.set_touch(touch))
        })
    }

    pub(crate) fn set_battery(
        &mut self,
        number: u16,
        state: GamepadBatteryState,
        percentage: Option<u8>,
    ) -> InputResult<()> {
        self.with_slot(number, |slot| {
            slot.apply_auxiliary_report(|device| device.set_battery(state, percentage))
        })
    }

    fn with_slot(
        &mut self,
        number: u16,
        apply: impl FnOnce(&mut GamepadSlot<Device>) -> InputResult<()>,
    ) -> InputResult<()> {
        let Some(slot) = self.devices.get_mut(usize::from(number)) else {
            return Err(InputError::new(
                "invalid_event",
                "controller number must be in the range 0..16",
            ));
        };
        let slot = slot.as_mut().ok_or_else(|| {
            InputError::new(
                "controller_unavailable",
                "controller auxiliary report arrived before its controller slot was active",
            )
        })?;
        if slot.pending_removal {
            return Err(InputError::new(
                "controller_unavailable",
                "controller slot is being detached",
            ));
        }
        apply(slot)
    }

    pub(crate) fn advance(&mut self, now: Instant) -> InputResult<()> {
        let mut first_error = None;
        for slot in &mut self.devices {
            let result = if slot.as_ref().is_some_and(|device| device.pending_removal) {
                Self::release_slot(slot)
            } else if let Some(device) = slot.as_mut() {
                device.advance(now)
            } else {
                Ok(())
            };
            if let Err(error) = result
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn release_slot(slot: &mut Option<GamepadSlot<Device>>) -> InputResult<()> {
        if let Some(device) = slot.as_mut() {
            device.device.release()?;
            *slot = None;
        }
        Ok(())
    }

    pub(crate) fn disable_pending_back_timeouts(&mut self) {
        for slot in self.devices.iter_mut().flatten() {
            slot.disable_pending_back_timeout();
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.devices.iter().all(Option::is_none)
    }

    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn set_ds4_back_as_touchpad_click(&mut self, enabled: bool) {
        for slot in self.devices.iter_mut().flatten() {
            slot.set_ds4_back_as_touchpad_click(enabled);
        }
    }

    /// A shared native connection was lost. Each slot keeps its client state
    /// and timers, but must resend its effective state to a replacement device.
    #[cfg(windows)]
    pub(crate) fn invalidate_devices(&mut self, mut invalidate: impl FnMut(&mut Device)) {
        for slot in self.devices.iter_mut().flatten() {
            invalidate(&mut slot.device);
            slot.dirty = true;
        }
    }

    pub(crate) fn stop(&mut self) -> InputResult<()> {
        let mut first_error = None;
        for slot_device in &mut self.devices {
            if let Some(device) = slot_device.as_mut() {
                device.cancel_timeout();
                // A failed release still ends this slot's lease. Later ticks
                // must retry removal, never send another report to it.
                device.pending_removal = true;
                match device.device.release() {
                    Ok(()) => *slot_device = None,
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl<Device: GamepadDevice> Default for GamepadSlots<Device> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct Trace {
        created: Vec<u8>,
        updates: Vec<(u8, ControllerSnapshot)>,
        dropped: Vec<u8>,
        recoveries: usize,
    }

    struct FakeDevice {
        slot: u8,
        trace: Rc<RefCell<Trace>>,
    }

    impl GamepadDevice for FakeDevice {
        fn update(&mut self, state: ControllerSnapshot) -> InputResult<()> {
            self.trace.borrow_mut().updates.push((self.slot, state));
            Ok(())
        }

        fn recover_after_update_failure(&mut self, _error: InputError) -> InputResult<()> {
            self.trace.borrow_mut().recoveries += 1;
            Ok(())
        }
    }

    impl Drop for FakeDevice {
        fn drop(&mut self) {
            self.trace.borrow_mut().dropped.push(self.slot);
        }
    }

    fn create_device(trace: Rc<RefCell<Trace>>, slot: u8) -> InputResult<FakeDevice> {
        trace.borrow_mut().created.push(slot);
        Ok(FakeDevice { slot, trace })
    }

    fn apply_state(
        slots: &mut GamepadSlots<FakeDevice>,
        trace: &Rc<RefCell<Trace>>,
        number: u16,
        active_mask: u16,
        buttons: u32,
        timeout_ms: i32,
        now: Instant,
    ) {
        slots
            .apply(
                number,
                active_mask,
                ControllerSnapshot {
                    buttons,
                    ..ControllerSnapshot::default()
                },
                timeout_ms,
                now,
                |slot| create_device(trace.clone(), slot),
            )
            .unwrap();
    }

    fn has_button(state: ControllerSnapshot, mask: u32) -> bool {
        state.buttons & mask != 0
    }

    #[test]
    fn unsupported_auxiliary_reports_do_not_rebuild_a_live_target() {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut slots = GamepadSlots::new();
        slots
            .ensure(0, |slot| create_device(trace.clone(), slot))
            .unwrap();

        let error = slots
            .set_touch(
                0,
                GamepadTouchReport {
                    touchpad_index: 0,
                    pointer_id: 0,
                    phase: GamepadTouchPhase::Down,
                    x: 0.5,
                    y: 0.5,
                    pressure: 1.0,
                },
            )
            .unwrap_err();

        assert_eq!(error.code, "unsupported_event");
        assert_eq!(trace.borrow().recoveries, 0);
        assert_eq!(trace.borrow().created, [0]);
        assert!(trace.borrow().dropped.is_empty());
    }

    #[test]
    fn controller_mapping_covers_each_defined_button_and_releases_each_one() {
        let all_buttons = BUTTON_MAP.iter().fold(0, |flags, (mask, _)| flags | mask);
        let pressed = controller_state_events(
            ControllerSnapshot::default(),
            ControllerSnapshot {
                buttons: all_buttons,
                ..ControllerSnapshot::default()
            },
        );

        assert_eq!(pressed.len(), BUTTON_MAP.len());
        assert!(
            pressed
                .iter()
                .all(|event| event.event_type == EV_KEY && event.value == 1)
        );
        assert!(BUTTON_MAP.iter().all(|(_, code)| {
            pressed
                .iter()
                .any(|event| event.code == *code && event.value == 1)
        }));

        let released = controller_state_events(
            ControllerSnapshot {
                buttons: all_buttons,
                ..ControllerSnapshot::default()
            },
            ControllerSnapshot::default(),
        );
        assert_eq!(released.len(), BUTTON_MAP.len());
        assert!(
            released
                .iter()
                .all(|event| event.event_type == EV_KEY && event.value == 0)
        );
    }

    #[test]
    fn controller_mapping_keeps_signed_axes_and_full_trigger_ranges() {
        let events = controller_state_events(
            ControllerSnapshot::default(),
            ControllerSnapshot {
                left_trigger: u8::MAX,
                right_trigger: 1,
                left_x: i16::MIN,
                left_y: i16::MAX,
                right_x: -1,
                right_y: 1,
                ..ControllerSnapshot::default()
            },
        );

        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == EV_ABS)
                .map(|event| (event.code, event.value))
                .collect::<Vec<_>>(),
            [
                (ABS_X, i32::from(i16::MIN)),
                (ABS_Y, i32::from(i16::MAX)),
                (ABS_Z, 255),
                (ABS_RX, -1),
                (ABS_RY, 1),
                (ABS_RZ, 1),
            ]
        );
        assert!(events.contains(&NativeGamepadEvent {
            event_type: EV_KEY,
            code: BTN_TL2,
            value: 1,
        }));
        assert!(events.contains(&NativeGamepadEvent {
            event_type: EV_KEY,
            code: BTN_TR2,
            value: 1,
        }));
    }

    #[test]
    fn ambiguous_partial_writes_retain_a_state_that_can_release_every_prior_press() {
        let previous = ControllerSnapshot {
            buttons: 1 << 4,
            left_trigger: 30,
            left_x: 100,
            ..ControllerSnapshot::default()
        };
        let next = ControllerSnapshot {
            buttons: 1 << 5,
            right_trigger: 70,
            left_y: -200,
            ..ControllerSnapshot::default()
        };
        let possible = possible_controller_state_after_write_failure(previous, next);
        assert_eq!(possible.buttons, (1 << 4) | (1 << 5));
        assert_eq!(possible.left_trigger, 30);
        assert_eq!(possible.right_trigger, 70);
        assert_eq!(possible.left_x, 100);
        assert_eq!(possible.left_y, -200);

        let release = controller_state_events(possible, ControllerSnapshot::default());
        assert!(
            release
                .iter()
                .any(|event| event.code == BTN_START && event.value == 0)
        );
        assert!(
            release
                .iter()
                .any(|event| event.code == BTN_SELECT && event.value == 0)
        );
        assert!(
            release
                .iter()
                .any(|event| event.code == ABS_X && event.value == 0)
        );
        assert!(
            release
                .iter()
                .any(|event| event.code == ABS_Y && event.value == 0)
        );
    }

    #[test]
    fn unassigned_button_bits_are_ignored() {
        let events = controller_state_events(
            ControllerSnapshot::default(),
            ControllerSnapshot {
                buttons: (1 << 11) | (1 << 22) | (1 << 31),
                ..ControllerSnapshot::default()
            },
        );
        assert!(events.is_empty());
    }

    #[test]
    fn special_home_and_extended_button_bits_match_the_pinned_protocol() {
        assert!(BUTTON_MAP.contains(&(0x00000400, BTN_MODE)));
        assert!(BUTTON_MAP.contains(&(0x00010000, BTN_TRIGGER_HAPPY1)));
        assert!(BUTTON_MAP.contains(&(0x00020000, BTN_TRIGGER_HAPPY1 + 1)));
        assert!(BUTTON_MAP.contains(&(0x00040000, BTN_TRIGGER_HAPPY1 + 2)));
        assert!(BUTTON_MAP.contains(&(0x00080000, BTN_TRIGGER_HAPPY1 + 3)));
        assert!(BUTTON_MAP.contains(&(0x00100000, BTN_TRIGGER_HAPPY1 + 4)));
        assert!(BUTTON_MAP.contains(&(0x00200000, BTN_TRIGGER_HAPPY1 + 5)));
    }

    #[test]
    fn virtual_device_capabilities_cover_the_mapped_buttons_and_axes() {
        assert_eq!(
            gamepad_key_codes().collect::<Vec<_>>(),
            [
                BTN_DPAD_UP,
                BTN_DPAD_DOWN,
                BTN_DPAD_LEFT,
                BTN_DPAD_RIGHT,
                BTN_START,
                BTN_SELECT,
                BTN_THUMBL,
                BTN_THUMBR,
                BTN_TL,
                BTN_TR,
                BTN_MODE,
                BTN_SOUTH,
                BTN_EAST,
                BTN_WEST,
                BTN_NORTH,
                BTN_TRIGGER_HAPPY1,
                BTN_TRIGGER_HAPPY1 + 1,
                BTN_TRIGGER_HAPPY1 + 2,
                BTN_TRIGGER_HAPPY1 + 3,
                BTN_TRIGGER_HAPPY1 + 4,
                BTN_TRIGGER_HAPPY1 + 5,
                BTN_TL2,
                BTN_TR2,
            ]
        );
        assert_eq!(
            GAMEPAD_ABSOLUTE_AXES,
            [
                (ABS_X, -32768, 32767),
                (ABS_Y, -32768, 32767),
                (ABS_Z, 0, 255),
                (ABS_RX, -32768, 32767),
                (ABS_RY, -32768, 32767),
                (ABS_RZ, 0, 255),
            ]
        );
    }

    #[test]
    fn lease_slots_are_created_by_client_number_removed_by_mask_and_released_on_stop() {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut slots = GamepadSlots::new();
        let state = ControllerSnapshot {
            buttons: 1,
            ..ControllerSnapshot::default()
        };

        slots
            .apply(
                0,
                0b1000_0000_0000_0001,
                state,
                -1,
                Instant::now(),
                |slot| create_device(trace.clone(), slot),
            )
            .unwrap();
        slots
            .apply(
                15,
                0b1000_0000_0000_0001,
                state,
                -1,
                Instant::now(),
                |slot| create_device(trace.clone(), slot),
            )
            .unwrap();
        assert_eq!(trace.borrow().created, [0, 15]);
        assert_eq!(trace.borrow().updates, [(0, state), (15, state)]);

        slots
            .apply(
                0,
                1,
                ControllerSnapshot::default(),
                -1,
                Instant::now(),
                |slot| create_device(trace.clone(), slot),
            )
            .unwrap();
        assert_eq!(trace.borrow().dropped, [15]);
        assert_eq!(
            trace.borrow().updates.last(),
            Some(&(0, ControllerSnapshot::default()))
        );

        slots.stop().unwrap();
        assert_eq!(trace.borrow().dropped, [15, 0]);
        assert_eq!(
            trace.borrow().updates.last(),
            Some(&(0, ControllerSnapshot::default()))
        );
    }

    #[test]
    fn inactive_packets_remove_stale_slots_without_allocating_the_current_slot() {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut slots = GamepadSlots::new();
        slots
            .apply(
                3,
                1 << 3,
                ControllerSnapshot::default(),
                -1,
                Instant::now(),
                |slot| create_device(trace.clone(), slot),
            )
            .unwrap();
        slots
            .apply(
                3,
                0,
                ControllerSnapshot::default(),
                -1,
                Instant::now(),
                |slot| create_device(trace.clone(), slot),
            )
            .unwrap();

        assert_eq!(trace.borrow().created, [3]);
        assert_eq!(trace.borrow().dropped, [3]);
    }

    #[test]
    fn failed_neutral_release_retains_the_slot_for_cleanup_retry() {
        struct FailingDevice {
            fail_release: Rc<std::cell::Cell<bool>>,
        }
        impl GamepadDevice for FailingDevice {
            fn update(&mut self, state: ControllerSnapshot) -> InputResult<()> {
                if state == ControllerSnapshot::default() && self.fail_release.get() {
                    Err(InputError::new("injection_failed", "test release failure"))
                } else {
                    Ok(())
                }
            }
        }

        let fail_release = Rc::new(std::cell::Cell::new(true));
        let mut slots = GamepadSlots::new();
        slots
            .apply(
                0,
                1,
                ControllerSnapshot {
                    buttons: 1,
                    ..ControllerSnapshot::default()
                },
                -1,
                Instant::now(),
                |_| {
                    Ok(FailingDevice {
                        fail_release: fail_release.clone(),
                    })
                },
            )
            .unwrap();

        assert_eq!(slots.stop().unwrap_err().code, "injection_failed");
        assert!(!slots.is_empty());
        fail_release.set(false);
        slots.stop().unwrap();
        assert!(slots.is_empty());
    }

    #[test]
    fn failed_removal_blocks_new_target_until_detach_then_releases_new_target() {
        struct NativeTarget {
            number: u8,
            fail_first_release: Rc<std::cell::Cell<bool>>,
            history: Rc<RefCell<Vec<(u8, ControllerSnapshot)>>>,
            destroyed: Rc<RefCell<Vec<u8>>>,
        }

        impl GamepadDevice for NativeTarget {
            fn update(&mut self, state: ControllerSnapshot) -> InputResult<()> {
                self.history.borrow_mut().push((self.number, state));
                if state == ControllerSnapshot::default()
                    && self.number == 0
                    && self.fail_first_release.replace(false)
                {
                    return Err(InputError::new(
                        "detach_failed",
                        "old target still attached",
                    ));
                }
                Ok(())
            }
        }

        impl Drop for NativeTarget {
            fn drop(&mut self) {
                self.destroyed.borrow_mut().push(self.number);
            }
        }

        let fail_first_release = Rc::new(std::cell::Cell::new(true));
        let history = Rc::new(RefCell::new(Vec::new()));
        let destroyed = Rc::new(RefCell::new(Vec::new()));
        let allocated = Rc::new(RefCell::new(Vec::new()));
        let mut slots = GamepadSlots::new();
        let pressed = ControllerSnapshot {
            buttons: 1,
            ..ControllerSnapshot::default()
        };
        let now = Instant::now();
        let mut create = |number| {
            allocated.borrow_mut().push(number);
            Ok(NativeTarget {
                number,
                fail_first_release: fail_first_release.clone(),
                history: history.clone(),
                destroyed: destroyed.clone(),
            })
        };

        slots.apply(0, 1, pressed, -1, now, &mut create).unwrap();
        assert_eq!(
            slots
                .apply(1, 1 << 1, pressed, -1, now, &mut create)
                .unwrap_err()
                .code,
            "detach_failed"
        );
        assert_eq!(*allocated.borrow(), [0], "new target must wait for detach");
        assert!(destroyed.borrow().is_empty());

        slots
            .apply(1, 1 << 1, pressed, -1, now, &mut create)
            .unwrap();
        assert_eq!(*allocated.borrow(), [0, 1]);
        assert_eq!(*destroyed.borrow(), [0]);
        assert_eq!(history.borrow().last(), Some(&(1, pressed)));

        slots.stop().unwrap();
        assert_eq!(*destroyed.borrow(), [0, 1]);
        assert_eq!(
            history.borrow().last(),
            Some(&(1, ControllerSnapshot::default()))
        );
    }

    #[test]
    fn back_timeout_releases_back_pulses_guide_and_suppresses_until_release() {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut slots = GamepadSlots::new();
        let start = Instant::now();

        apply_state(&mut slots, &trace, 2, 1 << 2, BACK_BUTTON_MASK, 200, start);
        slots.advance(start + Duration::from_millis(199)).unwrap();
        assert_eq!(trace.borrow().updates.len(), 1);
        assert!(has_button(trace.borrow().updates[0].1, BACK_BUTTON_MASK));

        slots.advance(start + Duration::from_millis(200)).unwrap();
        let pulse = trace.borrow().updates.last().unwrap().1;
        assert!(!has_button(pulse, BACK_BUTTON_MASK));
        assert!(has_button(pulse, GUIDE_BUTTON_MASK));

        apply_state(
            &mut slots,
            &trace,
            2,
            1 << 2,
            BACK_BUTTON_MASK,
            200,
            start + Duration::from_millis(210),
        );
        assert!(!has_button(
            trace.borrow().updates.last().unwrap().1,
            BACK_BUTTON_MASK
        ));
        assert!(has_button(
            trace.borrow().updates.last().unwrap().1,
            GUIDE_BUTTON_MASK
        ));

        slots.advance(start + Duration::from_millis(300)).unwrap();
        let guide_released = trace.borrow().updates.last().unwrap().1;
        assert!(!has_button(guide_released, BACK_BUTTON_MASK));
        assert!(!has_button(guide_released, GUIDE_BUTTON_MASK));

        apply_state(
            &mut slots,
            &trace,
            2,
            1 << 2,
            0,
            200,
            start + Duration::from_millis(310),
        );
        apply_state(
            &mut slots,
            &trace,
            2,
            1 << 2,
            BACK_BUTTON_MASK,
            200,
            start + Duration::from_millis(320),
        );
        slots.advance(start + Duration::from_millis(520)).unwrap();
        let second_pulse = trace.borrow().updates.last().unwrap().1;
        assert!(!has_button(second_pulse, BACK_BUTTON_MASK));
        assert!(has_button(second_pulse, GUIDE_BUTTON_MASK));
    }

    #[test]
    fn failed_timeout_writes_retry_back_release_and_guide_pulse_release() {
        struct IntermittentDevice {
            trace: Rc<RefCell<Vec<ControllerSnapshot>>>,
            failures_remaining: Rc<std::cell::Cell<usize>>,
        }

        impl GamepadDevice for IntermittentDevice {
            fn update(&mut self, state: ControllerSnapshot) -> InputResult<()> {
                self.trace.borrow_mut().push(state);
                let failures = self.failures_remaining.get();
                if failures > 0 {
                    self.failures_remaining.set(failures - 1);
                    Err(InputError::new(
                        "injection_failed",
                        "temporary write failure",
                    ))
                } else {
                    Ok(())
                }
            }
        }

        let start = Instant::now();
        let trace = Rc::new(RefCell::new(Vec::new()));
        let failures_remaining = Rc::new(std::cell::Cell::new(0));
        let mut slots = GamepadSlots::new();
        slots
            .apply(
                0,
                1,
                ControllerSnapshot {
                    buttons: BACK_BUTTON_MASK,
                    ..ControllerSnapshot::default()
                },
                100,
                start,
                |_| {
                    Ok(IntermittentDevice {
                        trace: trace.clone(),
                        failures_remaining: failures_remaining.clone(),
                    })
                },
            )
            .unwrap();

        failures_remaining.set(1);
        let timeout = start + Duration::from_millis(100);
        assert_eq!(slots.advance(timeout).unwrap_err().code, "injection_failed");
        let failed_back_release = *trace.borrow().last().unwrap();
        assert!(!has_button(failed_back_release, BACK_BUTTON_MASK));
        assert!(has_button(failed_back_release, GUIDE_BUTTON_MASK));

        slots.advance(timeout + Duration::from_millis(1)).unwrap();
        let delivered_back_release = *trace.borrow().last().unwrap();
        assert!(!has_button(delivered_back_release, BACK_BUTTON_MASK));
        assert!(has_button(delivered_back_release, GUIDE_BUTTON_MASK));

        let pulse_end = timeout + Duration::from_millis(101);
        failures_remaining.set(1);
        assert_eq!(
            slots.advance(pulse_end).unwrap_err().code,
            "injection_failed"
        );
        assert!(!has_button(
            *trace.borrow().last().unwrap(),
            GUIDE_BUTTON_MASK
        ));

        slots.advance(pulse_end + Duration::from_millis(1)).unwrap();
        assert!(!has_button(
            *trace.borrow().last().unwrap(),
            BACK_BUTTON_MASK
        ));
        assert!(!has_button(
            *trace.borrow().last().unwrap(),
            GUIDE_BUTTON_MASK
        ));
    }

    #[test]
    fn back_release_before_deadline_cancels_the_guide_pulse() {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut slots = GamepadSlots::new();
        let start = Instant::now();

        apply_state(&mut slots, &trace, 0, 1, BACK_BUTTON_MASK, 300, start);
        apply_state(
            &mut slots,
            &trace,
            0,
            1,
            0,
            300,
            start + Duration::from_millis(299),
        );
        slots.advance(start + Duration::from_secs(1)).unwrap();

        assert!(
            trace
                .borrow()
                .updates
                .iter()
                .all(|(_, state)| { !has_button(*state, GUIDE_BUTTON_MASK) })
        );
        assert!(!has_button(
            trace.borrow().updates.last().unwrap().1,
            BACK_BUTTON_MASK
        ));
    }

    #[test]
    fn disabled_back_timeout_never_schedules_guide() {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut slots = GamepadSlots::new();
        let start = Instant::now();

        apply_state(&mut slots, &trace, 4, 1 << 4, BACK_BUTTON_MASK, -1, start);
        slots.advance(start + Duration::from_secs(60)).unwrap();

        assert_eq!(trace.borrow().updates.len(), 1);
        assert!(has_button(trace.borrow().updates[0].1, BACK_BUTTON_MASK));
    }

    #[test]
    fn disabling_timeout_while_back_is_held_cancels_only_the_pending_deadline() {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut slots = GamepadSlots::new();
        let start = Instant::now();

        apply_state(&mut slots, &trace, 4, 1 << 4, BACK_BUTTON_MASK, 100, start);
        slots.disable_pending_back_timeouts();
        slots.advance(start + Duration::from_secs(1)).unwrap();

        assert!(has_button(
            trace.borrow().updates.last().unwrap().1,
            BACK_BUTTON_MASK
        ));
        assert!(
            trace
                .borrow()
                .updates
                .iter()
                .all(|(_, state)| { !has_button(*state, GUIDE_BUTTON_MASK) })
        );
    }

    #[test]
    fn disabling_timeout_after_expiry_preserves_guide_pulse_and_back_suppression() {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut slots = GamepadSlots::new();
        let start = Instant::now();

        apply_state(&mut slots, &trace, 4, 1 << 4, BACK_BUTTON_MASK, 10, start);
        slots.advance(start + Duration::from_millis(10)).unwrap();
        slots.disable_pending_back_timeouts();
        let pulse = trace.borrow().updates.last().unwrap().1;
        assert!(!has_button(pulse, BACK_BUTTON_MASK));
        assert!(has_button(pulse, GUIDE_BUTTON_MASK));

        slots.advance(start + Duration::from_millis(110)).unwrap();
        let pulse_end = trace.borrow().updates.last().unwrap().1;
        assert!(!has_button(pulse_end, BACK_BUTTON_MASK));
        assert!(!has_button(pulse_end, GUIDE_BUTTON_MASK));

        apply_state(
            &mut slots,
            &trace,
            4,
            1 << 4,
            0,
            -1,
            start + Duration::from_millis(111),
        );
        apply_state(
            &mut slots,
            &trace,
            4,
            1 << 4,
            BACK_BUTTON_MASK,
            -1,
            start + Duration::from_millis(112),
        );
        assert!(has_button(
            trace.borrow().updates.last().unwrap().1,
            BACK_BUTTON_MASK
        ));
    }

    #[test]
    fn back_timeout_is_independent_for_each_client_slot() {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut slots = GamepadSlots::new();
        let start = Instant::now();

        apply_state(&mut slots, &trace, 1, 0b11, BACK_BUTTON_MASK, 100, start);
        apply_state(
            &mut slots,
            &trace,
            0,
            0b11,
            BACK_BUTTON_MASK,
            100,
            start + Duration::from_millis(50),
        );
        slots.advance(start + Duration::from_millis(100)).unwrap();
        assert!(has_button(
            trace.borrow().updates.last().unwrap().1,
            GUIDE_BUTTON_MASK
        ));
        assert_eq!(trace.borrow().updates.last().unwrap().0, 1);

        slots.advance(start + Duration::from_millis(150)).unwrap();
        assert!(has_button(
            trace.borrow().updates.last().unwrap().1,
            GUIDE_BUTTON_MASK
        ));
        assert_eq!(trace.borrow().updates.last().unwrap().0, 0);
    }

    #[test]
    fn slot_removal_and_reuse_cancel_an_old_back_deadline() {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut slots = GamepadSlots::new();
        let start = Instant::now();

        apply_state(&mut slots, &trace, 3, 1 << 3, BACK_BUTTON_MASK, 100, start);
        apply_state(
            &mut slots,
            &trace,
            3,
            0,
            0,
            100,
            start + Duration::from_millis(10),
        );
        apply_state(
            &mut slots,
            &trace,
            3,
            1 << 3,
            1,
            100,
            start + Duration::from_millis(20),
        );
        slots.advance(start + Duration::from_millis(200)).unwrap();

        assert_eq!(trace.borrow().created, [3, 3]);
        assert_eq!(trace.borrow().dropped, [3]);
        assert!(
            trace
                .borrow()
                .updates
                .iter()
                .all(|(_, state)| { !has_button(*state, GUIDE_BUTTON_MASK) })
        );
    }

    #[test]
    fn lease_stop_cancels_back_deadlines_before_a_later_lease_reuses_the_slot() {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut slots = GamepadSlots::new();
        let start = Instant::now();

        apply_state(&mut slots, &trace, 1, 1 << 1, BACK_BUTTON_MASK, 100, start);
        slots.stop().unwrap();
        slots.advance(start + Duration::from_secs(1)).unwrap();
        apply_state(
            &mut slots,
            &trace,
            1,
            1 << 1,
            1,
            100,
            start + Duration::from_secs(2),
        );
        slots.advance(start + Duration::from_secs(3)).unwrap();

        assert_eq!(trace.borrow().created, [1, 1]);
        assert!(
            trace
                .borrow()
                .updates
                .iter()
                .all(|(_, state)| { !has_button(*state, GUIDE_BUTTON_MASK) })
        );
    }

    #[test]
    fn zero_timeout_delivers_back_before_same_time_timer_advance_and_suppresses_until_release() {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut slots = GamepadSlots::new();
        let start = Instant::now();

        apply_state(&mut slots, &trace, 0, 1, BACK_BUTTON_MASK, 0, start);
        let initiating_state = trace.borrow().updates[0].1;
        assert!(has_button(initiating_state, BACK_BUTTON_MASK));
        assert!(!has_button(initiating_state, GUIDE_BUTTON_MASK));

        slots.advance(start).unwrap();
        let guide_pulse = trace.borrow().updates.last().unwrap().1;
        assert!(!has_button(guide_pulse, BACK_BUTTON_MASK));
        assert!(has_button(guide_pulse, GUIDE_BUTTON_MASK));

        slots.advance(start + Duration::from_millis(100)).unwrap();
        let pulse_end = trace.borrow().updates.last().unwrap().1;
        assert!(!has_button(pulse_end, BACK_BUTTON_MASK));
        assert!(!has_button(pulse_end, GUIDE_BUTTON_MASK));

        apply_state(
            &mut slots,
            &trace,
            0,
            1,
            BACK_BUTTON_MASK,
            0,
            start + Duration::from_millis(101),
        );
        let held_state = trace.borrow().updates.last().unwrap().1;
        assert!(!has_button(held_state, BACK_BUTTON_MASK));
        assert!(!has_button(held_state, GUIDE_BUTTON_MASK));

        apply_state(
            &mut slots,
            &trace,
            0,
            1,
            0,
            0,
            start + Duration::from_millis(102),
        );
        apply_state(
            &mut slots,
            &trace,
            0,
            1,
            BACK_BUTTON_MASK,
            0,
            start + Duration::from_millis(103),
        );
        let repressed_state = trace.borrow().updates.last().unwrap().1;
        assert!(has_button(repressed_state, BACK_BUTTON_MASK));
        assert!(!has_button(repressed_state, GUIDE_BUTTON_MASK));
    }

    #[test]
    fn failed_timeout_write_retries_and_starts_guide_pulse_after_delivery() {
        struct FlakyDevice {
            fail_next: std::rc::Rc<std::cell::Cell<bool>>,
            updates: std::rc::Rc<RefCell<Vec<ControllerSnapshot>>>,
        }
        impl GamepadDevice for FlakyDevice {
            fn update(&mut self, state: ControllerSnapshot) -> InputResult<()> {
                self.updates.borrow_mut().push(state);
                if self.fail_next.replace(false) {
                    Err(InputError::new(
                        "injection_failed",
                        "injected write failure",
                    ))
                } else {
                    Ok(())
                }
            }
        }

        let fail_next = std::rc::Rc::new(std::cell::Cell::new(true));
        let updates = std::rc::Rc::new(RefCell::new(Vec::new()));
        let mut slots = GamepadSlots::new();
        let start = Instant::now();
        let result = slots.apply(
            0,
            1,
            ControllerSnapshot {
                buttons: BACK_BUTTON_MASK,
                ..ControllerSnapshot::default()
            },
            0,
            start,
            |_| {
                Ok(FlakyDevice {
                    fail_next: fail_next.clone(),
                    updates: updates.clone(),
                })
            },
        );

        assert_eq!(result.unwrap_err().code, "injection_failed");
        assert!(has_button(updates.borrow()[0], BACK_BUTTON_MASK));
        assert!(!has_button(updates.borrow()[0], GUIDE_BUTTON_MASK));
        slots.advance(start + Duration::from_millis(50)).unwrap();
        assert!(!has_button(updates.borrow()[1], BACK_BUTTON_MASK));
        assert!(has_button(updates.borrow()[1], GUIDE_BUTTON_MASK));
        slots.advance(start + Duration::from_millis(149)).unwrap();
        assert_eq!(updates.borrow().len(), 2);
        slots.advance(start + Duration::from_millis(150)).unwrap();
        assert!(!has_button(
            *updates.borrow().last().unwrap(),
            GUIDE_BUTTON_MASK
        ));
    }

    #[test]
    fn failed_target_update_detaches_before_replacement_and_replays_current_state() {
        struct RecoverableDevice {
            trace: Rc<RefCell<Vec<&'static str>>>,
            fail_update: Rc<std::cell::Cell<bool>>,
        }
        impl GamepadDevice for RecoverableDevice {
            fn update(&mut self, state: ControllerSnapshot) -> InputResult<()> {
                assert_eq!(state.buttons, BACK_BUTTON_MASK);
                self.trace.borrow_mut().push("update");
                if self.fail_update.replace(false) {
                    Err(InputError::new("controller_unavailable", "stale target"))
                } else {
                    Ok(())
                }
            }

            fn recover_after_update_failure(&mut self, _: InputError) -> InputResult<()> {
                self.trace.borrow_mut().extend(["detach", "allocate"]);
                Ok(())
            }

            fn release(&mut self) -> InputResult<()> {
                self.trace.borrow_mut().push("release");
                Ok(())
            }
        }

        let trace = Rc::new(RefCell::new(Vec::new()));
        let fail_update = Rc::new(std::cell::Cell::new(true));
        let mut slots = GamepadSlots::new();
        slots
            .apply(
                0,
                1,
                ControllerSnapshot {
                    buttons: BACK_BUTTON_MASK,
                    ..ControllerSnapshot::default()
                },
                -1,
                Instant::now(),
                |_| {
                    Ok(RecoverableDevice {
                        trace: trace.clone(),
                        fail_update: fail_update.clone(),
                    })
                },
            )
            .unwrap();
        assert_eq!(
            &*trace.borrow(),
            &["update", "detach", "allocate", "update"]
        );
        slots.stop().unwrap();
        assert_eq!(trace.borrow().last(), Some(&"release"));
        assert!(slots.is_empty());
    }

    #[test]
    fn failed_replay_retires_its_target_before_a_later_write() {
        struct ReplayedDevice {
            number: u8,
            trace: Rc<RefCell<Vec<(u8, &'static str)>>>,
            failures_left: u8,
            detach_count: u8,
            replacement_pending: bool,
            uncertain: bool,
        }

        impl ReplayedDevice {
            fn replace(&mut self) -> InputResult<()> {
                if self.uncertain {
                    self.trace.borrow_mut().push((self.number, "detach"));
                    self.detach_count += 1;
                    if self.number == 0 && self.detach_count == 2 {
                        return Err(InputError::new("release_failed", "detach failed"));
                    }
                    self.uncertain = false;
                }
                self.trace.borrow_mut().push((self.number, "allocate"));
                self.replacement_pending = false;
                Ok(())
            }
        }

        impl GamepadDevice for ReplayedDevice {
            fn update(&mut self, _: ControllerSnapshot) -> InputResult<()> {
                if self.replacement_pending {
                    self.replace()?;
                }
                assert!(!self.uncertain, "must detach before another native report");
                self.trace.borrow_mut().push((self.number, "write"));
                if self.failures_left > 0 {
                    self.failures_left -= 1;
                    self.uncertain = true;
                    return Err(InputError::new("injection_failed", "partial report"));
                }
                Ok(())
            }

            fn recover_after_update_failure(&mut self, _: InputError) -> InputResult<()> {
                self.replacement_pending = true;
                self.replace()
            }

            fn release(&mut self) -> InputResult<()> {
                if self.uncertain {
                    self.trace.borrow_mut().push((self.number, "detach"));
                    self.uncertain = false;
                }
                self.trace.borrow_mut().push((self.number, "release"));
                Ok(())
            }
        }

        let trace = Rc::new(RefCell::new(Vec::new()));
        let mut slots = GamepadSlots::new();
        let now = Instant::now();
        let mut create = |number| {
            Ok(ReplayedDevice {
                number,
                trace: trace.clone(),
                failures_left: if number == 0 { 2 } else { 0 },
                detach_count: 0,
                replacement_pending: false,
                uncertain: false,
            })
        };
        assert_eq!(
            slots
                .apply(0, 1, ControllerSnapshot::default(), -1, now, &mut create)
                .unwrap_err()
                .code,
            "release_failed"
        );
        slots
            .apply(1, 3, ControllerSnapshot::default(), -1, now, &mut create)
            .unwrap();
        assert_eq!(
            &*trace.borrow(),
            &[
                (0, "write"),
                (0, "detach"),
                (0, "allocate"),
                (0, "write"),
                (0, "detach"),
                (1, "write"),
            ]
        );

        slots.advance(now).unwrap();
        assert_eq!(
            &trace.borrow()[6..],
            &[(0, "detach"), (0, "allocate"), (0, "write")]
        );
        slots.stop().unwrap();
        assert!(slots.is_empty());
    }

    #[test]
    fn failed_recovery_remains_owned_until_lease_release_succeeds() {
        struct PendingDetach {
            detach_fails: Rc<std::cell::Cell<bool>>,
            replacements: Rc<std::cell::Cell<usize>>,
        }
        impl GamepadDevice for PendingDetach {
            fn update(&mut self, _: ControllerSnapshot) -> InputResult<()> {
                Err(InputError::new("controller_unavailable", "stale target"))
            }

            fn recover_after_update_failure(&mut self, _: InputError) -> InputResult<()> {
                if self.detach_fails.get() {
                    return Err(InputError::new("controller_unavailable", "detach failed"));
                }
                self.replacements.set(self.replacements.get() + 1);
                Ok(())
            }

            fn release(&mut self) -> InputResult<()> {
                if self.detach_fails.get() {
                    Err(InputError::new("controller_unavailable", "detach failed"))
                } else {
                    Ok(())
                }
            }
        }

        let detach_fails = Rc::new(std::cell::Cell::new(true));
        let replacements = Rc::new(std::cell::Cell::new(0));
        let mut slots = GamepadSlots::new();
        assert_eq!(
            slots
                .apply(
                    0,
                    1,
                    ControllerSnapshot::default(),
                    -1,
                    Instant::now(),
                    |_| {
                        Ok(PendingDetach {
                            detach_fails: detach_fails.clone(),
                            replacements: replacements.clone(),
                        })
                    },
                )
                .unwrap_err()
                .code,
            "controller_unavailable"
        );
        assert_eq!(slots.stop().unwrap_err().code, "controller_unavailable");
        assert!(!slots.is_empty());
        assert_eq!(replacements.get(), 0);
        detach_fails.set(false);
        slots.stop().unwrap();
        assert!(slots.is_empty());
        assert_eq!(replacements.get(), 0);
    }

    #[test]
    fn failed_slot_removal_retries_without_replaying_retired_state() {
        struct Device {
            number: u8,
            trace: Rc<RefCell<Vec<(u8, &'static str)>>>,
            remaining_release_failures: Rc<std::cell::Cell<u8>>,
        }
        impl GamepadDevice for Device {
            fn update(&mut self, _: ControllerSnapshot) -> InputResult<()> {
                self.trace.borrow_mut().push((self.number, "update"));
                Ok(())
            }

            fn release(&mut self) -> InputResult<()> {
                self.trace.borrow_mut().push((self.number, "release"));
                if self.number == 0 && self.remaining_release_failures.get() > 0 {
                    self.remaining_release_failures
                        .set(self.remaining_release_failures.get() - 1);
                    return Err(InputError::new("controller_unavailable", "detach failed"));
                }
                Ok(())
            }
        }

        let trace = Rc::new(RefCell::new(Vec::new()));
        let remaining_release_failures = Rc::new(std::cell::Cell::new(3));
        let allocations = Rc::new(std::cell::Cell::new(0));
        let mut slots = GamepadSlots::new();
        let now = Instant::now();
        let mut create = |number| {
            allocations.set(allocations.get() + 1);
            Ok(Device {
                number,
                trace: trace.clone(),
                remaining_release_failures: remaining_release_failures.clone(),
            })
        };

        slots
            .apply(0, 1, ControllerSnapshot::default(), -1, now, &mut create)
            .unwrap();
        slots
            .apply(1, 3, ControllerSnapshot::default(), -1, now, &mut create)
            .unwrap();
        assert_eq!(
            slots
                .apply(1, 2, ControllerSnapshot::default(), -1, now, &mut create)
                .unwrap_err()
                .code,
            "controller_unavailable"
        );
        assert_eq!(
            &*trace.borrow(),
            &[(0, "update"), (1, "update"), (0, "release"), (1, "update")]
        );

        assert_eq!(
            slots
                .apply(0, 3, ControllerSnapshot::default(), -1, now, &mut create)
                .unwrap_err()
                .code,
            "controller_unavailable"
        );
        assert_eq!(allocations.get(), 2);
        assert_eq!(slots.stop().unwrap_err().code, "controller_unavailable");
        assert!(!slots.is_empty());

        slots.advance(now + Duration::from_millis(100)).unwrap();
        assert_eq!(trace.borrow().last(), Some(&(0, "release")));
        assert_eq!(allocations.get(), 2);
        slots
            .apply(0, 1, ControllerSnapshot::default(), -1, now, &mut create)
            .unwrap();
        assert_eq!(allocations.get(), 3);
        slots.stop().unwrap();
        assert!(slots.is_empty());
    }

    #[test]
    fn replacement_allocation_failure_recovers_on_tick_without_losing_state() {
        struct ReplacingDevice {
            trace: Rc<RefCell<Vec<&'static str>>>,
            attached: bool,
            fail_update: bool,
            fail_allocation: bool,
        }
        impl GamepadDevice for ReplacingDevice {
            fn update(&mut self, state: ControllerSnapshot) -> InputResult<()> {
                assert_eq!(state.buttons, BACK_BUTTON_MASK);
                self.trace.borrow_mut().push("update");
                if !self.attached || std::mem::take(&mut self.fail_update) {
                    Err(InputError::new("controller_unavailable", "stale target"))
                } else {
                    Ok(())
                }
            }

            fn recover_after_update_failure(&mut self, _: InputError) -> InputResult<()> {
                if self.attached {
                    self.trace.borrow_mut().push("detach");
                    self.attached = false;
                }
                self.trace.borrow_mut().push("allocate");
                if std::mem::take(&mut self.fail_allocation) {
                    return Err(InputError::new(
                        "controller_unavailable",
                        "allocation failed",
                    ));
                }
                self.attached = true;
                Ok(())
            }

            fn release(&mut self) -> InputResult<()> {
                self.trace.borrow_mut().push("release");
                self.attached = false;
                Ok(())
            }
        }

        let trace = Rc::new(RefCell::new(Vec::new()));
        let mut slots = GamepadSlots::new();
        let start = Instant::now();
        assert_eq!(
            slots
                .apply(
                    0,
                    1,
                    ControllerSnapshot {
                        buttons: BACK_BUTTON_MASK,
                        ..ControllerSnapshot::default()
                    },
                    -1,
                    start,
                    |_| {
                        Ok(ReplacingDevice {
                            trace: trace.clone(),
                            attached: true,
                            fail_update: true,
                            fail_allocation: true,
                        })
                    },
                )
                .unwrap_err()
                .code,
            "controller_unavailable"
        );
        assert_eq!(&*trace.borrow(), &["update", "detach", "allocate"]);
        slots.advance(start + Duration::from_millis(100)).unwrap();
        assert_eq!(
            &*trace.borrow(),
            &[
                "update", "detach", "allocate", "update", "allocate", "update"
            ]
        );
        slots.stop().unwrap();
        assert_eq!(trace.borrow().last(), Some(&"release"));
        assert!(slots.is_empty());
    }

    #[test]
    fn failed_uinput_detach_retains_device_and_blocks_replacement() {
        let mut device = Some(1_u8);
        let mut allocations = 0;
        let error = replace_failed_device(
            &mut device,
            |_| Err(InputError::new("release_failed", "detach failed")),
            || {
                allocations += 1;
                Ok(2)
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "release_failed");
        assert_eq!(device, Some(1));
        assert_eq!(allocations, 0);
    }

    #[test]
    fn failed_uinput_allocation_leaves_no_old_device_and_allows_retry() {
        let mut device = Some(1_u8);
        let mut trace = Vec::new();
        let error = replace_failed_device(
            &mut device,
            |old| {
                trace.push(format!("detach {old}"));
                Ok(())
            },
            || {
                Err(InputError::new(
                    "controller_unavailable",
                    "allocation failed",
                ))
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "controller_unavailable");
        assert_eq!(device, None);
        assert_eq!(trace, ["detach 1"]);

        replace_failed_device(&mut device, |_| unreachable!(), || Ok(2)).unwrap();
        assert_eq!(device, Some(2));
    }

    #[test]
    fn failed_uinput_neutral_write_still_detaches_on_release() {
        let mut device = Some(1_u8);
        let trace = Rc::new(RefCell::new(Vec::new()));
        let neutral_trace = trace.clone();
        release_owned_device(
            &mut device,
            false,
            |_| {
                neutral_trace.borrow_mut().push("neutral failed");
                Err(InputError::new("injection_failed", "write failed"))
            },
            |_| {
                trace.borrow_mut().push("detach");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(&*trace.borrow(), &["neutral failed", "detach"]);
        assert_eq!(device, None);
    }

    #[test]
    fn failed_uinput_detach_retains_device_for_release_retry() {
        let mut device = Some(1_u8);
        let error = release_owned_device(
            &mut device,
            false,
            |_| Err(InputError::new("injection_failed", "write failed")),
            |_| Err(InputError::new("release_failed", "destroy failed")),
        )
        .unwrap_err();
        assert_eq!(error.code, "release_failed");
        assert_eq!(device, Some(1));
        release_owned_device(&mut device, false, |_| Ok(()), |_| Ok(())).unwrap();
        assert_eq!(device, None);
    }

    #[test]
    fn uncertain_uinput_slot_detaches_without_another_frame_and_keeps_peer_active() {
        use std::cell::Cell;

        struct FakeUinput {
            number: u8,
            device: Option<u8>,
            uncertain: bool,
            fail_first_write: Rc<Cell<bool>>,
            remaining_detach_failures: Rc<Cell<u8>>,
            trace: Rc<RefCell<Vec<(u8, &'static str)>>>,
        }

        impl GamepadDevice for FakeUinput {
            fn update(&mut self, _: ControllerSnapshot) -> InputResult<()> {
                if self.uncertain {
                    panic!("an uncertain uinput descriptor must not receive another frame");
                }
                if self.number == 0 && self.fail_first_write.replace(false) {
                    self.uncertain = true;
                    self.trace.borrow_mut().push((self.number, "partial write"));
                    return Err(InputError::new("injection_failed", "partial report"));
                }
                self.trace.borrow_mut().push((self.number, "update"));
                Ok(())
            }

            fn recover_after_update_failure(&mut self, _: InputError) -> InputResult<()> {
                replace_failed_device(
                    &mut self.device,
                    |_| {
                        self.trace.borrow_mut().push((self.number, "detach"));
                        let remaining = self.remaining_detach_failures.get();
                        if remaining > 0 {
                            self.remaining_detach_failures.set(remaining - 1);
                            Err(InputError::new("release_failed", "detach failed"))
                        } else {
                            Ok(())
                        }
                    },
                    || Ok(self.number),
                )?;
                self.uncertain = false;
                Ok(())
            }

            fn release(&mut self) -> InputResult<()> {
                release_owned_device(
                    &mut self.device,
                    self.uncertain,
                    |_| {
                        self.trace.borrow_mut().push((self.number, "neutral"));
                        Ok(())
                    },
                    |_| {
                        self.trace.borrow_mut().push((self.number, "detach"));
                        let remaining = self.remaining_detach_failures.get();
                        if self.number == 0 && remaining > 0 {
                            self.remaining_detach_failures.set(remaining - 1);
                            Err(InputError::new("release_failed", "detach failed"))
                        } else {
                            Ok(())
                        }
                    },
                )
            }
        }

        let trace = Rc::new(RefCell::new(Vec::new()));
        let fail_first_write = Rc::new(Cell::new(true));
        let remaining_detach_failures = Rc::new(Cell::new(2));
        let mut slots = GamepadSlots::new();
        let now = Instant::now();
        let mut create = |number| {
            Ok(FakeUinput {
                number,
                device: Some(number),
                uncertain: false,
                fail_first_write: fail_first_write.clone(),
                remaining_detach_failures: remaining_detach_failures.clone(),
                trace: trace.clone(),
            })
        };

        assert_eq!(
            slots
                .apply(
                    0,
                    0b01,
                    ControllerSnapshot {
                        buttons: 1,
                        ..ControllerSnapshot::default()
                    },
                    -1,
                    now,
                    &mut create,
                )
                .unwrap_err()
                .code,
            "release_failed"
        );
        slots
            .apply(1, 0b11, ControllerSnapshot::default(), -1, now, &mut create)
            .unwrap();
        assert_eq!(
            slots
                .apply(1, 0b10, ControllerSnapshot::default(), -1, now, &mut create)
                .unwrap_err()
                .code,
            "release_failed"
        );
        assert_eq!(remaining_detach_failures.get(), 0);
        slots.advance(now).unwrap();
        assert!(slots.devices[0].is_none());
        assert!(slots.devices[1].is_some());
        assert!(!trace.borrow().contains(&(0, "neutral")));
        assert!(trace.borrow().contains(&(1, "update")));
        slots.stop().unwrap();
    }

    #[test]
    fn failed_uinput_write_and_detach_force_recovery_on_unchanged_tick() {
        struct FakeUinput {
            state: ControllerSnapshot,
            recovery_required: bool,
            fail_emit: bool,
            fail_detach: Rc<std::cell::Cell<bool>>,
            trace: Rc<RefCell<Vec<&'static str>>>,
        }
        impl GamepadDevice for FakeUinput {
            fn update(&mut self, desired: ControllerSnapshot) -> InputResult<()> {
                if self.recovery_required {
                    self.trace.borrow_mut().push("blocked stale write");
                    return Err(InputError::new(
                        "controller_unavailable",
                        "recovery required",
                    ));
                }
                if controller_state_events(self.state, desired).is_empty() {
                    return Ok(());
                }
                self.state = possible_controller_state_after_write_failure(self.state, desired);
                self.recovery_required = true;
                self.trace.borrow_mut().push("emit");
                if std::mem::take(&mut self.fail_emit) {
                    return Err(InputError::new("injection_failed", "partial write"));
                }
                self.state = desired;
                self.recovery_required = false;
                Ok(())
            }

            fn recover_after_update_failure(&mut self, _: InputError) -> InputResult<()> {
                self.trace.borrow_mut().push("detach");
                if self.fail_detach.get() {
                    return Err(InputError::new("release_failed", "destroy failed"));
                }
                self.state = ControllerSnapshot::default();
                self.recovery_required = false;
                self.trace.borrow_mut().push("allocate");
                Ok(())
            }

            fn release(&mut self) -> InputResult<()> {
                self.trace.borrow_mut().push("release");
                Ok(())
            }
        }

        let fail_detach = Rc::new(std::cell::Cell::new(true));
        let trace = Rc::new(RefCell::new(Vec::new()));
        let mut slots = GamepadSlots::new();
        let now = Instant::now();
        assert!(
            slots
                .apply(
                    0,
                    1,
                    ControllerSnapshot {
                        buttons: BACK_BUTTON_MASK,
                        ..ControllerSnapshot::default()
                    },
                    -1,
                    now,
                    |_| {
                        Ok(FakeUinput {
                            state: ControllerSnapshot::default(),
                            recovery_required: false,
                            fail_emit: true,
                            fail_detach: fail_detach.clone(),
                            trace: trace.clone(),
                        })
                    },
                )
                .is_err()
        );
        assert!(slots.advance(now + Duration::from_millis(100)).is_err());
        assert_eq!(
            &*trace.borrow(),
            &["emit", "detach", "blocked stale write", "detach"]
        );
        fail_detach.set(false);
        slots.advance(now + Duration::from_millis(200)).unwrap();
        assert_eq!(trace.borrow().last(), Some(&"emit"));
        assert!(trace.borrow().contains(&"allocate"));
        slots.stop().unwrap();
        assert_eq!(trace.borrow().last(), Some(&"release"));
    }

    #[test]
    fn failed_lease_release_retries_removal_without_writing_another_report() {
        struct FlakyRelease {
            trace: Rc<RefCell<Vec<&'static str>>>,
            release_failures: Rc<std::cell::Cell<u8>>,
        }

        impl GamepadDevice for FlakyRelease {
            fn update(&mut self, _: ControllerSnapshot) -> InputResult<()> {
                self.trace.borrow_mut().push("update");
                Ok(())
            }

            fn release(&mut self) -> InputResult<()> {
                self.trace.borrow_mut().push("release");
                let remaining = self.release_failures.get();
                if remaining > 0 {
                    self.release_failures.set(remaining - 1);
                    Err(InputError::new("release_failed", "device still attached"))
                } else {
                    Ok(())
                }
            }
        }

        let trace = Rc::new(RefCell::new(Vec::new()));
        let release_failures = Rc::new(std::cell::Cell::new(1));
        let mut slots = GamepadSlots::new();
        let now = Instant::now();
        slots
            .apply(0, 1, ControllerSnapshot::default(), -1, now, |_| {
                Ok(FlakyRelease {
                    trace: trace.clone(),
                    release_failures: release_failures.clone(),
                })
            })
            .unwrap();
        assert_eq!(slots.stop().unwrap_err().code, "release_failed");
        assert!(!slots.is_empty());
        slots.advance(now + Duration::from_millis(100)).unwrap();
        assert!(slots.is_empty());
        assert_eq!(&*trace.borrow(), &["update", "release", "release"]);
    }
}
