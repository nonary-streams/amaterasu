//! Linux DualSense USB emulation with a native haptics gadget when available.
//!
//! The report contract follows the pinned Inputtino USB profile at
//! `f9eed6d34ff6cb5a30cfce6ee4e6f5362abedb82`: Sony USB identity, report IDs,
//! sensor scales, touch encoding, feature replies, and the 10 ms input cadence.
//! The pinned virtual gamepad UAPI supplies the USB-audio haptics stream. PCM is
//! forwarded only for clients that advertise the per-controller capability;
//! UHID remains the fallback for systems without the optional gadget driver.

#[path = "linux_ds5_gadget.rs"]
mod gadget;

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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const UHID_EVENT_SIZE: usize = 4 + 128 + 64 + 64 + 2 + 2 + 16 + 4096;
const UHID_DATA_MAX: usize = 4096;
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
const UHID_INPUT2: u32 = 12;
const UHID_SET_REPORT: u32 = 13;
const UHID_SET_REPORT_REPLY: u32 = 14;
const UHID_FEATURE_REPORT: u8 = 0;
const UHID_OUTPUT_REPORT: u8 = 1;
const DS5_INPUT_REPORT_ID: u8 = 0x01;
const DS5_OUTPUT_REPORT_ID: u8 = 0x02;
const DS5_CALIBRATION_REPORT_ID: u8 = 0x05;
const DS5_CONTROL_REPORT_ID: u8 = 0x08;
const DS5_PAIRING_REPORT_ID: u8 = 0x09;
const DS5_FIRMWARE_REPORT_ID: u8 = 0x20;
const DS5_INPUT_REPORT_SIZE: usize = 64;
const DS5_OUTPUT_REPORT_SIZE: usize = 48;
const DS5_VENDOR_ID: u32 = 0x054c;
const DS5_PRODUCT_ID: u32 = 0x0ce6;
const DS5_VERSION: u32 = 0x8111;
const DS5_TOUCHPAD_WIDTH: f32 = 1920.0;
const DS5_TOUCHPAD_HEIGHT: f32 = 1080.0;
const STANDARD_GRAVITY: f32 = 9.80665;
const BUS_USB: u16 = 0x03;
#[cfg(not(test))]
const DS5_REPORT_INTERVAL: Duration = Duration::from_millis(10);
const DS5_PCM_SOURCE_FRAME_BYTES: usize = 8;
const DS5_PCM_ACTUATOR_OFFSET: usize = 4;
const DS5_PCM_ACTUATOR_BYTES: usize = 4;
const DS5_PCM_PACKET_FRAMES: u16 = 240;
const DS5_PCM_PACKET_GAP: Duration = Duration::from_millis(20);

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
const MISC_BUTTON: u32 = 1 << 21;

// USB DualSense descriptor from the pinned Inputtino USB contract. This keeps
// the DS5 feature IDs and report lengths that hid-playstation and native Sony
// clients use to identify and initialize the USB device.
const DS5_REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35,
    0x09, 0x33, 0x09, 0x34, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x06, 0x81, 0x02, 0x06,
    0x00, 0xff, 0x09, 0x20, 0x95, 0x01, 0x81, 0x02, 0x05, 0x01, 0x09, 0x39, 0x15, 0x00, 0x25, 0x07,
    0x35, 0x00, 0x46, 0x3b, 0x01, 0x65, 0x14, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x65, 0x00, 0x05,
    0x09, 0x19, 0x01, 0x29, 0x0f, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x0f, 0x81, 0x02, 0x06,
    0x00, 0xff, 0x09, 0x21, 0x75, 0x01, 0x95, 0x0d, 0x81, 0x02, 0x09, 0x22, 0x15, 0x00, 0x26, 0xff,
    0x00, 0x75, 0x08, 0x95, 0x34, 0x81, 0x02, 0x85, 0x02, 0x09, 0x23, 0x95, 0x2f, 0x91, 0x02, 0x85,
    0x05, 0x09, 0x33, 0x95, 0x28, 0xb1, 0x02, 0x85, 0x08, 0x09, 0x34, 0x95, 0x2f, 0xb1, 0x02, 0x85,
    0x09, 0x09, 0x24, 0x95, 0x13, 0xb1, 0x02, 0x85, 0x0a, 0x09, 0x25, 0x95, 0x1a, 0xb1, 0x02, 0x85,
    0x20, 0x09, 0x26, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0x21, 0x09, 0x27, 0x95, 0x04, 0xb1, 0x02, 0x85,
    0x22, 0x09, 0x40, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0x80, 0x09, 0x28, 0x95, 0x3f, 0xb1, 0x02, 0x85,
    0x81, 0x09, 0x29, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0x82, 0x09, 0x2a, 0x95, 0x09, 0xb1, 0x02, 0x85,
    0x83, 0x09, 0x2b, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0x84, 0x09, 0x2c, 0x95, 0x3f, 0xb1, 0x02, 0x85,
    0x85, 0x09, 0x2d, 0x95, 0x02, 0xb1, 0x02, 0x85, 0xa0, 0x09, 0x2e, 0x95, 0x01, 0xb1, 0x02, 0x85,
    0xe0, 0x09, 0x2f, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xf0, 0x09, 0x30, 0x95, 0x3f, 0xb1, 0x02, 0x85,
    0xf1, 0x09, 0x31, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xf2, 0x09, 0x32, 0x95, 0x0f, 0xb1, 0x02, 0x85,
    0xf4, 0x09, 0x35, 0x95, 0x3f, 0xb1, 0x02, 0x85, 0xf5, 0x09, 0x36, 0x95, 0x03, 0xb1, 0x02, 0xc0,
];

