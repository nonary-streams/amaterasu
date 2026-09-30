//! Linux Nintendo Switch controller emulation through uinput.
//!
//! The report mapping follows Inputtino `f9eed6d34ff6cb5a30cfce6ee4e6f5362abedb82`,
//! pinned by Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`. Feedback
//! remains disabled until the input protocol has a client rumble callback.

use super::{
    ABS_HAT0X, ABS_HAT0Y, ABS_RX, ABS_RY, ABS_X, ABS_Y, BTN_EAST, BTN_MODE, BTN_NORTH, BTN_SELECT,
    BTN_SOUTH, BTN_START, BTN_SWITCH_Z, BTN_THUMBL, BTN_THUMBR, BTN_TL, BTN_TL2, BTN_TR, BTN_TR2,
    BTN_WEST, Device, EV_ABS, EV_KEY,
};
use crate::gamepad::{
    ControllerSnapshot, GamepadDevice, possible_controller_state_after_write_failure,
    release_owned_device, replace_failed_device,
};
use crate::model::{InputError, InputResult};

const DPAD_UP: u32 = 1 << 0;
const DPAD_DOWN: u32 = 1 << 1;
const DPAD_LEFT: u32 = 1 << 2;
const DPAD_RIGHT: u32 = 1 << 3;
const DPAD_VERTICAL: u32 = DPAD_UP | DPAD_DOWN;
const DPAD_HORIZONTAL: u32 = DPAD_LEFT | DPAD_RIGHT;
const START: u32 = 1 << 4;
const BACK: u32 = 1 << 5;
const LEFT_STICK: u32 = 1 << 6;
const RIGHT_STICK: u32 = 1 << 7;
const LEFT_BUTTON: u32 = 1 << 8;
const RIGHT_BUTTON: u32 = 1 << 9;
const HOME: u32 = 1 << 10;
const A: u32 = 1 << 12;
const B: u32 = 1 << 13;
const X: u32 = 1 << 14;
const Y: u32 = 1 << 15;
const CAPTURE: u32 = 1 << 21;

pub(super) struct SwitchGamepad {
    device: Option<Device>,
    path: String,
    state: ControllerSnapshot,
    recovery_required: bool,
}

impl SwitchGamepad {
    pub(super) fn create(path: &str, _slot: u8) -> InputResult<Self> {
        Ok(Self {
            device: Some(Device::create_switch_gamepad(path)?),
            path: path.to_owned(),
            state: ControllerSnapshot::default(),
            recovery_required: false,
        })
    }
}

impl GamepadDevice for SwitchGamepad {
    fn update(&mut self, state: ControllerSnapshot) -> InputResult<()> {
        if self.recovery_required {
            return Err(InputError::new(
                "controller_unavailable",
                "Switch gamepad must detach after a failed report",
            ));
        }
        let device = self.device.as_ref().ok_or_else(|| {
            InputError::new("controller_unavailable", "Switch gamepad is reacquiring")
        })?;
        let events = switch_state_events(self.state, state);
        if !events.is_empty() {
            self.state = possible_controller_state_after_write_failure(self.state, state);
            self.recovery_required = true;
            device.emit(&events)?;
        }
        self.state = state;
        self.recovery_required = false;
        Ok(())
    }

    fn recover_after_update_failure(&mut self, _error: InputError) -> InputResult<()> {
        replace_failed_device(&mut self.device, Device::destroy, || {
            Device::create_switch_gamepad(&self.path)
        })?;
        self.state = ControllerSnapshot::default();
        self.recovery_required = false;
        Ok(())
    }

    fn release(&mut self) -> InputResult<()> {
        let previous = self.state;
        let neutral = ControllerSnapshot::default();
        let events = switch_state_events(previous, neutral);
        let result = release_owned_device(
            &mut self.device,
            self.recovery_required,
            |device| {
                self.state = possible_controller_state_after_write_failure(previous, neutral);
                if events.is_empty() {
                    return Ok(());
                }
                // If neutralization fails, the helper still attempts detach.
                self.recovery_required = true;
                device.emit(&events)
            },
            Device::destroy,
        );
        if result.is_ok() {
            self.state = neutral;
            self.recovery_required = false;
        }
        result
    }
}

