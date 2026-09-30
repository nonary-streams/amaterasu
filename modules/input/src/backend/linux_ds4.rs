//! Linux DualShock 4 emulation over UHID.
//!
//! The public gamepad model supplies controller state, motion, touch, and
//! battery updates. This module owns the Sony USB identity and serializes that
//! state into the DS4 input report. UHID output reports capture rumble and
//! lightbar changes into the active, slot-owned feedback queue for delivery by
//! the authenticated stream session.

use crate::feedback::FeedbackProducer;
use crate::gamepad::{
    ControllerSnapshot, GamepadBatteryState, GamepadDevice, GamepadMotionType, GamepadTouchPhase,
    GamepadTouchReport,
};
use crate::model::{InputError, InputResult};
use amaterasu_gamestream::control::HostFeedback;
#[cfg(not(test))]
use std::fs::{File, OpenOptions};
#[cfg(not(test))]
use std::os::fd::{AsRawFd, RawFd};
#[cfg(not(test))]
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
#[cfg(not(test))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(not(test))]
use std::thread::{self, JoinHandle};
#[cfg(not(test))]
use std::time::{Duration, Instant};

const UHID_EVENT_SIZE: usize = 4 + 128 + 64 + 64 + 2 + 2 + 16 + 4096;
const UHID_DATA_MAX: usize = 4096;
#[cfg(not(test))]
#[cfg(not(test))]
const UHID_DESTROY: u32 = 1;
#[cfg(not(test))]
const UHID_START: u32 = 2;
#[cfg(not(test))]
const UHID_STOP: u32 = 3;
const UHID_OUTPUT: u32 = 6;
const UHID_GET_REPORT: u32 = 9;
const UHID_GET_REPORT_REPLY: u32 = 10;
const UHID_CREATE2: u32 = 11;
#[cfg(not(test))]
#[cfg(not(test))]
const UHID_INPUT2: u32 = 12;
const UHID_SET_REPORT: u32 = 13;
const UHID_SET_REPORT_REPLY: u32 = 14;
const UHID_FEATURE_REPORT: u8 = 0;
const UHID_OUTPUT_REPORT: u8 = 1;
const DS4_INPUT_REPORT_ID: u8 = 0x01;
const DS4_OUTPUT_REPORT_ID: u8 = 0x05;
const DS4_CALIBRATION_REPORT_ID: u8 = 0x02;
const DS4_PAIRING_REPORT_ID: u8 = 0x12;
const DS4_CONTROL_REPORT_ID: u8 = 0x14;
const DS4_FIRMWARE_REPORT_ID: u8 = 0xa3;
const DS4_INPUT_REPORT_SIZE: usize = 64;
const DS4_OUTPUT_REPORT_SIZE: usize = 32;
const DS4_VENDOR_ID: u32 = 0x054c;
const DS4_PRODUCT_ID: u32 = 0x05c4;
const DS4_VERSION: u32 = 0x0100;
const DS4_TOUCHPAD_WIDTH: u16 = 1920;
const DS4_TOUCHPAD_HEIGHT: u16 = 942;
#[cfg(not(test))]
const DS4_REPORT_INTERVAL: Duration = Duration::from_millis(4);
const STANDARD_GRAVITY: f32 = 9.80665;
const BUS_USB: u16 = 0x03;
#[cfg(test)]
const MAX_HID_DESCRIPTOR_SIZE: usize = 4096;

const DPAD_UP: u32 = 1 << 0;
const DPAD_DOWN: u32 = 1 << 1;
const DPAD_LEFT: u32 = 1 << 2;
const DPAD_RIGHT: u32 = 1 << 3;
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
const TOUCHPAD_BUTTON: u32 = 1 << 20;

// USB DS4 descriptor: it declares the 64-byte input report, 32-byte output
// report, and feature IDs queried by the kernel PlayStation HID driver. The
// report IDs and lengths are part of the device identity, not optional padding.
const DS4_REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35,
    0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x04, 0x81, 0x02, 0x09, 0x39, 0x15, 0x00, 0x25,
    0x07, 0x35, 0x00, 0x46, 0x3b, 0x01, 0x65, 0x14, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x65, 0x00,
    0x05, 0x09, 0x19, 0x01, 0x29, 0x0e, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x0e, 0x81, 0x02,
    0x06, 0x00, 0xff, 0x09, 0x20, 0x75, 0x06, 0x95, 0x01, 0x15, 0x00, 0x25, 0x3f, 0x81, 0x02, 0x05,
    0x01, 0x09, 0x33, 0x09, 0x34, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02,
    0x06, 0x00, 0xff, 0x09, 0x21, 0x95, 0x36, 0x81, 0x02, 0x85, 0x05, 0x09, 0x22, 0x95, 0x1f, 0x91,
    0x02, 0x85, 0x04, 0x09, 0x23, 0x95, 0x24, 0xb1, 0x02, 0x85, 0x02, 0x09, 0x24, 0x95, 0x24, 0xb1,
    0x02, 0x85, 0x08, 0x09, 0x25, 0x95, 0x03, 0xb1, 0x02, 0x85, 0x10, 0x09, 0x26, 0x95, 0x04, 0xb1,
    0x02, 0x85, 0x11, 0x09, 0x27, 0x95, 0x02, 0xb1, 0x02, 0x85, 0x12, 0x06, 0x02, 0xff, 0x09, 0x21,
    0x95, 0x0f, 0xb1, 0x02, 0x85, 0x13, 0x09, 0x22, 0x95, 0x16, 0xb1, 0x02, 0x85, 0x14, 0x06, 0x05,
    0xff, 0x09, 0x20, 0x95, 0x10, 0xb1, 0x02, 0x85, 0x15, 0x09, 0x21, 0x95, 0x2c, 0xb1, 0x02, 0x06,
    0x80, 0xff, 0x85, 0x80, 0x09, 0x20, 0x95, 0x06, 0xb1, 0x02, 0x85, 0x81, 0x09, 0x21, 0x95, 0x06,
    0xb1, 0x02, 0x85, 0x82, 0x09, 0x22, 0x95, 0x05, 0xb1, 0x02, 0x85, 0x83, 0x09, 0x23, 0x95, 0x01,
    0xb1, 0x02, 0x85, 0x84, 0x09, 0x24, 0x95, 0x04, 0xb1, 0x02, 0x85, 0x85, 0x09, 0x25, 0x95, 0x06,
    0xb1, 0x02, 0x85, 0x86, 0x09, 0x26, 0x95, 0x06, 0xb1, 0x02, 0x85, 0x87, 0x09, 0x27, 0x95, 0x23,
    0xb1, 0x02, 0x85, 0x88, 0x09, 0x28, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0x89, 0x09, 0x29, 0x95, 0x02,
    0xb1, 0x02, 0x85, 0x90, 0x09, 0x30, 0x95, 0x05, 0xb1, 0x02, 0x85, 0x91, 0x09, 0x31, 0x95, 0x03,
    0xb1, 0x02, 0x85, 0x92, 0x09, 0x32, 0x95, 0x03, 0xb1, 0x02, 0x85, 0x93, 0x09, 0x33, 0x95, 0x0c,
    0xb1, 0x02, 0x85, 0x94, 0x09, 0x34, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xa0, 0x09, 0x40, 0x95, 0x06,
    0xb1, 0x02, 0x85, 0xa1, 0x09, 0x41, 0x95, 0x01, 0xb1, 0x02, 0x85, 0xa2, 0x09, 0x42, 0x95, 0x01,
    0xb1, 0x02, 0x85, 0xa3, 0x09, 0x43, 0x95, 0x30, 0xb1, 0x02, 0x85, 0xa4, 0x09, 0x44, 0x95, 0x0d,
    0xb1, 0x02, 0x85, 0xf0, 0x09, 0x47, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xf1, 0x09, 0x48, 0x95, 0x3f,
    0xb1, 0x02, 0x85, 0xf2, 0x09, 0x49, 0x95, 0x0f, 0xb1, 0x02, 0x85, 0xa7, 0x09, 0x4a, 0x95, 0x01,
    0xb1, 0x02, 0x85, 0xa8, 0x09, 0x4b, 0x95, 0x01, 0xb1, 0x02, 0x85, 0xa9, 0x09, 0x4c, 0x95, 0x08,
    0xb1, 0x02, 0x85, 0xaa, 0x09, 0x4e, 0x95, 0x01, 0xb1, 0x02, 0x85, 0xab, 0x09, 0x4f, 0x95, 0x39,
    0xb1, 0x02, 0x85, 0xac, 0x09, 0x50, 0x95, 0x39, 0xb1, 0x02, 0x85, 0xad, 0x09, 0x51, 0x95, 0x0b,
    0xb1, 0x02, 0x85, 0xae, 0x09, 0x52, 0x95, 0x01, 0xb1, 0x02, 0x85, 0xaf, 0x09, 0x53, 0x95, 0x02,
    0xb1, 0x02, 0x85, 0xb0, 0x09, 0x54, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xe0, 0x09, 0x57, 0x95, 0x02,
    0xb1, 0x02, 0x85, 0xb3, 0x09, 0x55, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xb4, 0x09, 0x55, 0x95, 0x3f,
    0xb1, 0x02, 0x85, 0xb5, 0x09, 0x56, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xd0, 0x09, 0x58, 0x95, 0x3f,
    0xb1, 0x02, 0x85, 0xd4, 0x09, 0x59, 0x95, 0x3f, 0xb1, 0x02, 0xc0,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TouchPoint {
    id: u8,
    x: u16,
    y: u16,
    active: bool,
}