const DS5_CALIBRATION_REPORT: [u8; 41] = [
    0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20, 0x00, 0xe0, 0x00, 0x20, 0x00, 0xe0, 0x00,
    0x20, 0x00, 0xe0, 0x00, 0x02, 0x00, 0x02, 0x00, 0x20, 0x00, 0xe0, 0x00, 0x20, 0x00, 0xe0, 0x00,
    0x20, 0x00, 0xe0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

const DS5_FIRMWARE_REPORT: [u8; 64] = [
    0x20, b'J', b'u', b'n', b' ', b'1', b'9', b' ', b'2', b'0', b'2', b'3', b'1', b'4', b':', b'4',
    b'7', b':', b'3', b'4', 0x03, 0x00, 0x44, 0x00, 0x08, 0x02, 0x00, 0x01, 0x3e, 0x00, 0x00, 0x01,
    0xc1, 0xc8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x90, 0x03, 0x00, 0x00,
    0x14, 0x00, 0x00, 0x00, 0x0b, 0x00, 0x01, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
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
struct Ds5ReportState {
    controller: ControllerSnapshot,
    acceleration: [f32; 3],
    gyroscope: [f32; 3],
    touches: [TouchPoint; 2],
    battery_charge: u8,
    battery_status: u8,
    report_sequence: u8,
    last_touch_id: u8,
}

impl Default for Ds5ReportState {
    fn default() -> Self {
        Self {
            controller: ControllerSnapshot::default(),
            acceleration: [0.0; 3],
            gyroscope: [0.0; 3],
            touches: [TouchPoint::RELEASED; 2],
            battery_charge: 10,
            battery_status: 2,
            report_sequence: 0,
            last_touch_id: 0,
        }
    }
}

impl Ds5ReportState {
    fn input_report(&mut self) -> [u8; DS5_INPUT_REPORT_SIZE] {
        let timestamp = sensor_timestamp();
        self.input_report_at(timestamp)
    }

    fn input_report_at(&mut self, timestamp: u32) -> [u8; DS5_INPUT_REPORT_SIZE] {
        self.report_sequence = self.report_sequence.wrapping_add(1);
        if self.report_sequence == 255 {
            self.report_sequence = 0;
        }

        let mut report = [0; DS5_INPUT_REPORT_SIZE];
        report[0] = DS5_INPUT_REPORT_ID;
        report[1] = scale_axis(i32::from(self.controller.left_x));
        report[2] = scale_axis(-i32::from(self.controller.left_y));
        report[3] = scale_axis(i32::from(self.controller.right_x));
        report[4] = scale_axis(-i32::from(self.controller.right_y));
        report[5] = self.controller.left_trigger;
        report[6] = self.controller.right_trigger;
        report[7] = self.report_sequence;

        let buttons = self.button_bytes();
        report[8..12].copy_from_slice(&buttons);
        write_motion(&mut report[16..22], self.gyroscope, MotionScale::Gyroscope);
        write_motion(
            &mut report[22..28],
            self.acceleration,
            MotionScale::Acceleration,
        );
        report[28..32].copy_from_slice(&timestamp.to_le_bytes());
        write_touch(&mut report[33..37], self.touches[0]);
        write_touch(&mut report[37..41], self.touches[1]);
        report[53] = (self.battery_status << 4) | (self.battery_charge & 0x0f);
        report[54] = 0x0c;
        report
    }

    fn button_bytes(&self) -> [u8; 4] {
        let buttons = self.controller.buttons;
        let mut result = [dpad_hat(buttons), 0, 0, 0];
        if buttons & X != 0 {
            result[0] |= 0x10;
        }
        if buttons & A != 0 {
            result[0] |= 0x20;
        }
        if buttons & B != 0 {
            result[0] |= 0x40;
        }
        if buttons & Y != 0 {
            result[0] |= 0x80;
        }
        if buttons & LEFT_BUTTON != 0 {
            result[1] |= 0x01;
        }
        if buttons & RIGHT_BUTTON != 0 {
            result[1] |= 0x02;
        }
        if self.controller.left_trigger != 0 {
            result[1] |= 0x04;
        }
        if self.controller.right_trigger != 0 {
            result[1] |= 0x08;
        }
        if buttons & BACK != 0 {
            result[1] |= 0x10;
        }
        if buttons & START != 0 {
            result[1] |= 0x20;
        }
        if buttons & LEFT_STICK != 0 {
            result[1] |= 0x40;
        }
        if buttons & RIGHT_STICK != 0 {
            result[1] |= 0x80;
        }
        if buttons & HOME != 0 {
            result[2] |= 0x01;
        }
        if buttons & TOUCHPAD_BUTTON != 0 {
            result[2] |= 0x02;
        }
        if buttons & MISC_BUTTON != 0 {
            result[2] |= 0x04;
        }
        result
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

    fn set_touch(&mut self, touch: GamepadTouchReport) -> InputResult<()> {
        if touch.pointer_id > 1 {
            return Err(InputError::new(
                "unsupported_event",
                "DualSense touchpad supports two pointer IDs (0 and 1)",
            ));
        }
        if [touch.x, touch.y, touch.pressure]
            .into_iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err(InputError::new(
                "invalid_event",
                "controller touch coordinates and pressure must be normalized",
            ));
        }
        if touch.phase == GamepadTouchPhase::CancelAll {
            self.release_touch(0);
            self.release_touch(1);
            return Ok(());
        }
        match touch.phase {
            GamepadTouchPhase::Up | GamepadTouchPhase::Cancel | GamepadTouchPhase::HoverLeave => {
                self.release_touch(touch.pointer_id as usize);
                return Ok(());
            }
            GamepadTouchPhase::ButtonOnly => return Ok(()),
            GamepadTouchPhase::Hover | GamepadTouchPhase::Down | GamepadTouchPhase::Move => {}
            GamepadTouchPhase::CancelAll => unreachable!("handled above"),
        }
        let index = touch.pointer_id as usize;
        if touch.pressure <= 0.5 {
            self.release_touch(index);
            return Ok(());
        }
        if !self.touches[index].active {
            if self.last_touch_id >= 0x7e {
                self.last_touch_id = 0;
            }
            self.last_touch_id += 1;
            self.touches[index].id = self.last_touch_id;
        }
        let point = &mut self.touches[index];
        point.active = true;
        point.x = (touch.x * DS5_TOUCHPAD_WIDTH).round() as u16;
        point.y = (touch.y * DS5_TOUCHPAD_HEIGHT).round() as u16;
        Ok(())
    }

    fn release_touch(&mut self, index: usize) {
        self.touches[index].active = false;
        if self.last_touch_id >= 0x7e {
            self.last_touch_id = 0;
        }
    }

    fn set_battery(&mut self, state: GamepadBatteryState, percentage: Option<u8>) {
        let Some(percentage) = percentage else {
            return;
        };
        let status = match state {
            GamepadBatteryState::Discharging => 0,
            GamepadBatteryState::Charging => 1,
            GamepadBatteryState::Full => 2,
            GamepadBatteryState::Unknown
            | GamepadBatteryState::NotPresent
            | GamepadBatteryState::NotCharging => return,
        };
        self.battery_charge = percentage.min(100) / 10;
        self.battery_status = status;
    }

    fn neutralize(&mut self) {
        self.controller = ControllerSnapshot::default();
        self.acceleration = [0.0; 3];
        self.gyroscope = [0.0; 3];
        self.touches = [TouchPoint::RELEASED; 2];
    }

    fn feature_report(&self, report_id: u8, address: [u8; 6]) -> Option<Vec<u8>> {
        match report_id {
            DS5_CALIBRATION_REPORT_ID => Some(DS5_CALIBRATION_REPORT.to_vec()),
            DS5_PAIRING_REPORT_ID => {
                let mut report = vec![0; 20];
                report[0] = DS5_PAIRING_REPORT_ID;
                for (offset, byte) in address.iter().rev().enumerate() {
                    report[offset + 1] = *byte;
                }
                report[7] = 0x08;
                report[8] = 0x25;
                Some(report)
            }
            DS5_FIRMWARE_REPORT_ID => Some(DS5_FIRMWARE_REPORT.to_vec()),
            _ => None,
        }
    }

    fn accept_feature_report(&self, report_id: u8, data: &[u8]) -> bool {
        report_id == DS5_CONTROL_REPORT_ID
            && data.len() >= 48
            && data[0] == DS5_CONTROL_REPORT_ID
            && data[1] == 2
            && data[2..48].iter().all(|byte| *byte == 0)
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
    target[0] = point.id & 0x7f | if point.active { 0 } else { 0x80 };
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

fn scale_axis(value: i32) -> u8 {
    let normalized =
        (f64::from(value - i32::from(i16::MIN)) / f64::from(i32::from(u16::MAX))) * 255.0;
    normalized.round().clamp(0.0, 255.0) as u8
}

fn sensor_timestamp() -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    (nanos / 333) as u32
}

fn get_report_reply(request: &[u8], report: &Ds5ReportState, address: [u8; 6]) -> Vec<u8> {
    let mut reply = create_event(UHID_GET_REPORT_REPLY);
    put_u32(&mut reply, 4, get_u32(request, 4));
    if request[9] != UHID_FEATURE_REPORT {
        put_u16(&mut reply, 8, libc::EINVAL as u16);
        return reply;
    }
    let Some(feature) = report.feature_report(request[8], address) else {
        put_u16(&mut reply, 8, libc::EINVAL as u16);
        return reply;
    };
    put_u16(&mut reply, 10, feature.len() as u16);
    reply[12..12 + feature.len()].copy_from_slice(&feature);
    reply
}

fn set_report_reply(request: &[u8], report: &Ds5ReportState) -> Vec<u8> {
    let mut reply = create_event(UHID_SET_REPORT_REPLY);
    put_u32(&mut reply, 4, get_u32(request, 4));
    let report_id = request[8];
    let report_type = request[9];
    let report_size = usize::from(get_u16(request, 10));
    let data = &request[12..12 + report_size.min(UHID_DATA_MAX)];
    let accepted = match report_type {
        UHID_FEATURE_REPORT => report.accept_feature_report(report_id, data),
        UHID_OUTPUT_REPORT => {
            report_id == DS5_OUTPUT_REPORT_ID
                && data.len() >= DS5_OUTPUT_REPORT_SIZE
                && data.first() == Some(&DS5_OUTPUT_REPORT_ID)
        }
        _ => false,
    };
    if !accepted || report_size > UHID_DATA_MAX {
        put_u16(&mut reply, 8, libc::EINVAL as u16);
    }
    reply
}

fn output_report_is_well_formed(event: &[u8]) -> bool {
    if event.len() != UHID_EVENT_SIZE {
        return false;
    }
    let size = usize::from(get_u16(event, 4 + UHID_DATA_MAX));
    let report_type = event[4 + UHID_DATA_MAX + 2];
    report_type == UHID_OUTPUT_REPORT
        && size >= DS5_OUTPUT_REPORT_SIZE
        && event.get(4) == Some(&DS5_OUTPUT_REPORT_ID)
}

fn dispatch_output_feedback(event: &[u8], producer: &FeedbackProducer) -> bool {
    if !output_report_is_well_formed(event) {
        return false;
    }
    dispatch_output_report(&event[4..4 + DS5_OUTPUT_REPORT_SIZE], producer)
}

fn dispatch_output_report(report: &[u8], producer: &FeedbackProducer) -> bool {
    if report.len() < DS5_OUTPUT_REPORT_SIZE || report[0] != DS5_OUTPUT_REPORT_ID {
        return false;
    }
    let report = &report[..DS5_OUTPUT_REPORT_SIZE];
    let flags0 = report[1];
    let flags1 = report[2];
    let flags2 = report[39];
    if flags0 & 0x01 != 0 || flags2 & 0x04 != 0 {
        producer.push(HostFeedback::Rumble {
            controller: producer.controller(),
            low_frequency: u16::from(report[4]) * 257,
            high_frequency: u16::from(report[3]) * 257,
        });
    } else if flags0 == 0 && flags1 == 0 && flags2 == 0 {
        producer.push(HostFeedback::Rumble {
            controller: producer.controller(),
            low_frequency: 0,
            high_frequency: 0,
        });
    }
    let event_flags = flags0 & 0x0c;
    if event_flags != 0 {
        let mut left = [0; 10];
        let mut right = [0; 10];
        left.copy_from_slice(&report[23..33]);
        right.copy_from_slice(&report[12..22]);
        producer.push(HostFeedback::AdaptiveTriggers {
            controller: producer.controller(),
            event_flags,
            type_left: report[22],
            type_right: report[11],
            left,
            right,
        });
    }
    if flags1 & 0x04 != 0 {
        producer.push(HostFeedback::RgbLed {
            controller: producer.controller(),
            red: report[45],
            green: report[46],
            blue: report[47],
        });
    }
    true
}

#[derive(Debug)]
struct Ds5HapticsPacketizer {
    sequence: u32,
    frames: usize,
    samples_s16le: [u8; DS5_PCM_PACKET_FRAMES as usize * DS5_PCM_ACTUATOR_BYTES],
    last_input_at: Option<Instant>,
}

impl Default for Ds5HapticsPacketizer {
    fn default() -> Self {
        Self {
            sequence: 0,
            frames: 0,
            samples_s16le: [0; DS5_PCM_PACKET_FRAMES as usize * DS5_PCM_ACTUATOR_BYTES],
            last_input_at: None,
        }
    }
}

impl Ds5HapticsPacketizer {
    fn push(&mut self, pcm: &[u8], producer: &FeedbackProducer, now: Instant) {
        let (source_frames, remainder) = pcm.as_chunks::<DS5_PCM_SOURCE_FRAME_BYTES>();
        if !producer.supports_haptics_pcm() || source_frames.is_empty() || !remainder.is_empty() {
            return;
        }

        if self
            .last_input_at
            .is_some_and(|previous| now.saturating_duration_since(previous) > DS5_PCM_PACKET_GAP)
        {
            if self.frames != 0 {
                self.sequence = self.sequence.wrapping_add(1);
            }
            self.frames = 0;
        }
        self.last_input_at = Some(now);

        for source_frame in source_frames {
            let destination = self.frames * DS5_PCM_ACTUATOR_BYTES;
            self.samples_s16le[destination..destination + DS5_PCM_ACTUATOR_BYTES].copy_from_slice(
                &source_frame
                    [DS5_PCM_ACTUATOR_OFFSET..DS5_PCM_ACTUATOR_OFFSET + DS5_PCM_ACTUATOR_BYTES],
            );
            self.frames += 1;
            if self.frames == usize::from(DS5_PCM_PACKET_FRAMES) {
                let feedback = HostFeedback::HapticsPcm {
                    controller: producer.controller(),
                    sequence: self.sequence,
                    frames: DS5_PCM_PACKET_FRAMES,
                    samples_s16le: self.samples_s16le.to_vec(),
                };
                // PCM is best-effort. Advance the source sequence even when a
                // bounded queue drops this packet so later audio stays ordered.
                producer.push(feedback);
                self.sequence = self.sequence.wrapping_add(1);
                self.frames = 0;
            }
        }
    }
}

fn create_event(event_type: u32) -> Vec<u8> {
    let mut event = vec![0; UHID_EVENT_SIZE];
    put_u32(&mut event, 0, event_type);
    event
}

fn create_device_event(_slot: u8, address: [u8; 6], randomize_mac: bool) -> Vec<u8> {
    let mut event = create_event(UHID_CREATE2);
    let name = b"Sunshine PS5 (virtual) pad";
    let address_text = format_address(address);
    let phys = if randomize_mac {
        "INPUTTINO_USB_LINK"
    } else {
        address_text.as_str()
    };
    let uniq = address_text.as_str();
    event[4..4 + name.len()].copy_from_slice(name);
    event[132..132 + phys.len()].copy_from_slice(phys.as_bytes());
    event[196..196 + uniq.len()].copy_from_slice(uniq.as_bytes());
    put_u16(
        &mut event,
        260,
        u16::try_from(DS5_REPORT_DESCRIPTOR.len()).expect("DS5 descriptor fits u16"),
    );
    put_u16(&mut event, 262, BUS_USB);
    put_u32(&mut event, 264, DS5_VENDOR_ID);
    put_u32(&mut event, 268, DS5_PRODUCT_ID);
    put_u32(&mut event, 272, DS5_VERSION);
    event[280..280 + DS5_REPORT_DESCRIPTOR.len()].copy_from_slice(DS5_REPORT_DESCRIPTOR);
    event
}

fn format_address(address: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        address[0], address[1], address[2], address[3], address[4], address[5]
    )
}

fn device_address(slot: u8, randomize_mac: bool) -> InputResult<[u8; 6]> {
    if !randomize_mac {
        return Ok([0x02, 0, 0, 0, 0, slot]);
    }
    let mut address = [0; 6];
    getrandom::fill(&mut address).map_err(|error| {
        InputError::new(
            "controller_unavailable",
            format!("cannot generate a unique DS5 address: {error}"),
        )
    })?;
    address[0] = (address[0] | 0x02) & 0xfe;
    Ok(address)
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
    report: Arc<Mutex<Ds5ReportState>>,
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
        report: Arc<Mutex<Ds5ReportState>>,
        address: [u8; 6],
        randomize_mac: bool,
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
        write_event(&file, &create_device_event(slot, address, randomize_mac))?;
        let failure = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let worker = {
            let worker_file = Arc::clone(&file);
            let worker_report = Arc::clone(&report);
            let worker_failure = Arc::clone(&failure);
            let worker_stop = Arc::clone(&stop);
            let worker_started = Arc::clone(&started);
            let worker_feedback = feedback.clone();
            thread::Builder::new()
                .name(format!("amaterasu-ds5-{slot}"))
                .spawn(move || {
                    if let Err(error) = run_uhid_loop(
                        &worker_file,
                        &worker_report,
                        address,
                        &worker_feedback,
                        &worker_stop,
                        &worker_started,
                    ) {
                        *worker_failure
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                    }
                })
                .map_err(|error| {
                    let _ = write_event(&file, &create_event(UHID_DESTROY));
                    InputError::new(
                        "controller_unavailable",
                        format!("cannot start DS5 UHID event thread: {error}"),
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
                .map_err(|_| InputError::new("release_failed", "DS5 UHID event thread panicked"))?;
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
fn input_report_event(report: [u8; DS5_INPUT_REPORT_SIZE]) -> Vec<u8> {
    let mut event = create_event(UHID_INPUT2);
    put_u16(
        &mut event,
        4,
        u16::try_from(DS5_INPUT_REPORT_SIZE).expect("DS5 report size fits u16"),
    );
    event[6..6 + DS5_INPUT_REPORT_SIZE].copy_from_slice(&report);
    event
}

#[cfg(not(test))]
fn run_uhid_loop(
    file: &Arc<Mutex<File>>,
    report: &Arc<Mutex<Ds5ReportState>>,
    address: [u8; 6],
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
        // SAFETY: poll_descriptor is one initialized record and the Arc-owned
        // File remains alive for the duration of poll.
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
                "Linux DS5 UHID device stopped accepting events",
            ));
        }
        if poll_descriptor.revents & libc::POLLIN != 0
            && let Some(event) = read_event(poll_descriptor.fd)?
        {
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
                    let report_state = report
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    write_event(file, &get_report_reply(&event, &report_state, address))?;
                }
                UHID_SET_REPORT => {
                    let report_state = report
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    write_event(file, &set_report_reply(&event, &report_state))?;
                }
                UHID_OUTPUT => {
                    let _ = dispatch_output_feedback(&event, feedback);
                }
                _ => {}
            }
        }
        if is_started && Instant::now() >= next_report {
            let input = report
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .input_report();
            write_event(file, &input_report_event(input))?;
            next_report = Instant::now() + DS5_REPORT_INTERVAL;
        }
    }
    started.store(false, Ordering::Release);
    Ok(())
}

#[cfg(not(test))]
fn read_event(fd: RawFd) -> InputResult<Option<Vec<u8>>> {
    let mut event = vec![0; UHID_EVENT_SIZE];
    // SAFETY: event has room for the complete UHID packet and remains writable
    // until the synchronous read returns.
    let bytes = unsafe { libc::read(fd, event.as_mut_ptr().cast(), event.len()) };
    if bytes < 0 {
        let error = std::io::Error::last_os_error();
        if matches!(
            error.kind(),
            std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
        ) {
            return Ok(None);
        }
        return Err(uhid_error(error));
    }
    if bytes as usize != event.len() {
        return Err(InputError::new(
            "controller_unavailable",
            "Linux DS5 UHID returned an incomplete event packet",
        ));
    }
    Ok(Some(event))
}

#[cfg(not(test))]
fn write_event(file: &Arc<Mutex<File>>, event: &[u8]) -> InputResult<()> {
    if event.len() != UHID_EVENT_SIZE {
        return Err(InputError::new(
            "invalid_event",
            "DS5 UHID event does not match the Linux UAPI packet size",
        ));
    }
    let file = file
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        // SAFETY: event contains one complete UAPI packet and remains alive
        // until the synchronous write returns; the kernel retains no pointer.
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
                "Linux DS5 UHID accepted an incomplete event packet",
            ));
        }
        return Ok(());
    }
}