fn switch_state_events(
    previous: ControllerSnapshot,
    next: ControllerSnapshot,
) -> Vec<(u16, u16, i32)> {
    let mut events = Vec::with_capacity(24);
    for (mask, code) in [
        (START, BTN_START),
        (BACK, BTN_SELECT),
        (LEFT_STICK, BTN_THUMBL),
        (RIGHT_STICK, BTN_THUMBR),
        (LEFT_BUTTON, BTN_TL),
        (RIGHT_BUTTON, BTN_TR),
        (HOME, BTN_MODE),
        (CAPTURE, BTN_SWITCH_Z),
        (A, BTN_EAST),
        (B, BTN_SOUTH),
        (X, BTN_NORTH),
        (Y, BTN_WEST),
    ] {
        push_changed_key(&mut events, previous.buttons, next.buttons, mask, code);
    }

    for (mask, code) in [(DPAD_VERTICAL, ABS_HAT0Y), (DPAD_HORIZONTAL, ABS_HAT0X)] {
        let old_value = hat_value(previous.buttons, mask);
        let new_value = hat_value(next.buttons, mask);
        if old_value != new_value {
            events.push((EV_ABS, code, new_value));
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
            events.push((EV_KEY, code, i32::from(is_pressed)));
        }
    }

    for (old, new, code) in [
        (previous.left_x, next.left_x, ABS_X),
        (previous.left_y, next.left_y, ABS_Y),
        (previous.right_x, next.right_x, ABS_RX),
        (previous.right_y, next.right_y, ABS_RY),
    ] {
        if old != new {
            let value = if code == ABS_Y || code == ABS_RY {
                -i32::from(new)
            } else {
                i32::from(new)
            };
            events.push((EV_ABS, code, value));
        }
    }
    events
}

fn push_changed_key(
    events: &mut Vec<(u16, u16, i32)>,
    previous: u32,
    next: u32,
    mask: u32,
    code: u16,
) {
    let was_pressed = previous & mask != 0;
    let is_pressed = next & mask != 0;
    if was_pressed != is_pressed {
        events.push((EV_KEY, code, i32::from(is_pressed)));
    }
}

fn hat_value(buttons: u32, opposing_directions: u32) -> i32 {
    match opposing_directions {
        DPAD_VERTICAL => {
            if buttons & DPAD_UP != 0 {
                -1
            } else if buttons & DPAD_DOWN != 0 {
                1
            } else {
                0
            }
        }
        DPAD_HORIZONTAL => {
            if buttons & DPAD_LEFT != 0 {
                -1
            } else if buttons & DPAD_RIGHT != 0 {
                1
            } else {
                0
            }
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABS_Z: u16 = 0x02;
    const ABS_RZ: u16 = 0x05;

    #[test]
    fn switch_report_swaps_face_buttons_and_uses_digital_triggers() {
        let state = ControllerSnapshot {
            buttons: (A | B | X | Y | BACK | LEFT_BUTTON | RIGHT_BUTTON | HOME | CAPTURE)
                | DPAD_UP
                | DPAD_DOWN
                | DPAD_LEFT
                | DPAD_RIGHT,
            left_trigger: 1,
            right_trigger: 0,
            left_x: i16::MIN,
            left_y: i16::MAX,
            right_x: 1234,
            right_y: -2345,
        };
        let events = switch_state_events(ControllerSnapshot::default(), state);

        for event in [
            (EV_KEY, BTN_EAST, 1),
            (EV_KEY, BTN_SOUTH, 1),
            (EV_KEY, BTN_NORTH, 1),
            (EV_KEY, BTN_WEST, 1),
            (EV_KEY, BTN_TL2, 1),
            (EV_KEY, BTN_TL, 1),
            (EV_KEY, BTN_TR, 1),
            (EV_KEY, BTN_SELECT, 1),
            (EV_KEY, BTN_MODE, 1),
            (EV_KEY, BTN_SWITCH_Z, 1),
            (EV_ABS, ABS_HAT0X, -1),
            (EV_ABS, ABS_HAT0Y, -1),
            (EV_ABS, ABS_X, i32::from(i16::MIN)),
            (EV_ABS, ABS_Y, -i32::from(i16::MAX)),
            (EV_ABS, ABS_RX, 1234),
            (EV_ABS, ABS_RY, 2345),
        ] {
            assert!(events.contains(&event), "missing event {event:?}");
        }
        assert!(
            !events
                .iter()
                .any(|(kind, code, _)| { *kind == EV_ABS && (*code == ABS_Z || *code == ABS_RZ) })
        );
    }

    #[test]
    fn switch_report_releases_dpad_with_pinned_opposite_direction_precedence() {
        let previous = ControllerSnapshot {
            buttons: DPAD_UP | DPAD_LEFT,
            ..ControllerSnapshot::default()
        };
        let next = ControllerSnapshot {
            buttons: DPAD_DOWN | DPAD_RIGHT,
            ..ControllerSnapshot::default()
        };
        assert_eq!(
            switch_state_events(previous, next),
            [(EV_ABS, ABS_HAT0Y, 1), (EV_ABS, ABS_HAT0X, 1),]
        );
        assert_eq!(hat_value(DPAD_UP | DPAD_DOWN, DPAD_UP | DPAD_DOWN), -1);
        assert_eq!(
            hat_value(DPAD_LEFT | DPAD_RIGHT, DPAD_LEFT | DPAD_RIGHT),
            -1
        );
    }
}