impl TouchPoint {
    const RELEASED: Self = Self {
        id: 0,
        x: 0,
        y: 0,
        active: false,
    };
}

#[derive(Clone, Debug, PartialEq)]
struct Ds4ReportState {
    controller: ControllerSnapshot,
    address: [u8; 6],
    back_as_touchpad_click: bool,
    acceleration: [f32; 3],
    gyroscope: [f32; 3],
    touches: [TouchPoint; 2],
    battery_status: u8,
    report_counter: u8,
    sensor_timestamp: u16,
    touch_timestamp: u8,
    next_touch_id: u8,
}

impl Default for Ds4ReportState {
    fn default() -> Self {
        Self {
            controller: ControllerSnapshot::default(),
            address: [0x02, 0, 0, 4, 0, 0],
            back_as_touchpad_click: true,
            acceleration: [0.0, STANDARD_GRAVITY, 0.0],
            gyroscope: [0.0; 3],
            touches: [TouchPoint::RELEASED; 2],
            battery_status: 0x1b,
            report_counter: 0,
            sensor_timestamp: 0,
            touch_timestamp: 0,
            next_touch_id: 0,
        }
    }
}

impl Ds4ReportState {
    fn input_report(&mut self) -> [u8; DS4_INPUT_REPORT_SIZE] {
        let mut report = [0; DS4_INPUT_REPORT_SIZE];
        report[0] = DS4_INPUT_REPORT_ID;
        report[1] = scale_axis(
            i32::from(self.controller.left_x),
            i32::from(i16::MIN),
            i32::from(i16::MAX),
        );
        report[2] = scale_axis(
            -i32::from(self.controller.left_y),
            i16::MIN.into(),
            i16::MAX.into(),
        );
        report[3] = scale_axis(
            i32::from(self.controller.right_x),
            i32::from(i16::MIN),
            i32::from(i16::MAX),
        );
        report[4] = scale_axis(
            -i32::from(self.controller.right_y),
            i16::MIN.into(),
            i16::MAX.into(),
        );

        let mut buttons = [0x08, 0, 0];
        buttons[0] = dpad_hat(self.controller.buttons);
        if self.controller.buttons & X != 0 {
            buttons[0] |= 0x10;
        }
        if self.controller.buttons & A != 0 {
            buttons[0] |= 0x20;
        }
        if self.controller.buttons & B != 0 {
            buttons[0] |= 0x40;
        }
        if self.controller.buttons & Y != 0 {
            buttons[0] |= 0x80;
        }
        if self.controller.buttons & LEFT_BUTTON != 0 {
            buttons[1] |= 0x01;
        }
        if self.controller.buttons & RIGHT_BUTTON != 0 {
            buttons[1] |= 0x02;
        }
        if self.controller.left_trigger != 0 {
            buttons[1] |= 0x04;
        }
        if self.controller.right_trigger != 0 {
            buttons[1] |= 0x08;
        }
        if self.controller.buttons & BACK != 0 {
            buttons[1] |= 0x10;
        }
        if self.controller.buttons & START != 0 {
            buttons[1] |= 0x20;
        }
        if self.controller.buttons & LEFT_STICK != 0 {
            buttons[1] |= 0x40;
        }
        if self.controller.buttons & RIGHT_STICK != 0 {
            buttons[1] |= 0x80;
        }
        if self.controller.buttons & HOME != 0 {
            buttons[2] |= 0x01;
        }
        if self.controller.buttons & TOUCHPAD_BUTTON != 0
            || (self.back_as_touchpad_click && self.controller.buttons & BACK != 0)
        {
            buttons[2] |= 0x02;
        }
        report[5] = buttons[0];
        report[6] = buttons[1];
        self.report_counter = self.report_counter.wrapping_add(1) & 0x3f;
        report[7] = buttons[2] | (self.report_counter << 2);
        report[8] = self.controller.left_trigger;
        report[9] = self.controller.right_trigger;
        self.sensor_timestamp = self.sensor_timestamp.wrapping_add(750);
        report[10..12].copy_from_slice(&self.sensor_timestamp.to_le_bytes());
        report[12] = 25;
        write_motion(&mut report[13..19], self.gyroscope, MotionScale::Gyroscope);
        write_motion(
            &mut report[19..25],
            self.acceleration,
            MotionScale::Acceleration,
        );
        report[30] = self.battery_status;
        report[33] = 1;
        self.touch_timestamp = self.touch_timestamp.wrapping_add(1);
        report[34] = self.touch_timestamp;
        write_touch(&mut report[35..39], self.touches[0]);
        write_touch(&mut report[39..43], self.touches[1]);
        for index in 1..3 {
            let base = 34 + index * 9;
            write_touch(&mut report[base + 1..base + 5], TouchPoint::RELEASED);
            write_touch(&mut report[base + 5..base + 9], TouchPoint::RELEASED);
        }
        report
    }