#[cfg(not(test))]
fn uhid_error(error: std::io::Error) -> InputError {
    InputError::new("controller_unavailable", format!("Linux DS5 UHID: {error}"))
}

#[cfg(test)]
thread_local! {
    static FAKE_UHID_ADAPTER: std::cell::RefCell<Option<std::rc::Rc<std::cell::RefCell<FakeUhidAdapter>>>> =
        const { std::cell::RefCell::new(None) };
    static FAKE_DS5_GADGET_ADAPTER: std::cell::RefCell<Option<std::rc::Rc<std::cell::RefCell<FakeGadgetAdapter>>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct FakeUhidAdapter {
    pub(super) next_id: u8,
    pub(super) created: Vec<(u8, Vec<u8>)>,
    pub(super) reports: Vec<(u8, [u8; DS5_INPUT_REPORT_SIZE])>,
    pub(super) destroyed: Vec<u8>,
    pub(super) fail_destroy: Option<u8>,
    fail_create: bool,
    fail_report: Option<u8>,
}

#[cfg(test)]
#[derive(Default)]
struct FakeGadgetAdapter {
    next_id: u8,
    created: Vec<u8>,
    reports: Vec<(u8, [u8; DS5_INPUT_REPORT_SIZE])>,
    destroyed: Vec<u8>,
    fail_create: bool,
}

#[cfg(test)]
struct FakeGadgetGuard;

#[cfg(test)]
impl FakeGadgetGuard {
    fn install() -> (Self, std::rc::Rc<std::cell::RefCell<FakeGadgetAdapter>>) {
        let adapter = std::rc::Rc::new(std::cell::RefCell::new(FakeGadgetAdapter::default()));
        FAKE_DS5_GADGET_ADAPTER
            .with(|slot| *slot.borrow_mut() = Some(std::rc::Rc::clone(&adapter)));
        (Self, adapter)
    }
}

#[cfg(test)]
impl Drop for FakeGadgetGuard {
    fn drop(&mut self) {
        FAKE_DS5_GADGET_ADAPTER.with(|slot| *slot.borrow_mut() = None);
    }
}

#[cfg(test)]
struct FakeGadgetDevice {
    id: u8,
    adapter: std::rc::Rc<std::cell::RefCell<FakeGadgetAdapter>>,
    destroyed: bool,
}

#[cfg(test)]
impl FakeGadgetDevice {
    fn create(_slot: u8, _report: Arc<Mutex<Ds5ReportState>>) -> InputResult<Option<Self>> {
        let Some(adapter) = FAKE_DS5_GADGET_ADAPTER.with(|slot| slot.borrow().clone()) else {
            return Ok(None);
        };
        let id = {
            let mut state = adapter.borrow_mut();
            if std::mem::take(&mut state.fail_create) {
                return Err(InputError::new(
                    "acquire_failed",
                    "synthetic DS5 gadget creation failed",
                ));
            }
            state.next_id = state.next_id.wrapping_add(1);
            state.next_id
        };
        adapter.borrow_mut().created.push(id);
        Ok(Some(Self {
            id,
            adapter,
            destroyed: false,
        }))
    }

    fn send_report(&mut self, report: &Arc<Mutex<Ds5ReportState>>) -> InputResult<()> {
        let bytes = report
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .input_report();
        self.adapter.borrow_mut().reports.push((self.id, bytes));
        Ok(())
    }

    fn destroy(&mut self) -> InputResult<()> {
        if !self.destroyed {
            self.adapter.borrow_mut().destroyed.push(self.id);
            self.destroyed = true;
        }
        Ok(())
    }
}

#[cfg(test)]
impl Drop for FakeGadgetDevice {
    fn drop(&mut self) {
        let _ = self.destroy();
    }
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
    fn create(slot: u8, address: [u8; 6], randomize_mac: bool) -> InputResult<Option<Self>> {
        let Some(adapter) = FAKE_UHID_ADAPTER.with(|slot| slot.borrow().clone()) else {
            return Ok(None);
        };
        let id = {
            let mut state = adapter.borrow_mut();
            if std::mem::take(&mut state.fail_create) {
                return Err(InputError::new(
                    "acquire_failed",
                    "synthetic DS5 UHID creation failed",
                ));
            }
            state.next_id = state.next_id.wrapping_add(1);
            state.next_id
        };
        adapter
            .borrow_mut()
            .created
            .push((slot, create_device_event(slot, address, randomize_mac)));
        Ok(Some(Self {
            id,
            adapter,
            destroyed: false,
        }))
    }

    fn send_report(&mut self, report: &Arc<Mutex<Ds5ReportState>>) -> InputResult<()> {
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
                "synthetic DS5 input report failed",
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
                "synthetic DS5 destroy failed",
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
        _report: Arc<Mutex<Ds5ReportState>>,
        address: [u8; 6],
        randomize_mac: bool,
        _feedback: FeedbackProducer,
    ) -> InputResult<Self> {
        #[cfg(test)]
        if let Some(device) = FakeUhidDevice::create(slot, address, randomize_mac)? {
            return Ok(Self::Fake(device));
        }
        #[cfg(test)]
        return Err(InputError::new(
            "controller_unavailable",
            "DS5 UHID tests require the synthetic adapter",
        ));
        #[cfg(not(test))]
        Ok(Self::Linux(UhidDevice::create(
            slot,
            _report,
            address,
            randomize_mac,
            _feedback,
        )?))
    }

    fn send_report(&mut self, _report: &Arc<Mutex<Ds5ReportState>>) -> InputResult<()> {
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

enum Ds5Transport {
    #[cfg(not(test))]
    Gadget(gadget::GadgetDevice),
    #[cfg(test)]
    FakeGadget(FakeGadgetDevice),
    Uhid(UhidTransport),
}

impl Ds5Transport {
    fn create(
        slot: u8,
        report: Arc<Mutex<Ds5ReportState>>,
        address: [u8; 6],
        randomize_mac: bool,
        feedback: FeedbackProducer,
    ) -> InputResult<Self> {
        #[cfg(not(test))]
        if let Ok(device) =
            gadget::GadgetDevice::create(slot, Arc::clone(&report), address, feedback.clone())
        {
            return Ok(Self::Gadget(device));
        }

        #[cfg(test)]
        if let Ok(Some(device)) = FakeGadgetDevice::create(slot, Arc::clone(&report)) {
            return Ok(Self::FakeGadget(device));
        }

        Ok(Self::Uhid(UhidTransport::create(
            slot,
            report,
            address,
            randomize_mac,
            feedback,
        )?))
    }

    fn send_report(&mut self, report: &Arc<Mutex<Ds5ReportState>>) -> InputResult<()> {
        match self {
            #[cfg(not(test))]
            Self::Gadget(device) => device.send_report(),
            #[cfg(test)]
            Self::FakeGadget(device) => device.send_report(report),
            Self::Uhid(device) => device.send_report(report),
        }
    }

    fn check_failure(&self) -> InputResult<()> {
        match self {
            #[cfg(not(test))]
            Self::Gadget(device) => device.check_failure(),
            #[cfg(test)]
            Self::FakeGadget(_) => Ok(()),
            Self::Uhid(device) => device.check_failure(),
        }
    }

    fn destroy(&mut self) -> InputResult<()> {
        match self {
            #[cfg(not(test))]
            Self::Gadget(device) => device.destroy(),
            #[cfg(test)]
            Self::FakeGadget(device) => device.destroy(),
            Self::Uhid(device) => device.destroy(),
        }
    }
}

pub(super) struct Ds5Gamepad {
    slot: u8,
    address: [u8; 6],
    randomize_mac: bool,
    device: Option<Ds5Transport>,
    report: Arc<Mutex<Ds5ReportState>>,
    recovery_required: bool,
    feedback: FeedbackProducer,
}

impl Ds5Gamepad {
    pub(super) fn create(
        slot: u8,
        randomize_mac: bool,
        feedback: FeedbackProducer,
    ) -> InputResult<Self> {
        let report = Arc::new(Mutex::new(Ds5ReportState::default()));
        let address = device_address(slot, randomize_mac)?;
        Ok(Self {
            slot,
            address,
            randomize_mac,
            device: Some(Ds5Transport::create(
                slot,
                Arc::clone(&report),
                address,
                randomize_mac,
                feedback.clone(),
            )?),
            report,
            recovery_required: false,
            feedback,
        })
    }

    fn mutate_report(
        &mut self,
        update: impl FnOnce(&mut Ds5ReportState) -> InputResult<()>,
    ) -> InputResult<()> {
        if self.recovery_required {
            return Err(InputError::new(
                "controller_unavailable",
                "DS5 UHID device must be detached after a failed report",
            ));
        }
        update(
            &mut self
                .report
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )?;
        let device = self.device.as_mut().ok_or_else(|| {
            InputError::new("controller_unavailable", "DS5 UHID device is reacquiring")
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

impl GamepadDevice for Ds5Gamepad {
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
                "DualSense exposes one touchpad at index 0",
            ));
        }
        self.mutate_report(|report| report.set_touch(touch))
    }

    fn set_battery(
        &mut self,
        state: GamepadBatteryState,
        percentage: Option<u8>,
    ) -> InputResult<()> {
        if percentage.is_none()
            || matches!(
                state,
                GamepadBatteryState::Unknown
                    | GamepadBatteryState::NotPresent
                    | GamepadBatteryState::NotCharging
            )
        {
            return Ok(());
        }
        self.mutate_report(|report| {
            report.set_battery(state, percentage);
            Ok(())
        })
    }

    fn recover_after_update_failure(&mut self, _error: InputError) -> InputResult<()> {
        if let Some(device) = self.device.as_mut() {
            device.destroy()?;
        }
        self.device = None;
        self.device = Some(Ds5Transport::create(
            self.slot,
            Arc::clone(&self.report),
            self.address,
            self.randomize_mac,
            self.feedback.clone(),
        )?);
        self.recovery_required = false;
        Ok(())
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
        let result = self.device.as_mut().expect("device is present").destroy();
        if result.is_ok() {
            self.device = None;
            self.recovery_required = false;
        }
        result
    }
}

impl Drop for Ds5Gamepad {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_feedback(slot: u8) -> FeedbackProducer {
        test_feedback_with_capabilities(slot, 0).1
    }

    fn test_feedback_with_capabilities(
        slot: u8,
        capabilities: u32,
    ) -> (crate::feedback::FeedbackQueue, FeedbackProducer) {
        let queue = crate::feedback::FeedbackQueue::default();
        let generation = queue.begin_lease();
        let producer = queue.producer(generation, u16::from(slot), capabilities);
        (queue, producer)
    }

    fn pcm_frames(frame_count: usize, frame: [u8; DS5_PCM_SOURCE_FRAME_BYTES]) -> Vec<u8> {
        frame.repeat(frame_count)
    }

    #[test]
    fn ds5_input_report_maps_usb_axes_buttons_triggers_and_sequence() {
        let mut state = Ds5ReportState {
            controller: ControllerSnapshot {
                buttons: DPAD_UP
                    | DPAD_RIGHT
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
                    | Y
                    | TOUCHPAD_BUTTON
                    | MISC_BUTTON,
                left_trigger: 12,
                right_trigger: 245,
                left_x: i16::MIN,
                left_y: i16::MAX,
                right_x: 0,
                right_y: i16::MIN,
            },
            ..Ds5ReportState::default()
        };
        let report = state.input_report_at(0x1234_5678);
        assert_eq!(report.len(), DS5_INPUT_REPORT_SIZE);
        assert_eq!(report[0], DS5_INPUT_REPORT_ID);
        assert_eq!(&report[1..7], &[0, 0, 128, 255, 12, 245]);
        assert_eq!(report[7], 1);
        assert_eq!(report[8], 0xf1);
        assert_eq!(report[9], 0xff);
        assert_eq!(report[10], 0x07);
        assert_eq!(report[11], 0);
        assert_eq!(&report[28..32], &0x1234_5678u32.to_le_bytes());
        assert_eq!(report[53], 0x2a);
        assert_eq!(report[54], 0x0c);
        assert_eq!(state.input_report_at(0).get(7), Some(&2));
    }

    #[test]
    fn ds5_motion_touch_and_battery_fields_match_the_reference_units() {
        let mut state = Ds5ReportState::default();
        state
            .set_motion(
                GamepadMotionType::Accel,
                [0.0, STANDARD_GRAVITY, -STANDARD_GRAVITY],
            )
            .unwrap();
        state
            .set_motion(GamepadMotionType::Gyro, [1.0, -2.0, 180.0])
            .unwrap();
        let touches = [
            GamepadTouchReport {
                touchpad_index: 0,
                pointer_id: 0,
                phase: GamepadTouchPhase::Down,
                x: 0.5,
                y: 0.25,
                pressure: 1.0,
            },
            GamepadTouchReport {
                touchpad_index: 0,
                pointer_id: 1,
                phase: GamepadTouchPhase::Move,
                x: 1.0,
                y: 1.0,
                pressure: 0.75,
            },
        ];
        for touch in touches {
            state.set_touch(touch).unwrap();
        }
        state.set_battery(GamepadBatteryState::Discharging, Some(69));
        let report = state.input_report_at(5);
        assert_eq!(&report[16..22], &[16, 0, 224, 255, 0x40, 0x0b]);
        assert_eq!(&report[22..28], &[0, 0, 0, 32, 0, 224]);
        assert_eq!(&report[33..37], &[1, 0xc0, 0xe3, 0x10]);
        assert_eq!(&report[37..41], &[2, 0x80, 0x87, 0x43]);
        assert_eq!(report[53], 0x06);
        assert_eq!(
            state
                .set_motion(GamepadMotionType::Gyro, [f32::NAN, 0.0, 0.0])
                .unwrap_err()
                .code,
            "invalid_event"
        );
        state
            .set_touch(GamepadTouchReport {
                touchpad_index: 0,
                pointer_id: 0,
                phase: GamepadTouchPhase::CancelAll,
                x: 0.0,
                y: 0.0,
                pressure: 0.0,
            })
            .unwrap();
        let cancelled = state.input_report_at(6);
        assert_eq!(cancelled[33] & 0x80, 0x80);
        assert_eq!(cancelled[37] & 0x80, 0x80);
    }

    #[test]
    fn ds5_feature_reports_and_control_command_match_usb_descriptor() {
        let state = Ds5ReportState::default();
        let address = [0x02, 0x00, 0x00, 0x00, 0x00, 0x07];
        let calibration = state
            .feature_report(DS5_CALIBRATION_REPORT_ID, address)
            .unwrap();
        assert_eq!(calibration, DS5_CALIBRATION_REPORT);
        let pairing = state
            .feature_report(DS5_PAIRING_REPORT_ID, address)
            .unwrap();
        assert_eq!(
            &pairing[..10],
            &[0x09, 0x07, 0x00, 0x00, 0x00, 0x00, 0x02, 0x08, 0x25, 0x00]
        );
        let firmware = state
            .feature_report(DS5_FIRMWARE_REPORT_ID, address)
            .unwrap();
        assert_eq!(firmware, DS5_FIRMWARE_REPORT);
        assert!(state.feature_report(0x0a, address).is_none());
        assert!(state.accept_feature_report(
            DS5_CONTROL_REPORT_ID,
            &[&[DS5_CONTROL_REPORT_ID, 2][..], &[0; 46]].concat()
        ));
        assert!(
            !state.accept_feature_report(DS5_CONTROL_REPORT_ID, &[DS5_CONTROL_REPORT_ID, 1, 0, 0])
        );
        assert_eq!(DS5_REPORT_DESCRIPTOR.len(), 272);
        assert!(
            DS5_REPORT_DESCRIPTOR
                .windows(4)
                .any(|bytes| bytes == [0x85, 0x20, 0x09, 0x26])
        );
    }

    #[test]
    fn create2_identity_and_output_report_validation_are_explicit() {
        let address = [0x02, 0, 0, 0, 0, 7];
        let stable = create_device_event(7, address, false);
        assert_eq!(get_u32(&stable, 0), UHID_CREATE2);
        assert_eq!(&stable[4..30], b"Sunshine PS5 (virtual) pad");
        assert_eq!(&stable[132..149], b"02:00:00:00:00:07");
        assert_eq!(&stable[196..213], b"02:00:00:00:00:07");
        assert_eq!(get_u16(&stable, 260) as usize, DS5_REPORT_DESCRIPTOR.len());
        assert_eq!(get_u16(&stable, 262), BUS_USB);
        assert_eq!(get_u32(&stable, 264), DS5_VENDOR_ID);
        assert_eq!(get_u32(&stable, 268), DS5_PRODUCT_ID);
        assert_eq!(get_u32(&stable, 272), DS5_VERSION);
        assert_eq!(
            &stable[280..280 + DS5_REPORT_DESCRIPTOR.len()],
            DS5_REPORT_DESCRIPTOR
        );
        let randomized = create_device_event(7, address, true);
        assert_eq!(&randomized[132..150], b"INPUTTINO_USB_LINK");
        assert_eq!(&randomized[196..213], b"02:00:00:00:00:07");

        let mut output = create_event(UHID_OUTPUT);
        output[4] = DS5_OUTPUT_REPORT_ID;
        put_u16(
            &mut output,
            4 + UHID_DATA_MAX,
            u16::try_from(DS5_OUTPUT_REPORT_SIZE).unwrap(),
        );
        output[4 + UHID_DATA_MAX + 2] = UHID_OUTPUT_REPORT;
        assert!(output_report_is_well_formed(&output));
        output[4 + UHID_DATA_MAX] = 47;
        assert!(!output_report_is_well_formed(&output));
        assert!(!output_report_is_well_formed(&output[..16]));
        assert_eq!(device_address(7, false).unwrap(), address);
        let randomized_address = device_address(7, true).unwrap();
        assert_eq!(randomized_address[0] & 0x03, 0x02);
    }

    #[test]
    fn valid_output_report_enqueues_slot_owned_rumble_triggers_and_lightbar() {
        let queue = crate::feedback::FeedbackQueue::default();
        let generation = queue.begin_lease();
        let producer = queue.producer(generation, 4, 0);
        let mut output = create_event(UHID_OUTPUT);
        output[4] = DS5_OUTPUT_REPORT_ID;
        output[5] = 0x0d;
        output[6] = 0x04;
        output[7] = 128;
        output[8] = 64;
        output[15] = 3;
        output[16..26].copy_from_slice(&[1; 10]);
        output[26] = 2;
        output[27..37].copy_from_slice(&[2; 10]);
        output[49..52].copy_from_slice(&[10, 20, 30]);
        put_u16(
            &mut output,
            4 + UHID_DATA_MAX,
            u16::try_from(DS5_OUTPUT_REPORT_SIZE).unwrap(),
        );
        output[4 + UHID_DATA_MAX + 2] = UHID_OUTPUT_REPORT;

        assert!(dispatch_output_feedback(&output, &producer));
        let events = queue.peek(4);
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[0].feedback,
            HostFeedback::Rumble {
                controller: 4,
                low_frequency: 64 * 257,
                high_frequency: 128 * 257,
            }
        );
        assert_eq!(
            events[1].feedback,
            HostFeedback::AdaptiveTriggers {
                controller: 4,
                event_flags: 0x0c,
                type_left: 2,
                type_right: 3,
                left: [2; 10],
                right: [1; 10],
            }
        );
        assert_eq!(
            events[2].feedback,
            HostFeedback::RgbLed {
                controller: 4,
                red: 10,
                green: 20,
                blue: 30,
            }
        );
    }

    #[test]
    fn pcm_packetizer_selects_actuator_channels_and_emits_bounded_five_ms_packets() {
        let (queue, producer) = test_feedback_with_capabilities(3, 0x8000);
        assert!(producer.supports_haptics_pcm());
        let source_frame = [0x11, 0x22, 0x33, 0x44, 0x85, 0x96, 0xa7, 0xb8];
        let source = pcm_frames(DS5_PCM_PACKET_FRAMES as usize, source_frame);
        let mut packetizer = Ds5HapticsPacketizer::default();
        let start = Instant::now();

        packetizer.push(
            &source[..239 * DS5_PCM_SOURCE_FRAME_BYTES],
            &producer,
            start,
        );
        assert!(queue.peek(16).is_empty());
        assert_eq!(packetizer.frames, 239);

        packetizer.push(
            &source[239 * DS5_PCM_SOURCE_FRAME_BYTES..],
            &producer,
            start,
        );
        let queued = queue.peek(16);
        assert_eq!(queued.len(), 1);
        assert_eq!(
            queued[0].feedback,
            HostFeedback::HapticsPcm {
                controller: 3,
                sequence: 0,
                frames: DS5_PCM_PACKET_FRAMES,
                samples_s16le: [0x85, 0x96, 0xa7, 0xb8].repeat(240),
            }
        );
        assert_eq!(packetizer.frames, 0);
        assert_eq!(packetizer.sequence, 1);
    }

    #[test]
    fn pcm_packetizer_requires_capability_and_aligned_source_frames() {
        let (disabled_queue, disabled) = test_feedback_with_capabilities(0, 0);
        assert!(!disabled.supports_haptics_pcm());
        let mut disabled_packetizer = Ds5HapticsPacketizer::default();
        let packet = pcm_frames(usize::from(DS5_PCM_PACKET_FRAMES), [0; 8]);
        disabled_packetizer.push(&packet, &disabled, Instant::now());
        assert!(disabled_queue.peek(16).is_empty());
        assert_eq!(disabled_packetizer.frames, 0);

        let (queue, producer) = test_feedback_with_capabilities(0, 0x8000);
        let mut packetizer = Ds5HapticsPacketizer::default();
        packetizer.push(&[0; 7], &producer, Instant::now());
        assert_eq!(packetizer.frames, 0);
        assert!(queue.peek(16).is_empty());
    }

    #[test]
    fn pcm_packetizer_discards_stale_partial_packet_and_wraps_sequence() {
        let (queue, producer) = test_feedback_with_capabilities(2, 0x8000);
        let mut packetizer = Ds5HapticsPacketizer {
            sequence: u32::MAX,
            ..Ds5HapticsPacketizer::default()
        };
        let start = Instant::now();
        let old_frame = [1, 2, 3, 4, 5, 6, 7, 8];
        let new_frame = [9, 10, 11, 12, 0x81, 0x82, 0x83, 0x84];

        packetizer.push(&old_frame, &producer, start);
        packetizer.push(
            &new_frame,
            &producer,
            start + DS5_PCM_PACKET_GAP + Duration::from_millis(1),
        );
        assert_eq!(packetizer.frames, 1);
        assert_eq!(packetizer.sequence, 0);

        let remainder = pcm_frames(239, new_frame);
        packetizer.push(
            &remainder,
            &producer,
            start + DS5_PCM_PACKET_GAP + Duration::from_millis(2),
        );
        let queued = queue.peek(16);
        assert_eq!(queued.len(), 1);
        let HostFeedback::HapticsPcm {
            controller,
            sequence,
            frames,
            samples_s16le,
        } = &queued[0].feedback
        else {
            panic!("expected PCM feedback packet");
        };
        assert_eq!(*controller, 2);
        assert_eq!(*sequence, 0);
        assert_eq!(*frames, DS5_PCM_PACKET_FRAMES);
        assert_eq!(samples_s16le, &[0x81, 0x82, 0x83, 0x84].repeat(240));
        assert_eq!(packetizer.sequence, 1);

        packetizer.push(
            &pcm_frames(240, new_frame),
            &producer,
            start + DS5_PCM_PACKET_GAP + Duration::from_millis(3),
        );
        let queued = queue.peek(16);
        assert_eq!(queued.len(), 2);
        assert!(matches!(
            queued[1].feedback,
            HostFeedback::HapticsPcm { sequence: 1, .. }
        ));
        assert_eq!(packetizer.sequence, 2);
    }

    #[test]
    fn native_gadget_is_preferred_and_release_leaves_one_visible_target() {
        let (_gadget_guard, gadget) = FakeGadgetGuard::install();
        let (_uhid_guard, uhid) = FakeUhidGuard::install();
        let mut device = Ds5Gamepad::create(4, false, test_feedback(4)).unwrap();
        device
            .update(ControllerSnapshot {
                buttons: A,
                ..ControllerSnapshot::default()
            })
            .unwrap();
        device.release().unwrap();

        assert_eq!(gadget.borrow().created.len(), 1);
        assert!(!gadget.borrow().reports.is_empty());
        assert_eq!(gadget.borrow().destroyed.len(), 1);
        assert!(uhid.borrow().created.is_empty());
    }

    #[test]
    fn native_gadget_acquisition_failure_falls_back_to_one_uhid_target() {
        let (_gadget_guard, gadget) = FakeGadgetGuard::install();
        gadget.borrow_mut().fail_create = true;
        let (_uhid_guard, uhid) = FakeUhidGuard::install();
        let mut device = Ds5Gamepad::create(4, false, test_feedback(4)).unwrap();
        assert!(gadget.borrow().created.is_empty());
        assert_eq!(uhid.borrow().created.len(), 1);
        device.release().unwrap();
        assert_eq!(uhid.borrow().destroyed.len(), 1);
    }

    #[test]
    fn get_and_set_report_replies_reject_unsupported_reports() {
        let state = Ds5ReportState::default();
        let address = [0x02, 0, 0, 0, 0, 3];
        let mut get = create_event(UHID_GET_REPORT);
        put_u32(&mut get, 4, 91);
        get[8] = DS5_FIRMWARE_REPORT_ID;
        get[9] = UHID_FEATURE_REPORT;
        let reply = get_report_reply(&get, &state, address);
        assert_eq!(get_u32(&reply, 0), UHID_GET_REPORT_REPLY);
        assert_eq!(get_u32(&reply, 4), 91);
        assert_eq!(get_u16(&reply, 8), 0);
        assert_eq!(get_u16(&reply, 10), 64);
        assert_eq!(&reply[12..76], &DS5_FIRMWARE_REPORT);

        get[8] = DS5_OUTPUT_REPORT_ID;
        let reply = get_report_reply(&get, &state, address);
        assert_eq!(get_u16(&reply, 8), libc::EINVAL as u16);

        let mut set = create_event(UHID_SET_REPORT);
        put_u32(&mut set, 4, 92);
        set[8] = DS5_CONTROL_REPORT_ID;
        set[9] = UHID_FEATURE_REPORT;
        put_u16(&mut set, 10, 48);
        set[12] = DS5_CONTROL_REPORT_ID;
        set[13] = 2;
        let reply = set_report_reply(&set, &state);
        assert_eq!(get_u32(&reply, 0), UHID_SET_REPORT_REPLY);
        assert_eq!(get_u32(&reply, 4), 92);
        assert_eq!(get_u16(&reply, 8), 0);

        set[8] = DS5_OUTPUT_REPORT_ID;
        set[9] = UHID_OUTPUT_REPORT;
        put_u16(&mut set, 10, u16::try_from(DS5_OUTPUT_REPORT_SIZE).unwrap());
        set[12] = DS5_OUTPUT_REPORT_ID;
        assert_eq!(get_u16(&set_report_reply(&set, &state), 8), 0);
        set[12] = 0xff;
        assert_eq!(
            get_u16(&set_report_reply(&set, &state), 8),
            libc::EINVAL as u16
        );
    }

    #[test]
    fn synthetic_device_failure_recovery_keeps_identity_and_release_retries() {
        let (guard, adapter) = FakeUhidGuard::install();
        let mut device = Ds5Gamepad::create(4, false, test_feedback(4)).unwrap();
        device
            .update(ControllerSnapshot {
                buttons: A,
                ..ControllerSnapshot::default()
            })
            .unwrap();
        let first_id = adapter.borrow().next_id;
        adapter.borrow_mut().fail_report = Some(first_id);
        let error = device
            .update(ControllerSnapshot {
                buttons: B,
                ..ControllerSnapshot::default()
            })
            .unwrap_err();
        assert_eq!(error.code, "injection_failed");
        assert!(device.recovery_required);
        device.recover_after_update_failure(error).unwrap();
        assert!(!device.recovery_required);
        device
            .update(ControllerSnapshot {
                buttons: B,
                ..ControllerSnapshot::default()
            })
            .unwrap();
        let created = adapter.borrow().created.clone();
        assert_eq!(created.len(), 2);
        assert_eq!(&created[0].1[196..213], &created[1].1[196..213]);
        let active_id = adapter.borrow().next_id;
        adapter.borrow_mut().fail_destroy = Some(active_id);
        assert_eq!(device.release().unwrap_err().code, "release_failed");
        assert!(device.device.is_some());
        device.release().unwrap();
        assert!(adapter.borrow().destroyed.contains(&first_id));
        assert!(adapter.borrow().destroyed.contains(&active_id));
        drop(guard);
    }
}
