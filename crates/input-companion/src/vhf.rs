//! Fixed keyboard/mouse VHF control protocol and companion-side report state.
//! A transport may be opened only by the separately installed companion;
//! device ACLs remain the final authorization boundary.
use crate::{DeviceSink, InputError, InputEvent, InputResult, MouseButton};

const VERSION: u16 = 1;
const QUERY: u32 = 0x226000;
const CREATE: u32 = 0x22a004;
const KEYBOARD: u32 = 0x22a008;
const MOUSE: u32 = 0x22a00c;
const DESTROY: u32 = 0x22a014;
const HEARTBEAT: u32 = 0x22a018;

pub trait VhfTransport {
    /// Returns exactly the number of response bytes written. A transport error
    /// means delivery may be uncertain, so the caller must neutralize/close.
    fn control(&mut self, ioctl: u32, request: &[u8], response: &mut [u8]) -> InputResult<usize>;
}

pub struct VhfDeviceSink<T: VhfTransport> {
    transport: T,
    modifiers: u8,
    keys: [u8; 6],
    buttons: u8,
    created: bool,
}

impl<T: VhfTransport> VhfDeviceSink<T> {
    pub fn create(mut transport: T) -> InputResult<Self> {
        let mut response = [0u8; 16];
        let read = transport.control(QUERY, &header(8), &mut response)?;
        if read != response.len()
            || response[..8] != header(16)
            || u32::from_le_bytes(response[8..12].try_into().expect("fixed response")) & 3 != 3
            || u32::from_le_bytes(response[12..16].try_into().expect("fixed response")) == 0
        {
            return Err(InputError::Device);
        }
        if transport.control(CREATE, &header(8), &mut [])? != 0 {
            return Err(InputError::Device);
        }
        Ok(Self {
            transport,
            modifiers: 0,
            keys: [0; 6],
            buttons: 0,
            created: true,
        })
    }

    fn keyboard(&mut self, modifiers: u8, keys: [u8; 6]) -> InputResult<()> {
        let mut request = [0u8; 16];
        request[..8].copy_from_slice(&header(16));
        request[8] = modifiers;
        request[9..15].copy_from_slice(&keys);
        if self.transport.control(KEYBOARD, &request, &mut [])? != 0 {
            return Err(InputError::Device);
        }
        self.modifiers = modifiers;
        self.keys = keys;
        Ok(())
    }

    fn mouse(&mut self, buttons: u8, dx: i16, dy: i16, wheel: i16, pan: i16) -> InputResult<()> {
        let mut request = [0u8; 20];
        request[..8].copy_from_slice(&header(20));
        request[8] = buttons;
        for (index, value) in [dx, dy, wheel, pan].into_iter().enumerate() {
            request[10 + 2 * index..12 + 2 * index].copy_from_slice(&value.to_le_bytes());
        }
        if self.transport.control(MOUSE, &request, &mut [])? != 0 {
            return Err(InputError::Device);
        }
        self.buttons = buttons;
        Ok(())
    }

    fn key(&mut self, scan_code: u8, extended: bool, pressed: bool) -> InputResult<()> {
        let (usage, modifier) =
            map_scan_code(scan_code, extended).ok_or(InputError::InvalidEvent)?;
        if let Some(bit) = modifier {
            let updated = if pressed {
                self.modifiers | (1 << bit)
            } else {
                self.modifiers & !(1 << bit)
            };
            return self.keyboard(updated, self.keys);
        }
        let mut keys = self.keys;
        if pressed {
            let index = keys
                .iter()
                .position(|key| *key == 0)
                .ok_or(InputError::ResourceExhausted)?;
            keys[index] = usage;
        } else {
            let index = keys
                .iter()
                .position(|key| *key == usage)
                .ok_or(InputError::InvalidSequence)?;
            keys.copy_within(index + 1..6, index);
            keys[5] = 0;
        }
        self.keyboard(self.modifiers, keys)
    }

    /// Call on the companion timer even when the worker has no input events.
    /// The driver removes the HID child and rejects input after two seconds
    /// without a heartbeat or accepted state update.
    pub fn heartbeat(&mut self) -> InputResult<()> {
        if self.transport.control(HEARTBEAT, &header(8), &mut [])? != 0 {
            return Err(InputError::Device);
        }
        Ok(())
    }
}

impl<T: VhfTransport> DeviceSink for VhfDeviceSink<T> {
    fn keep_alive(&mut self) -> InputResult<()> {
        self.heartbeat()
    }