    fn set_motion(&mut self, motion_type: GamepadMotionType, values: [f32; 3]) -> InputResult<()> {
        if values.into_iter().any(|value| !value.is_finite()) {
            return Err(InputError::new(
                "invalid_event",
                "controller motion values must be finite",
            ));
        }
        match motion_type {
            GamepadMotionType::Accel => self.acceleration = values,
            GamepadMotionType::Gyro => self.gyroscope = values,
        }
        Ok(())
    }

    fn set_touch(
        &mut self,
        pointer_id: u32,
        phase: GamepadTouchPhase,
        x: f32,
        y: f32,
        pressure: f32,
    ) -> InputResult<()> {
        if pointer_id > 1 {
            return Err(InputError::new(
                "unsupported_event",
                "DS4 touchpad supports two pointer IDs (0 and 1)",
            ));
        }
        if [x, y, pressure]
            .into_iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err(InputError::new(
                "invalid_event",
                "controller touch coordinates and pressure must be normalized",
            ));
        }
        if phase == GamepadTouchPhase::CancelAll {
            for touch in &mut self.touches {
                touch.active = false;
            }
            return Ok(());
        }
        match phase {
            GamepadTouchPhase::Hover | GamepadTouchPhase::ButtonOnly => return Ok(()),
            GamepadTouchPhase::Up | GamepadTouchPhase::Cancel | GamepadTouchPhase::HoverLeave => {
                self.touches[pointer_id as usize].active = false;
                return Ok(());
            }
            GamepadTouchPhase::Down | GamepadTouchPhase::Move => {}
            GamepadTouchPhase::CancelAll => unreachable!("handled above"),
        }
        let point = &mut self.touches[pointer_id as usize];
        if pressure <= 0.5 {
            point.active = false;
            return Ok(());
        }
        if !point.active {
            self.next_touch_id = self.next_touch_id.wrapping_add(1) & 0x7f;
            point.id = self.next_touch_id;
        }
        point.active = true;
        point.x = (x * f32::from(DS4_TOUCHPAD_WIDTH)) as u16;
        point.y = (y * f32::from(DS4_TOUCHPAD_HEIGHT)) as u16;
        Ok(())
    }

    fn set_battery(&mut self, state: GamepadBatteryState, percentage: Option<u8>) {
        let Some(percentage) = percentage else {
            return;
        };
        let level = match state {
            GamepadBatteryState::Charging | GamepadBatteryState::Discharging => {
                percentage.min(100) / 10
            }
            GamepadBatteryState::Full => 11,
            GamepadBatteryState::Unknown
            | GamepadBatteryState::NotPresent
            | GamepadBatteryState::NotCharging => return,
        };
        let cable_connected = matches!(
            state,
            GamepadBatteryState::Charging
                | GamepadBatteryState::NotCharging
                | GamepadBatteryState::Full
        );
        self.battery_status = (if cable_connected { 0x10 } else { 0 }) | level;
    }

    fn neutralize(&mut self) {
        self.controller = ControllerSnapshot::default();
        self.acceleration = [0.0, STANDARD_GRAVITY, 0.0];
        self.gyroscope = [0.0; 3];
        self.touches = [TouchPoint::RELEASED; 2];
    }

    fn feature_report(&self, report_id: u8) -> Option<Vec<u8>> {
        match report_id {
            DS4_CALIBRATION_REPORT_ID => Some(vec![
                DS4_CALIBRATION_REPORT_ID,
                0,
                0,
                0,
                0,
                0,
                0,
                0x00,
                0x20,
                0x00,
                0xe0,
                0x00,
                0x20,
                0x00,
                0xe0,
                0x00,
                0x20,
                0x00,
                0xe0,
                0x00,
                0x02,
                0x00,
                0x02,
                0x00,
                0x20,
                0x00,
                0xe0,
                0x00,
                0x20,
                0x00,
                0xe0,
                0x00,
                0x20,
                0x00,
                0xe0,
                0,
                0,
            ]),
            DS4_PAIRING_REPORT_ID => {
                let mut report = vec![0; 16];
                report[0] = DS4_PAIRING_REPORT_ID;
                for (offset, address_byte) in self.address.iter().rev().enumerate() {
                    report[offset + 1] = *address_byte;
                }
                report[7] = 0x08;
                report[8] = 0x25;
                Some(report)
            }
            DS4_CONTROL_REPORT_ID => {
                let mut report = vec![0; 17];
                report[0] = DS4_CONTROL_REPORT_ID;
                report[1] = 0x02;
                Some(report)
            }
            DS4_FIRMWARE_REPORT_ID => {
                let mut report = vec![0; 49];
                report[0] = DS4_FIRMWARE_REPORT_ID;
                report[34] = 0x01;
                report[36] = 0x43;
                report[37] = 0x03;
                report[41] = 0x51;
                report[43] = 0x05;
                report[46] = 0x80;
                report[47] = 0x03;
                Some(report)
            }
            _ => None,
        }
    }

    fn accept_feature_report(&self, report_id: u8, report: &[u8]) -> bool {
        report_id == DS4_CONTROL_REPORT_ID
            && report.len() >= 17
            && report[0] == DS4_CONTROL_REPORT_ID
            && report[1] == 0x02
            && report[2..17].iter().all(|byte| *byte == 0)
    }
}

#[derive(Clone, Copy)]
enum MotionScale {
    Acceleration,
    Gyroscope,
}

fn write_motion(target: &mut [u8], values: [f32; 3], scale: MotionScale) {
    for (index, value) in values.into_iter().enumerate() {
        let raw = match scale {
            MotionScale::Acceleration => (value / STANDARD_GRAVITY * 8192.0)
                .round()
                .clamp(-32768.0, 32767.0) as i16,
            MotionScale::Gyroscope => (value * 16.0).round().clamp(-32768.0, 32767.0) as i16,
        };
        target[index * 2..index * 2 + 2].copy_from_slice(&raw.to_le_bytes());
    }
}

fn write_touch(target: &mut [u8], point: TouchPoint) {
    let contact = if point.active {
        point.id & 0x7f
    } else {
        point.id | 0x80
    };
    target[0] = contact;
    target[1] = point.x as u8;
    target[2] = ((point.x >> 8) as u8 & 0x0f) | ((point.y as u8 & 0x0f) << 4);
    target[3] = (point.y >> 4) as u8;
}

fn dpad_hat(buttons: u32) -> u8 {
    match (
        buttons & (DPAD_UP | DPAD_DOWN) != 0,
        buttons & (DPAD_LEFT | DPAD_RIGHT) != 0,
    ) {
        (true, true) if buttons & DPAD_UP != 0 && buttons & DPAD_LEFT != 0 => 7,
        (true, true) if buttons & DPAD_UP != 0 => 1,
        (true, true) if buttons & DPAD_DOWN != 0 && buttons & DPAD_LEFT != 0 => 5,
        (true, true) if buttons & DPAD_DOWN != 0 => 3,
        (true, false) if buttons & DPAD_UP != 0 => 0,
        (true, false) => 4,
        (false, true) if buttons & DPAD_LEFT != 0 => 6,
        (false, true) => 2,
        _ => 8,
    }
}

fn scale_axis(value: i32, minimum: i32, maximum: i32) -> u8 {
    let normalized = (f64::from(value - minimum) / f64::from(maximum - minimum)) * 255.0;
    normalized.round().clamp(0.0, 255.0) as u8
}

fn create_event(event_type: u32) -> Vec<u8> {
    let mut event = vec![0; UHID_EVENT_SIZE];
    put_u32(&mut event, 0, event_type);
    event
}

#[cfg(test)]
fn create_device_event(slot: u8) -> Vec<u8> {
    create_device_event_with_address(slot, [0x02, 0, 0, 4, 0, slot])
}

fn create_device_event_with_address(slot: u8, address: [u8; 6]) -> Vec<u8> {
    let mut event = create_event(UHID_CREATE2);
    let name = b"Amaterasu DualShock 4";
    let phys = format!("amaterasu/uhid/ds4/{slot}");
    let uniq = format_address(address);
    event[4..4 + name.len()].copy_from_slice(name);
    event[132..132 + phys.len()].copy_from_slice(phys.as_bytes());
    event[196..196 + uniq.len()].copy_from_slice(uniq.as_bytes());
    put_u16(&mut event, 260, DS4_REPORT_DESCRIPTOR.len() as u16);
    put_u16(&mut event, 262, BUS_USB);
    put_u32(&mut event, 264, DS4_VENDOR_ID);
    put_u32(&mut event, 268, DS4_PRODUCT_ID);
    put_u32(&mut event, 272, DS4_VERSION);
    event[280..280 + DS4_REPORT_DESCRIPTOR.len()].copy_from_slice(DS4_REPORT_DESCRIPTOR);
    event
}

fn format_address(address: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        address[0], address[1], address[2], address[3], address[4], address[5]
    )
}

fn random_device_address(slot: u8) -> InputResult<[u8; 6]> {
    #[cfg(test)]
    {
        Ok([0x02, 0, 0, 4, 0, slot])
    }

    #[cfg(not(test))]
    {
        let _ = slot;
        let mut address = [0; 6];
        getrandom::fill(&mut address).map_err(|error| {
            InputError::new(
                "controller_unavailable",
                format!("cannot generate a unique DS4 address: {error}"),
            )
        })?;
        address[0] = (address[0] | 0x02) & 0xfe;
        Ok(address)
    }
}

#[cfg(not(test))]
#[cfg(not(test))]
fn input_report_event(report: [u8; DS4_INPUT_REPORT_SIZE]) -> Vec<u8> {
    let mut event = create_event(UHID_INPUT2);
    put_u16(&mut event, 4, DS4_INPUT_REPORT_SIZE as u16);
    event[6..6 + DS4_INPUT_REPORT_SIZE].copy_from_slice(&report);
    event
}

fn get_report_reply(request: &[u8], report: &Ds4ReportState) -> Vec<u8> {
    let mut reply = create_event(UHID_GET_REPORT_REPLY);
    put_u32(&mut reply, 4, get_u32(request, 4));
    let report_id = request[8];
    let report_type = request[9];
    if report_type != UHID_FEATURE_REPORT {
        put_u16(&mut reply, 8, libc::EINVAL as u16);
        return reply;
    }
    let Some(feature) = report.feature_report(report_id) else {
        put_u16(&mut reply, 8, libc::EINVAL as u16);
        return reply;
    };
    put_u16(&mut reply, 10, feature.len() as u16);
    reply[12..12 + feature.len()].copy_from_slice(&feature);
    reply
}

fn set_report_reply(request: &[u8], report: &Ds4ReportState) -> Vec<u8> {
    let mut reply = create_event(UHID_SET_REPORT_REPLY);
    put_u32(&mut reply, 4, get_u32(request, 4));
    let report_number = request[8];
    let report_type = request[9];
    let report_size = get_u16(request, 10) as usize;
    let data = &request[12..12 + report_size.min(UHID_DATA_MAX)];
    let accepted = match report_type {
        UHID_FEATURE_REPORT => report.accept_feature_report(report_number, data),
        UHID_OUTPUT_REPORT => {
            report_number == DS4_OUTPUT_REPORT_ID
                && data.len() >= DS4_OUTPUT_REPORT_SIZE
                && data.first() == Some(&DS4_OUTPUT_REPORT_ID)
        }
        _ => false,
    };
    if !accepted || report_size > UHID_DATA_MAX {
        put_u16(&mut reply, 8, libc::EINVAL as u16);
    }
    reply
}

fn output_report_is_well_formed(event: &[u8]) -> bool {
    let size = get_u16(event, 4 + UHID_DATA_MAX) as usize;
    let report_type = event[4 + UHID_DATA_MAX + 2];
    report_type == UHID_OUTPUT_REPORT
        && size >= DS4_OUTPUT_REPORT_SIZE
        && event.get(4) == Some(&DS4_OUTPUT_REPORT_ID)
}

fn dispatch_output_feedback(event: &[u8], producer: &FeedbackProducer) -> bool {
    if !output_report_is_well_formed(event) {
        return false;
    }
    let report = &event[4..4 + DS4_OUTPUT_REPORT_SIZE];
    let valid_flags = report[1];
    if valid_flags & 0x01 != 0 {
        producer.push(HostFeedback::Rumble {
            controller: producer.controller(),
            low_frequency: u16::from(report[5]) * 257,
            high_frequency: u16::from(report[4]) * 257,
        });
    }
    if valid_flags & 0x02 != 0 {
        producer.push(HostFeedback::RgbLed {
            controller: producer.controller(),
            red: report[6],
            green: report[7],
            blue: report[8],
        });
    }
    true
}

fn put_u16(target: &mut [u8], offset: usize, value: u16) {
    target[offset..offset + 2].copy_from_slice(&value.to_ne_bytes());
}