    fn submit(&mut self, event: &InputEvent) -> InputResult<()> {
        match event {
            InputEvent::Key {
                scan_code,
                extended,
                pressed,
            } => self.key(*scan_code, *extended, *pressed),
            InputEvent::EvdevKey { .. }
            | InputEvent::EvdevKeyRepeat { .. }
            | InputEvent::Absolute { .. }
            | InputEvent::Gamepad { .. } => Err(InputError::InvalidEvent),
            InputEvent::MouseButton { button, pressed } => {
                let bit = match button {
                    MouseButton::Left => 0,
                    MouseButton::Right => 1,
                    MouseButton::Middle => 2,
                    MouseButton::Back => 3,
                    MouseButton::Forward => 4,
                };
                let buttons = if *pressed {
                    self.buttons | (1 << bit)
                } else {
                    self.buttons & !(1 << bit)
                };
                self.mouse(buttons, 0, 0, 0, 0)
            }
            InputEvent::Relative { dx, dy } => self.mouse(self.buttons, *dx, *dy, 0, 0),
            InputEvent::Scroll {
                vertical,
                horizontal,
            } => self.mouse(self.buttons, 0, 0, *vertical, *horizontal),
        }
    }

    fn release_all(&mut self) -> InputResult<()> {
        // Revoke cannot acknowledge cleanup until VhfDelete has completed.
        // A queued neutral report could remain unread after this call returns.
        if self.transport.control(DESTROY, &header(8), &mut [])? != 0 {
            return Err(InputError::Device);
        }
        self.created = false;
        self.modifiers = 0;
        self.keys = [0; 6];
        self.buttons = 0;
        Ok(())
    }
}

impl<T: VhfTransport> Drop for VhfDeviceSink<T> {
    fn drop(&mut self) {
        if self.created {
            let _ = self.transport.control(DESTROY, &header(8), &mut []);
        }
    }
}

fn header(size: u16) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[..2].copy_from_slice(&size.to_le_bytes());
    bytes[2..4].copy_from_slice(&VERSION.to_le_bytes());
    bytes
}