fn put_u32(target: &mut [u8], offset: usize, value: u32) {
    target[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
}

fn get_u16(source: &[u8], offset: usize) -> u16 {
    u16::from_ne_bytes([source[offset], source[offset + 1]])
}

fn get_u32(source: &[u8], offset: usize) -> u32 {
    u32::from_ne_bytes([
        source[offset],
        source[offset + 1],
        source[offset + 2],
        source[offset + 3],
    ])
}

#[cfg(not(test))]
struct UhidDevice {
    file: Arc<Mutex<File>>,
    report: Arc<Mutex<Ds4ReportState>>,
    failure: Arc<Mutex<Option<InputError>>>,
    stop: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    destroyed: bool,
}

#[cfg(not(test))]
impl UhidDevice {
    fn create(
        slot: u8,
        report: Arc<Mutex<Ds4ReportState>>,
        address: [u8; 6],
        feedback: FeedbackProducer,
    ) -> InputResult<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open("/dev/uhid")
            .map_err(uhid_error)?;
        if !file
            .metadata()
            .map_err(uhid_error)?
            .file_type()
            .is_char_device()
        {
            return Err(InputError::new(
                "invalid_device",
                "/dev/uhid must be a character device",
            ));
        }
        let file = Arc::new(Mutex::new(file));
        write_event(&file, &create_device_event_with_address(slot, address))?;
        let failure = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let worker = {
            let worker_file = Arc::clone(&file);
            let report = Arc::clone(&report);
            let failure = Arc::clone(&failure);
            let stop = Arc::clone(&stop);
            let started = Arc::clone(&started);
            let feedback = feedback.clone();
            thread::Builder::new()
                .name(format!("amaterasu-ds4-{slot}"))
                .spawn(move || {
                    if let Err(error) =
                        run_uhid_loop(&worker_file, &report, &feedback, &stop, &started)
                    {
                        *failure
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                    }
                })
                .map_err(|error| {
                    let destroy = create_event(UHID_DESTROY);
                    let _ = write_event(&file, &destroy);
                    InputError::new(
                        "controller_unavailable",
                        format!("cannot start UHID event thread: {error}"),
                    )
                })?
        };
        Ok(Self {
            file,
            report,
            failure,
            stop,
            started,
            worker: Some(worker),
            destroyed: false,
        })
    }

    fn send_report(&mut self) -> InputResult<()> {
        self.check_failure()?;
        if !self.started.load(Ordering::Acquire) {
            return Ok(());
        }
        let report = self
            .report
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .input_report();
        write_event(&self.file, &input_report_event(report))
    }

    fn check_failure(&self) -> InputResult<()> {
        if let Some(error) = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            Err(error.clone())
        } else {
            Ok(())
        }
    }

    fn stop_worker(&mut self) -> InputResult<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| InputError::new("release_failed", "DS4 UHID event thread panicked"))?;
        }
        self.started.store(false, Ordering::Release);
        Ok(())
    }

    fn destroy(&mut self) -> InputResult<()> {
        if self.destroyed {
            return Ok(());
        }
        self.stop_worker()?;
        write_event(&self.file, &create_event(UHID_DESTROY))?;
        self.destroyed = true;
        Ok(())
    }
}

#[cfg(not(test))]
impl Drop for UhidDevice {
    fn drop(&mut self) {
        let _ = self.stop_worker();
        if !self.destroyed {
            let _ = write_event(&self.file, &create_event(UHID_DESTROY));
        }
    }
}

#[cfg(not(test))]
fn run_uhid_loop(
    file: &Arc<Mutex<File>>,
    report: &Arc<Mutex<Ds4ReportState>>,
    feedback: &FeedbackProducer,
    stop: &AtomicBool,
    started: &AtomicBool,
) -> InputResult<()> {
    let mut is_started = false;
    let mut next_report = Instant::now();
    while !stop.load(Ordering::Acquire) {
        let timeout = if is_started {
            next_report
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(20))
        } else {
            Duration::from_millis(50)
        };
        let mut poll_descriptor = libc::pollfd {
            fd: file
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: the pollfd pointer addresses one initialized record for the
        // duration of poll; the fd remains owned by the shared File.
        let ready = unsafe {
            libc::poll(
                &mut poll_descriptor,
                1,
                timeout.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(uhid_error(error));
        }
        if poll_descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(InputError::new(
                "controller_unavailable",
                "Linux UHID device stopped accepting events",
            ));
        }
        if poll_descriptor.revents & libc::POLLIN != 0 {
            let event = read_event(poll_descriptor.fd)?;
            match get_u32(&event, 0) {
                UHID_START => {
                    is_started = true;
                    started.store(true, Ordering::Release);
                    next_report = Instant::now();
                }
                UHID_STOP => {
                    is_started = false;
                    started.store(false, Ordering::Release);
                }
                UHID_GET_REPORT => {
                    let report = report
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    write_event(file, &get_report_reply(&event, &report))?;
                }
                UHID_SET_REPORT => {
                    let report = report
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    write_event(file, &set_report_reply(&event, &report))?;
                }
                UHID_OUTPUT => {
                    let _ = dispatch_output_feedback(&event, feedback);
                }
                _ => {}
            }
        }
        if is_started && Instant::now() >= next_report {
            let report = report
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .input_report();
            write_event(file, &input_report_event(report))?;
            next_report = Instant::now() + DS4_REPORT_INTERVAL;
        }
    }
    started.store(false, Ordering::Release);
    Ok(())
}

#[cfg(not(test))]
fn read_event(fd: RawFd) -> InputResult<Vec<u8>> {
    let mut event = vec![0; UHID_EVENT_SIZE];
    loop {
        // SAFETY: the event buffer is writable for the full UHID UAPI packet;
        // the kernel retains no pointer after this synchronous read.
        let bytes = unsafe { libc::read(fd, event.as_mut_ptr().cast(), event.len()) };
        if bytes < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(uhid_error(error));
        }
        if bytes as usize != event.len() {
            return Err(InputError::new(
                "controller_unavailable",
                "Linux UHID returned an incomplete event packet",
            ));
        }
        return Ok(event);
    }
}

#[cfg(not(test))]
fn write_event(file: &Arc<Mutex<File>>, event: &[u8]) -> InputResult<()> {
    if event.len() != UHID_EVENT_SIZE {
        return Err(InputError::new(
            "invalid_event",
            "UHID event does not match the Linux UAPI packet size",
        ));
    }
    let file = file
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        // SAFETY: the event slice contains one complete UAPI packet and stays
        // alive until the synchronous write returns; the kernel stores no pointer.
        let bytes = unsafe { libc::write(file.as_raw_fd(), event.as_ptr().cast(), event.len()) };
        if bytes < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(uhid_error(error));
        }
        if bytes as usize != event.len() {
            return Err(InputError::new(
                "injection_failed",
                "Linux UHID accepted an incomplete event packet",
            ));
        }
        return Ok(());
    }
}

#[cfg(not(test))]
fn uhid_error(error: std::io::Error) -> InputError {
    InputError::new("controller_unavailable", format!("Linux UHID: {error}"))
}

#[cfg(test)]
thread_local! {
    static FAKE_UHID_ADAPTER: std::cell::RefCell<Option<std::rc::Rc<std::cell::RefCell<FakeUhidAdapter>>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct FakeUhidAdapter {
    pub(super) next_id: u8,
    pub(super) created: Vec<(u8, Vec<u8>)>,
    pub(super) reports: Vec<(u8, [u8; DS4_INPUT_REPORT_SIZE])>,
    pub(super) destroyed: Vec<u8>,
    fail_create: bool,
    fail_report: Option<u8>,
    pub(super) fail_destroy: Option<u8>,
}

#[cfg(test)]
pub(super) struct FakeUhidGuard;

#[cfg(test)]
impl FakeUhidGuard {
    pub(super) fn install() -> (Self, std::rc::Rc<std::cell::RefCell<FakeUhidAdapter>>) {
        let adapter = std::rc::Rc::new(std::cell::RefCell::new(FakeUhidAdapter::default()));
        FAKE_UHID_ADAPTER.with(|slot| *slot.borrow_mut() = Some(std::rc::Rc::clone(&adapter)));
        (Self, adapter)
    }
}

#[cfg(test)]
impl Drop for FakeUhidGuard {
    fn drop(&mut self) {
        FAKE_UHID_ADAPTER.with(|slot| *slot.borrow_mut() = None);
    }
}

#[cfg(test)]
struct FakeUhidDevice {
    id: u8,
    adapter: std::rc::Rc<std::cell::RefCell<FakeUhidAdapter>>,
    destroyed: bool,
}

#[cfg(test)]
impl FakeUhidDevice {
    fn create(slot: u8, address: [u8; 6]) -> InputResult<Option<Self>> {
        let Some(adapter) = FAKE_UHID_ADAPTER.with(|slot| slot.borrow().clone()) else {
            return Ok(None);
        };
        let id = {
            let mut state = adapter.borrow_mut();
            if std::mem::take(&mut state.fail_create) {
                return Err(InputError::new(
                    "acquire_failed",
                    "fake UHID creation failed",
                ));
            }
            state.next_id = state.next_id.wrapping_add(1);
            state.next_id
        };
        adapter
            .borrow_mut()
            .created
            .push((slot, create_device_event_with_address(slot, address)));
        Ok(Some(Self {
            id,
            adapter,
            destroyed: false,
        }))
    }

    fn send_report(&mut self, report: &Arc<Mutex<Ds4ReportState>>) -> InputResult<()> {
        let bytes = report
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .input_report();
        let mut state = self.adapter.borrow_mut();
        state.reports.push((self.id, bytes));
        if state.fail_report == Some(self.id) {
            state.fail_report = None;
            return Err(InputError::new(
                "injection_failed",
                "fake UHID report failed",
            ));
        }
        Ok(())
    }

    fn destroy(&mut self) -> InputResult<()> {
        if self.destroyed {
            return Ok(());
        }
        let mut state = self.adapter.borrow_mut();
        state.destroyed.push(self.id);
        if state.fail_destroy == Some(self.id) {
            state.fail_destroy = None;
            return Err(InputError::new(
                "release_failed",
                "fake UHID destroy failed",
            ));
        }
        self.destroyed = true;
        Ok(())
    }
}

#[cfg(test)]
impl Drop for FakeUhidDevice {
    fn drop(&mut self) {
        let _ = self.destroy();
    }
}

enum UhidTransport {
    #[cfg(not(test))]
    Linux(UhidDevice),
    #[cfg(test)]
    Fake(FakeUhidDevice),
}

impl UhidTransport {
    fn create(
        slot: u8,
        _report: Arc<Mutex<Ds4ReportState>>,
        address: [u8; 6],
        _feedback: FeedbackProducer,
    ) -> InputResult<Self> {
        #[cfg(test)]
        if let Some(device) = FakeUhidDevice::create(slot, address)? {
            return Ok(Self::Fake(device));
        }
        #[cfg(test)]
        return Err(InputError::new(
            "controller_unavailable",
            "DS4 UHID tests require the synthetic adapter",
        ));
        #[cfg(not(test))]
        Ok(Self::Linux(UhidDevice::create(
            slot, _report, address, _feedback,
        )?))
    }

    fn send_report(&mut self, _report: &Arc<Mutex<Ds4ReportState>>) -> InputResult<()> {
        match self {
            #[cfg(not(test))]
            Self::Linux(device) => device.send_report(),
            #[cfg(test)]
            Self::Fake(device) => device.send_report(_report),
        }
    }

    fn check_failure(&self) -> InputResult<()> {
        match self {
            #[cfg(not(test))]
            Self::Linux(device) => device.check_failure(),
            #[cfg(test)]
            Self::Fake(_) => Ok(()),
        }
    }

    fn destroy(&mut self) -> InputResult<()> {
        match self {
            #[cfg(not(test))]
            Self::Linux(device) => device.destroy(),
            #[cfg(test)]
            Self::Fake(device) => device.destroy(),
        }
    }
}

pub(super) struct Ds4Gamepad {
    slot: u8,
    address: [u8; 6],
    device: Option<UhidTransport>,
    report: Arc<Mutex<Ds4ReportState>>,
    recovery_required: bool,
    back_as_touchpad_click: bool,
    feedback: FeedbackProducer,
}

impl Ds4Gamepad {
    pub(super) fn create(
        slot: u8,
        back_as_touchpad_click: bool,
        feedback: FeedbackProducer,
    ) -> InputResult<Self> {
        let report = Arc::new(Mutex::new(Ds4ReportState::default()));
        let address = random_device_address(slot)?;
        {
            let mut report_state = report
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            report_state.address = address;
            report_state.back_as_touchpad_click = back_as_touchpad_click;
        }
        Ok(Self {
            slot,
            address,
            device: Some(UhidTransport::create(
                slot,
                Arc::clone(&report),
                address,
                feedback.clone(),
            )?),
            report,
            recovery_required: false,
            back_as_touchpad_click,
            feedback,
        })
    }

    fn mutate_report(
        &mut self,
        update: impl FnOnce(&mut Ds4ReportState) -> InputResult<()>,
    ) -> InputResult<()> {
        if self.recovery_required {
            return Err(InputError::new(
                "controller_unavailable",
                "DS4 UHID device must be detached after a failed report",
            ));
        }
        update(
            &mut self
                .report
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )?;
        let device = self.device.as_mut().ok_or_else(|| {
            InputError::new("controller_unavailable", "DS4 UHID device is reacquiring")
        })?;
        let result = device
            .check_failure()
            .and_then(|()| device.send_report(&self.report));
        if result.is_err() {
            self.recovery_required = true;
        }
        result
    }
}

impl GamepadDevice for Ds4Gamepad {
    fn update(&mut self, state: ControllerSnapshot) -> InputResult<()> {
        self.mutate_report(|report| {
            report.controller = state;
            Ok(())
        })
    }

    fn set_motion(&mut self, motion_type: GamepadMotionType, values: [f32; 3]) -> InputResult<()> {
        self.mutate_report(|report| report.set_motion(motion_type, values))
    }

    fn set_touch(&mut self, touch: GamepadTouchReport) -> InputResult<()> {
        if touch.touchpad_index != 0 {
            return Err(InputError::new(
                "unsupported_event",
                "DS4 exposes one touchpad at index 0",
            ));
        }
        self.mutate_report(|report| {
            report.set_touch(
                touch.pointer_id,
                touch.phase,
                touch.x,
                touch.y,
                touch.pressure,
            )
        })
    }

    fn set_battery(
        &mut self,
        state: GamepadBatteryState,
        percentage: Option<u8>,
    ) -> InputResult<()> {
        self.mutate_report(|report| {
            report.set_battery(state, percentage);
            Ok(())
        })
    }

    fn set_ds4_back_as_touchpad_click(&mut self, enabled: bool) -> bool {
        if self.back_as_touchpad_click == enabled {
            return false;
        }
        self.back_as_touchpad_click = enabled;
        let _ = self.mutate_report(|report| {
            report.back_as_touchpad_click = enabled;
            Ok(())
        });
        true
    }

    fn release(&mut self) -> InputResult<()> {
        if self.device.is_none() {
            return Ok(());
        }
        if !self.recovery_required {
            self.report
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .neutralize();
            if let Some(device) = self.device.as_mut() {
                let _ = device.send_report(&self.report);
            }
        }
        let destroy = self.device.as_mut().expect("device is present").destroy();
        if destroy.is_ok() {
            self.device = None;
            self.recovery_required = false;
        }
        destroy
    }

    fn recover_after_update_failure(&mut self, _error: InputError) -> InputResult<()> {
        if let Some(device) = self.device.as_mut() {
            device.destroy()?;
        }
        self.device = None;
        self.device = Some(UhidTransport::create(
            self.slot,
            Arc::clone(&self.report),
            self.address,
            self.feedback.clone(),
        )?);
        self.recovery_required = false;
        Ok(())
    }
}

impl Drop for Ds4Gamepad {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_feedback(slot: u8) -> FeedbackProducer {
        let queue = crate::feedback::FeedbackQueue::default();
        let generation = queue.begin_lease();
        queue.producer(generation, u16::from(slot), 0)
    }

    fn frame(state: &mut Ds4ReportState) -> [u8; DS4_INPUT_REPORT_SIZE] {
        state.input_report()
    }

    #[test]
    fn descriptor_and_creation_event_publish_usb_ds4_identity_and_report_sizes() {
        assert!(DS4_REPORT_DESCRIPTOR.len() <= MAX_HID_DESCRIPTOR_SIZE);
        assert_eq!(DS4_REPORT_DESCRIPTOR.len(), 507);
        assert_eq!(DS4_INPUT_REPORT_SIZE, 64);
        assert_eq!(DS4_OUTPUT_REPORT_SIZE, 32);
        for report_id in [
            DS4_INPUT_REPORT_ID,
            DS4_OUTPUT_REPORT_ID,
            DS4_CALIBRATION_REPORT_ID,
            DS4_PAIRING_REPORT_ID,
            DS4_CONTROL_REPORT_ID,
            DS4_FIRMWARE_REPORT_ID,
        ] {
            assert!(
                DS4_REPORT_DESCRIPTOR
                    .windows(2)
                    .any(|item| item == [0x85, report_id])
            );
        }
        let event = create_device_event(3);
        assert_eq!(get_u32(&event, 0), UHID_CREATE2);
        assert_eq!(get_u16(&event, 260) as usize, DS4_REPORT_DESCRIPTOR.len());
        assert_eq!(get_u16(&event, 262), BUS_USB);
        assert_eq!(get_u32(&event, 264), DS4_VENDOR_ID);
        assert_eq!(get_u32(&event, 268), DS4_PRODUCT_ID);
        assert_eq!(get_u32(&event, 272), DS4_VERSION);
        assert_eq!(&event[4..25], b"Amaterasu DualShock 4");
        assert_eq!(&event[196..213], b"02:00:00:04:00:03");
    }

    #[test]
    fn ds4_report_projects_face_dpad_shoulders_triggers_and_sticks() {
        let mut state = Ds4ReportState {
            controller: ControllerSnapshot {
                buttons: DPAD_UP
                    | DPAD_LEFT
                    | START
                    | BACK
                    | LEFT_STICK
                    | RIGHT_STICK
                    | LEFT_BUTTON
                    | RIGHT_BUTTON
                    | HOME
                    | A
                    | B
                    | X
                    | Y,
                left_trigger: 91,
                right_trigger: 203,
                left_x: i16::MIN,
                left_y: i16::MAX,
                right_x: 0,
                right_y: i16::MIN,
            },
            ..Ds4ReportState::default()
        };
        let report = frame(&mut state);
        assert_eq!(report[0], DS4_INPUT_REPORT_ID);
        assert_eq!(&report[1..5], &[0, 0, 128, 255]);
        assert_eq!(report[5], 0xf7);
        assert_eq!(report[6], 0xff);
        assert_eq!(report[7] & 0x03, 0x03);
        assert_eq!(&report[8..10], &[91, 203]);
    }

    #[test]
    fn sensor_reports_use_ds4_acceleration_and_degree_per_second_scales() {
        let mut state = Ds4ReportState::default();
        assert_eq!(&frame(&mut state)[19..25], &[0, 0, 0, 32, 0, 0]);
        state
            .set_motion(
                GamepadMotionType::Accel,
                [0.0, STANDARD_GRAVITY, -STANDARD_GRAVITY],
            )
            .unwrap();
        state
            .set_motion(GamepadMotionType::Gyro, [1.0, -2.0, 2048.0])
            .unwrap();
        let report = frame(&mut state);
        assert_eq!(&report[13..19], &[16, 0, 224, 255, 255, 127]);
        assert_eq!(&report[19..25], &[0, 0, 0, 32, 0, 224]);
        assert!(
            state
                .set_motion(GamepadMotionType::Accel, [f32::NAN, 0.0, 0.0])
                .is_err()
        );
    }

    #[test]
    fn touch_reports_pack_two_contacts_and_cancel_all_releases_both() {
        let mut state = Ds4ReportState::default();
        state
            .set_touch(0, GamepadTouchPhase::Down, 0.5, 0.25, 1.0)
            .unwrap();
        state
            .set_touch(1, GamepadTouchPhase::Move, 1.0, 1.0, 0.75)
            .unwrap();
        let report = frame(&mut state);
        assert_eq!(&report[35..39], &[1, 192, 0xb3, 14]);
        assert_eq!(report[43], 0);
        assert_eq!(report[52], 0);
        assert_eq!(&report[39..43], &[2, 128, 0xe7, 58]);
        state
            .set_touch(0, GamepadTouchPhase::CancelAll, 0.0, 0.0, 0.0)
            .unwrap();
        let report = frame(&mut state);
        assert_eq!(report[35] & 0x80, 0x80);
        assert_eq!(report[39] & 0x80, 0x80);
        assert!(
            state
                .set_touch(2, GamepadTouchPhase::Down, 0.0, 0.0, 1.0)
                .is_err()
        );
    }

    #[test]
    fn battery_projection_preserves_unknown_and_marks_charge_state() {
        let mut state = Ds4ReportState::default();
        state.set_battery(GamepadBatteryState::Discharging, Some(69));
        assert_eq!(state.battery_status, 6);
        state.set_battery(GamepadBatteryState::Charging, Some(36));
        assert_eq!(state.battery_status, 0x13);
        state.set_battery(GamepadBatteryState::Full, Some(100));
        assert_eq!(state.battery_status, 0x1b);
        state.set_battery(GamepadBatteryState::Unknown, None);
        assert_eq!(state.battery_status, 0x1b);
        state.set_battery(GamepadBatteryState::NotCharging, Some(20));
        assert_eq!(state.battery_status, 0x1b);
        state.set_battery(GamepadBatteryState::Full, None);
        assert_eq!(state.battery_status, 0x1b);
    }

    #[test]
    fn sony_feature_queries_return_driver_calibration_pairing_control_and_firmware_data() {
        let state = Ds4ReportState::default();
        for (report_id, expected_size) in [
            (DS4_CALIBRATION_REPORT_ID, 37),
            (DS4_PAIRING_REPORT_ID, 16),
            (DS4_CONTROL_REPORT_ID, 17),
            (DS4_FIRMWARE_REPORT_ID, 49),
        ] {
            let mut request = create_event(UHID_GET_REPORT);
            put_u32(&mut request, 4, 0x1234);
            request[8] = report_id;
            request[9] = UHID_FEATURE_REPORT;
            let reply = get_report_reply(&request, &state);
            assert_eq!(get_u32(&reply, 0), UHID_GET_REPORT_REPLY);
            assert_eq!(get_u32(&reply, 4), 0x1234);
            assert_eq!(get_u16(&reply, 8), 0);
            assert_eq!(get_u16(&reply, 10) as usize, expected_size);
            assert_eq!(reply[12], report_id);
        }

        let mut pairing = create_event(UHID_GET_REPORT);
        pairing[8] = DS4_PAIRING_REPORT_ID;
        pairing[9] = UHID_FEATURE_REPORT;
        let reply = get_report_reply(&pairing, &state);
        assert_eq!(&reply[12..21], &[0x12, 0, 0, 4, 0, 0, 2, 8, 0x25]);

        let mut unknown = create_event(UHID_GET_REPORT);
        unknown[8] = 0xfe;
        unknown[9] = UHID_FEATURE_REPORT;
        assert_eq!(
            get_u16(&get_report_reply(&unknown, &state), 8),
            libc::EINVAL as u16
        );
    }

    #[test]
    fn feature_set_and_output_reports_ack_only_the_supported_ds4_shapes() {
        let state = Ds4ReportState::default();
        let mut request = create_event(UHID_SET_REPORT);
        put_u32(&mut request, 4, 0xfeed);
        request[8] = DS4_CONTROL_REPORT_ID;
        request[9] = UHID_FEATURE_REPORT;
        put_u16(&mut request, 10, 17);
        request[12] = DS4_CONTROL_REPORT_ID;
        request[13] = 0x02;
        let reply = set_report_reply(&request, &state);
        assert_eq!(get_u32(&reply, 0), UHID_SET_REPORT_REPLY);
        assert_eq!(get_u32(&reply, 4), 0xfeed);
        assert_eq!(get_u16(&reply, 8), 0);

        request[13] = 0x03;
        assert_eq!(
            get_u16(&set_report_reply(&request, &state), 8),
            libc::EINVAL as u16
        );

        let mut output = create_event(UHID_OUTPUT);
        output[4] = DS4_OUTPUT_REPORT_ID;
        put_u16(
            &mut output,
            4 + UHID_DATA_MAX,
            DS4_OUTPUT_REPORT_SIZE as u16,
        );
        output[4 + UHID_DATA_MAX + 2] = UHID_OUTPUT_REPORT;
        assert!(output_report_is_well_formed(&output));
        output[4] = 0;
        assert!(!output_report_is_well_formed(&output));
    }

    #[test]
    fn output_report_flags_gate_slot_owned_rumble_and_lightbar_feedback() {
        let queue = crate::feedback::FeedbackQueue::default();
        let generation = queue.begin_lease();
        let producer = queue.producer(generation, 3, 0);
        let mut output = create_event(UHID_OUTPUT);
        output[4] = DS4_OUTPUT_REPORT_ID;
        output[5] = 0x03;
        output[8] = 128;
        output[9] = 64;
        output[10..13].copy_from_slice(&[10, 20, 30]);
        put_u16(
            &mut output,
            4 + UHID_DATA_MAX,
            DS4_OUTPUT_REPORT_SIZE as u16,
        );
        output[4 + UHID_DATA_MAX + 2] = UHID_OUTPUT_REPORT;

        assert!(dispatch_output_feedback(&output, &producer));
        let events = queue.peek(4);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].feedback,
            HostFeedback::Rumble {
                controller: 3,
                low_frequency: 64 * 257,
                high_frequency: 128 * 257,
            }
        );
        assert_eq!(
            events[1].feedback,
            HostFeedback::RgbLed {
                controller: 3,
                red: 10,
                green: 20,
                blue: 30,
            }
        );

        for (flags, expected_events) in [(0x01, 1), (0x02, 1), (0, 0)] {
            let generation = queue.begin_lease();
            let producer = queue.producer(generation, 3, 0);
            output[5] = flags;
            assert!(dispatch_output_feedback(&output, &producer));
            let events = queue.peek(4);
            assert_eq!(events.len(), expected_events);
            if flags == 0x01 {
                assert!(matches!(events[0].feedback, HostFeedback::Rumble { .. }));
            } else if flags == 0x02 {
                assert!(matches!(events[0].feedback, HostFeedback::RgbLed { .. }));
            }
        }
    }

    #[test]
    fn back_click_option_adds_touchpad_click_without_changing_share_mapping() {
        let mut state = Ds4ReportState::default();
        state.controller.buttons = BACK;
        let enabled = frame(&mut state);
        assert_eq!(enabled[6] & 0x10, 0x10);
        assert_eq!(enabled[7] & 0x02, 0x02);
        state.back_as_touchpad_click = false;
        let disabled = frame(&mut state);
        assert_eq!(disabled[6] & 0x10, 0x10);
        assert_eq!(disabled[7] & 0x02, 0);
    }

    #[test]
    fn synthetic_uhid_lifecycle_retains_failed_destroy_for_retry() {
        let (guard, adapter) = FakeUhidGuard::install();
        let mut gamepad = Ds4Gamepad::create(4, false, test_feedback(4)).unwrap();
        gamepad.update(ControllerSnapshot::default()).unwrap();
        let created = adapter.borrow().created.clone();
        assert_eq!(created.len(), 1);
        assert_eq!(get_u32(&created[0].1, 0), UHID_CREATE2);
        let id = adapter.borrow().next_id;
        adapter.borrow_mut().fail_destroy = Some(id);
        assert_eq!(gamepad.release().unwrap_err().code, "release_failed");
        assert!(gamepad.device.is_some());
        gamepad.release().unwrap();
        assert!(gamepad.device.is_none());
        drop(guard);
    }

    #[test]
    fn report_write_failure_recreates_identity_before_replay() {
        let (guard, adapter) = FakeUhidGuard::install();
        let mut gamepad = Ds4Gamepad::create(1, false, test_feedback(1)).unwrap();
        let id = adapter.borrow().next_id;
        adapter.borrow_mut().fail_report = Some(id);
        let state = ControllerSnapshot {
            buttons: A,
            ..ControllerSnapshot::default()
        };
        assert!(gamepad.update(state).is_err());
        gamepad
            .recover_after_update_failure(InputError::new("injection_failed", "test"))
            .unwrap();
        gamepad.update(state).unwrap();
        assert_eq!(adapter.borrow().created.len(), 2);
        assert_eq!(
            &adapter.borrow().created[0].1[196..213],
            &adapter.borrow().created[1].1[196..213]
        );
        assert_eq!(adapter.borrow().destroyed, [id]);
        let latest = adapter.borrow().reports.last().unwrap().1;
        assert_eq!(latest[5] & 0x20, 0x20);
        gamepad.release().unwrap();
        drop(guard);
    }
}