// Set-1 keyboard scan codes, qualified individually against the fixed HID
// keyboard collection. Unknown or ambiguous forms fail closed.
fn map_scan_code(code: u8, extended: bool) -> Option<(u8, Option<u8>)> {
    let mapped = match (code, extended) {
        (0x1d, false) => (0, Some(0)),
        (0x1d, true) => (0, Some(4)),
        (0x2a, false) => (0, Some(1)),
        (0x36, false) => (0, Some(5)),
        (0x38, false) => (0, Some(2)),
        (0x38, true) => (0, Some(6)),
        (0x5b, true) => (0, Some(3)),
        (0x5c, true) => (0, Some(7)),
        (0x01, false) => (0x29, None),
        (0x0e, false) => (0x2a, None),
        (0x0f, false) => (0x2b, None),
        (0x1c, false) => (0x28, None),
        (0x39, false) => (0x2c, None),
        (0x3a, false) => (0x39, None),
        (0x02..=0x0b, false) => (0x1e + (code - 0x02), None),
        (0x10, false) => (0x14, None),
        (0x11, false) => (0x1a, None),
        (0x12, false) => (0x08, None),
        (0x13, false) => (0x15, None),
        (0x14, false) => (0x17, None),
        (0x15, false) => (0x1c, None),
        (0x16, false) => (0x18, None),
        (0x17, false) => (0x0c, None),
        (0x18, false) => (0x12, None),
        (0x19, false) => (0x13, None),
        (0x1e, false) => (0x04, None),
        (0x1f, false) => (0x16, None),
        (0x20, false) => (0x07, None),
        (0x21, false) => (0x09, None),
        (0x22, false) => (0x0a, None),
        (0x23, false) => (0x0b, None),
        (0x24, false) => (0x0d, None),
        (0x25, false) => (0x0e, None),
        (0x26, false) => (0x0f, None),
        (0x2c, false) => (0x1d, None),
        (0x2d, false) => (0x1b, None),
        (0x2e, false) => (0x06, None),
        (0x2f, false) => (0x19, None),
        (0x30, false) => (0x05, None),
        (0x31, false) => (0x11, None),
        (0x32, false) => (0x10, None),
        (0x33, false) => (0x36, None),
        (0x34, false) => (0x37, None),
        (0x35, false) => (0x38, None),
        (0x3b..=0x44, false) => (0x3a + code - 0x3b, None),
        (0x57, false) => (0x44, None),
        (0x58, false) => (0x45, None),
        (0x48, true) => (0x52, None),
        (0x50, true) => (0x51, None),
        (0x4b, true) => (0x50, None),
        (0x4d, true) => (0x4f, None),
        (0x52, true) => (0x49, None),
        (0x53, true) => (0x4c, None),
        (0x47, true) => (0x4a, None),
        (0x4f, true) => (0x4d, None),
        (0x49, true) => (0x4b, None),
        (0x51, true) => (0x4e, None),
        _ => return None,
    };
    Some(mapped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Observed {
        calls: Vec<(u32, Vec<u8>)>,
        fail_next_input: bool,
        child_active: bool,
    }

    struct FixtureTransport(Arc<Mutex<Observed>>);

    impl VhfTransport for FixtureTransport {
        fn control(
            &mut self,
            ioctl: u32,
            request: &[u8],
            response: &mut [u8],
        ) -> InputResult<usize> {
            let mut observed = self.0.lock().expect("fixture lock");
            observed.calls.push((ioctl, request.to_vec()));
            if observed.fail_next_input && (ioctl == KEYBOARD || ioctl == MOUSE) {
                observed.fail_next_input = false;
                return Err(InputError::Device);
            }
            if ioctl == QUERY {
                response.copy_from_slice(&[16, 0, 1, 0, 0, 0, 0, 0, 3, 0, 0, 0, 64, 0, 0, 0]);
                return Ok(16);
            }
            if ioctl == CREATE {
                if observed.child_active {
                    return Err(InputError::Device);
                }
                observed.child_active = true;
            }
            if ioctl == DESTROY {
                // Driver-side watchdog and submit failure may already have
                // removed the child before companion cleanup arrives.
                observed.child_active = false;
            }
            Ok(0)
        }
    }

    fn fixture() -> (VhfDeviceSink<FixtureTransport>, Arc<Mutex<Observed>>) {
        let observed = Arc::new(Mutex::new(Observed::default()));
        let sink = VhfDeviceSink::create(FixtureTransport(Arc::clone(&observed))).unwrap();
        (sink, observed)
    }

    #[test]
    fn fixed_protocol_reports_full_state_and_destroys_child_on_revoke() {
        let (mut sink, observed) = fixture();
        sink.submit(&InputEvent::Key {
            scan_code: 30,
            extended: false,
            pressed: true,
        })
        .unwrap();
        sink.submit(&InputEvent::Key {
            scan_code: 29,
            extended: false,
            pressed: true,
        })
        .unwrap();
        sink.submit(&InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        })
        .unwrap();
        sink.submit(&InputEvent::Relative { dx: -12, dy: 7 })
            .unwrap();
        sink.release_all().unwrap();
        drop(sink);
        let calls = &observed.lock().unwrap().calls;
        assert_eq!(
            calls.iter().map(|call| call.0).collect::<Vec<_>>(),
            [QUERY, CREATE, KEYBOARD, KEYBOARD, MOUSE, MOUSE, DESTROY]
        );
        assert_eq!(calls[2].1[9], 4); // Scan code 30 is HID A.
        assert_eq!(calls[3].1[8], 1); // Left Control modifier.
        assert_eq!(calls[4].1[8], 1); // Left button is held.
        assert_eq!(
            &calls[5].1[10..14],
            &[(-12i16).to_le_bytes(), 7i16.to_le_bytes()].concat()
        );
    }

    #[test]
    fn unsupported_keys_and_rollover_fail_before_device_io() {
        let (mut sink, observed) = fixture();
        assert_eq!(
            sink.submit(&InputEvent::Key {
                scan_code: 0x55,
                extended: false,
                pressed: true
            }),
            Err(InputError::InvalidEvent)
        );
        assert_eq!(
            sink.submit(&InputEvent::EvdevKeyRepeat { code: 30 }),
            Err(InputError::InvalidEvent)
        );
        assert_eq!(
            sink.submit(&InputEvent::Absolute { x: 0, y: u16::MAX }),
            Err(InputError::InvalidEvent)
        );
        assert_eq!(
            sink.submit(&InputEvent::Gamepad {
                number: 0,
                active_mask: 1,
                buttons: 0,
                left_trigger: 0,
                right_trigger: 0,
                left_x: 0,
                left_y: 0,
                right_x: 0,
                right_y: 0,
            }),
            Err(InputError::InvalidEvent)
        );
        for code in [30, 31, 32, 33, 34, 35] {
            sink.submit(&InputEvent::Key {
                scan_code: code,
                extended: false,
                pressed: true,
            })
            .unwrap();
        }
        assert_eq!(
            sink.submit(&InputEvent::Key {
                scan_code: 36,
                extended: false,
                pressed: true
            }),
            Err(InputError::ResourceExhausted)
        );
        assert_eq!(observed.lock().unwrap().calls.len(), 8);
    }

    #[test]
    fn uncertain_device_delivery_keeps_state_until_child_removal() {
        let (mut sink, observed) = fixture();
        observed.lock().unwrap().fail_next_input = true;
        assert_eq!(
            sink.submit(&InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true
            }),
            Err(InputError::Device)
        );
        sink.submit(&InputEvent::Relative { dx: 1, dy: 0 }).unwrap();
        let calls = &observed.lock().unwrap().calls;
        assert_eq!(calls.last().unwrap().1[8], 0);
    }

    #[test]
    fn revoke_after_driver_fault_accepts_absent_child_and_allows_new_lease() {
        let (mut sink, observed) = fixture();
        sink.submit(&InputEvent::Key {
            scan_code: 30,
            extended: false,
            pressed: true,
        })
        .unwrap();
        // Simulate the driver's asynchronous submit failure or watchdog
        // removing its child after the key-down IOCTL was accepted.
        observed.lock().unwrap().child_active = false;
        sink.release_all().unwrap();
        drop(sink);
        let replacement = VhfDeviceSink::create(FixtureTransport(Arc::clone(&observed))).unwrap();
        assert!(observed.lock().unwrap().child_active);
        drop(replacement);
        assert!(!observed.lock().unwrap().child_active);
    }
}
