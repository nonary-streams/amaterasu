//! Explicit native qualification. Default tests never create input devices.
//! The exact owned input device is grabbed before any report is emitted, keeping
//! reports out of the desktop's other input consumers. No permissions change.

use super::*;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const NATIVE_TIMEOUT: Duration = Duration::from_secs(3);
const EVIOCGRAB: libc::Ioctl = libc::_IOW::<libc::c_int>(b'E' as u32, 0x90);
const GAMEPAD_BUTTON_CODES: [u16; 23] = [
    0x220, 0x221, 0x222, 0x223, // D-pad.
    0x13b, 0x13a, 0x13d, 0x13e, 0x136, 0x137, 0x13c, // Menu/sticks/shoulders/Guide.
    0x130, 0x131, 0x134, 0x133, // A/B/X/Y.
    0x2c0, 0x2c1, 0x2c2, 0x2c3, 0x2c4, 0x2c5, // Additional protocol buttons.
    0x138, 0x139, // Digital trigger companions.
];

// Native production writes and release remain unchanged. Failures propagate
// through the trait's default recovery instead of recreating an ungrabbed
// device and replaying a report outside this test's exclusive reader.
struct GrabbedGamepad(UinputGamepad);

impl GamepadDevice for GrabbedGamepad {
    fn update(&mut self, state: ControllerSnapshot) -> InputResult<()> {
        self.0.update(state)
    }

    fn release(&mut self) -> InputResult<()> {
        self.0.release()
    }
}

struct NativeEventReader {
    file: File,
    sysfs_path: PathBuf,
    event_path: PathBuf,
}

impl NativeEventReader {
    fn open(device: &Device) -> io::Result<Self> {
        assert!(
            device.fake.is_none(),
            "native test cannot use a fake adapter"
        );
        let mut sysname = [0_u8; 128];
        // UI_GET_SYSNAME has a variable-length output buffer. _IOR encodes
        // precisely the same direction and size as the installed Linux UAPI.
        let request = libc::_IOR::<[u8; 128]>(b'U' as u32, 44);
        // SAFETY: this live descriptor owns the newly created device, and the
        // initialized output array remains writable for the encoded 128 bytes.
        ioctl_result(unsafe {
            libc::ioctl(device.file.as_raw_fd(), request, sysname.as_mut_ptr())
        })?;
        let sysname = nul_terminated_text(&sysname)?;
        let suffix = sysname.strip_prefix("input").unwrap_or_default();
        if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(io::Error::other("uinput returned an invalid sysfs name"));
        }
        let sysfs_path = Path::new("/sys/devices/virtual/input").join(sysname);
        let deadline = Instant::now() + NATIVE_TIMEOUT;
        loop {
            match Self::open_event_node(&sysfs_path) {
                Ok((file, event_path)) => {
                    // SAFETY: EVIOCGRAB takes a scalar boolean. This fd is the
                    // exact owned uinput device's evdev node, never a physical
                    // or unrelated input device. No report has been sent yet.
                    ioctl_result(unsafe { libc::ioctl(file.as_raw_fd(), EVIOCGRAB, 1) })?;
                    return Ok(Self {
                        file,
                        sysfs_path,
                        event_path,
                    });
                }
                Err(error) if Instant::now() >= deadline => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!("cannot open owned input event node: {error}"),
                    ));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    fn open_event_node(sysfs_path: &Path) -> io::Result<(File, PathBuf)> {
        let mut events = Vec::new();
        for entry in std::fs::read_dir(sysfs_path)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Some(suffix) = name.strip_prefix("event")
                && !suffix.is_empty()
                && suffix.bytes().all(|byte| byte.is_ascii_digit())
            {
                events.push(entry.path());
            }
        }
        if events.len() != 1 {
            return Err(io::Error::other(
                "owned input device must have one evdev node",
            ));
        }
        let sysfs_event = &events[0];
        let event_path = Path::new("/dev/input").join(
            sysfs_event
                .file_name()
                .expect("sysfs event path has a file name"),
        );
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&event_path)?;
        let metadata = file.metadata()?;
        if !metadata.file_type().is_char_device() {
            return Err(io::Error::other(
                "owned evdev node is not a character device",
            ));
        }
        let device_numbers = std::fs::read_to_string(sysfs_event.join("dev"))?;
        let actual_numbers = format!(
            "{}:{}",
            libc::major(metadata.rdev()),
            libc::minor(metadata.rdev())
        );
        if device_numbers.trim() != actual_numbers {
            return Err(io::Error::other(
                "evdev node does not match owned sysfs device",
            ));
        }
        Ok((file, event_path))
    }

    fn assert_identity(&self, expected_name: &str) {
        let mut name = [0_u8; 128];
        let request = libc::_IOR::<[u8; 128]>(b'E' as u32, 0x06);
        // SAFETY: EVIOCGNAME writes at most the encoded array size to this
        // initialized buffer, and retains no pointer after the live fd ioctl.
        ioctl_result(unsafe { libc::ioctl(self.file.as_raw_fd(), request, name.as_mut_ptr()) })
            .expect("read owned input name");
        assert_eq!(nul_terminated_text(&name).unwrap(), expected_name);
        // SAFETY: input_id contains only integer fields, and the initialized
        // record remains writable for the exact size encoded by EVIOCGID.
        let mut identity: libc::input_id = unsafe { std::mem::zeroed() };
        let request = libc::_IOR::<libc::input_id>(b'E' as u32, 0x02);
        // SAFETY: the live evdev ioctl writes only the input_id record.
        ioctl_result(unsafe { libc::ioctl(self.file.as_raw_fd(), request, &mut identity) })
            .expect("read owned input identity");
        assert_eq!(identity.bustype, 0x06, "virtual bus identity");
        assert_eq!(
            (identity.vendor, identity.product, identity.version),
            (0, 0, 1)
        );
    }

    fn read_event(&self) -> io::Result<libc::input_event> {
        // SAFETY: input_event contains only integer fields. Its initialized
        // storage remains writable for this one-record read; no pointer escapes.
        let mut event: libc::input_event = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&event);
        // SAFETY: the read buffer owns exactly size writable bytes and fd is a
        // live nonblocking evdev descriptor. The kernel retains no pointer.
        let count = unsafe {
            libc::read(
                self.file.as_raw_fd(),
                (&mut event as *mut libc::input_event).cast(),
                size,
            )
        };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count as usize != size {
            return Err(io::Error::other(
                "evdev returned an incomplete event record",
            ));
        }
        Ok(event)
    }

    fn read_report(&self, expected_events: usize) -> Vec<(u16, u16, i32)> {
        let deadline = Instant::now() + NATIVE_TIMEOUT;
        let mut frame = Vec::new();
        let mut sync_groups = 0;
        loop {
            match self.read_event() {
                Ok(event) if event.type_ == EV_SYN && event.code == 0 => {
                    // input_event_dispose may insert SYN_REPORT when the
                    // kernel's value buffer fills, splitting one uinput write.
                    // Expected values remain independent and complete across
                    // those groups; no SYN value is treated as a public marker.
                    sync_groups += 1;
                    assert!(sync_groups <= 128, "native report has too many sync groups");
                    if frame.len() == expected_events {
                        let error = self
                            .read_event()
                            .expect_err("native report has extra events");
                        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
                        return frame;
                    }
                }
                Ok(event) => {
                    assert_ne!((event.type_, event.code), (EV_SYN, 3), "SYN_DROPPED");
                    assert!(frame.len() < 128, "native frame exceeded its bounded size");
                    frame.push((event.type_, event.code, event.value));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "native report timed out: {frame:?}"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    assert!(
                        Instant::now() < deadline,
                        "native read was repeatedly interrupted"
                    );
                }
                Err(error) => panic!("read native controller report: {error}"),
            }
        }
    }

    fn assert_kernel_state(&self, pressed: bool, axis_values: [i32; 6]) {
        let mut keys = [0_u8; 96]; // Linux KEY_CNT == 0x300.
        let request = libc::_IOR::<[u8; 96]>(b'E' as u32, 0x18);
        // SAFETY: EVIOCGKEY writes at most the encoded 96 bytes into this
        // initialized array while this exact controller fd remains live.
        ioctl_result(unsafe { libc::ioctl(self.file.as_raw_fd(), request, keys.as_mut_ptr()) })
            .expect("read native controller key state");
        for code in GAMEPAD_BUTTON_CODES {
            let code = usize::from(code);
            assert_eq!(
                keys[code / 8] & (1 << (code % 8)) != 0,
                pressed,
                "key {code:#x}"
            );
        }
        for (axis, expected_value) in axis_values.into_iter().enumerate() {
            // SAFETY: input_absinfo contains only integer fields; initialized
            // storage remains writable for the encoded EVIOCGABS record size.
            let mut info: libc::input_absinfo = unsafe { std::mem::zeroed() };
            let request = libc::_IOR::<libc::input_absinfo>(b'E' as u32, 0x40 + axis as u32);
            // SAFETY: the live fd ioctl writes only this input_absinfo record.
            ioctl_result(unsafe { libc::ioctl(self.file.as_raw_fd(), request, &mut info) })
                .expect("read native controller axis state");
            assert_eq!(info.value, expected_value, "axis {axis}");
            let bounds = if matches!(axis, 2 | 5) {
                (0, 255)
            } else {
                (-32768, 32767)
            };
            assert_eq!((info.minimum, info.maximum), bounds, "axis {axis} bounds");
        }
    }

    fn assert_key_state(&self, code: u16, pressed: bool) {
        let mut keys = [0_u8; 96];
        let request = libc::_IOR::<[u8; 96]>(b'E' as u32, 0x18);
        // SAFETY: this exact owned evdev descriptor writes at most the encoded
        // 96 bytes into the initialized buffer and retains no pointer.
        ioctl_result(unsafe { libc::ioctl(self.file.as_raw_fd(), request, keys.as_mut_ptr()) })
            .expect("read native keyboard key state");
        let code = usize::from(code);
        assert!(code < keys.len() * 8);
        assert_eq!(keys[code / 8] & (1 << (code % 8)) != 0, pressed);
    }

    fn assert_quiet(&self) {
        assert_eq!(
            self.read_event()
                .expect_err("unexpected native input event")
                .kind(),
            io::ErrorKind::WouldBlock
        );
    }

    fn assert_disconnected(&self) {
        let deadline = Instant::now() + NATIVE_TIMEOUT;
        while self.sysfs_path.exists() || self.event_path.exists() {
            assert!(
                Instant::now() < deadline,
                "owned input device survived teardown"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let error = self
            .read_event()
            .expect_err("destroyed input device cannot deliver input");
        assert_eq!(error.raw_os_error(), Some(libc::ENODEV));
    }
}

impl Drop for NativeEventReader {
    fn drop(&mut self) {
        // SAFETY: release only this fd's grab. Teardown may already have removed
        // the owned device; closing File also releases its grab on every path.
        let _ = unsafe { libc::ioctl(self.file.as_raw_fd(), EVIOCGRAB, 0) };
    }
}

fn ioctl_result(result: libc::c_int) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn nul_terminated_text(buffer: &[u8]) -> io::Result<&str> {
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| io::Error::other("unterminated kernel text"))?;
    std::str::from_utf8(&buffer[..end]).map_err(io::Error::other)
}

fn assert_frame(mut actual: Vec<(u16, u16, i32)>, mut expected: Vec<(u16, u16, i32)>) {
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected, "independent native event expectations");
}

fn full_report(pressed: bool, axis_values: [i32; 6]) -> Vec<(u16, u16, i32)> {
    let mut expected = GAMEPAD_BUTTON_CODES
        .into_iter()
        .map(|code| (EV_KEY, code, i32::from(pressed)))
        .collect::<Vec<_>>();
    expected.extend(
        axis_values
            .into_iter()
            .enumerate()
            .map(|(axis, value)| (EV_ABS, axis as u16, value)),
    );
    expected
}

#[test]
#[ignore = "creates a real uinput controller; requires uinput and owned evdev access; grabs before emitting"]
fn native_uinput_xbox_slot_reports_readback_and_teardown() {
    FAKE_INPUT_ADAPTER.with(|adapter| assert!(adapter.borrow().is_none()));
    // Declare the reader first so panic unwinding destroys the owned controller
    // before releasing its exclusive evdev grab. Creation emits no reports.
    let mut reader = None;
    let mut slots = GamepadSlots::<GrabbedGamepad>::new();
    let state = ControllerSnapshot {
        buttons: ((1 << 22) - 1) & !(1 << 11),
        left_trigger: 73,
        right_trigger: 219,
        left_x: i16::MIN,
        left_y: i16::MAX,
        right_x: 12345,
        right_y: -23456,
    };
    slots
        .apply(0, 1, state, -1, Instant::now(), |number| {
            assert_eq!(number, 0);
            let gamepad = UinputGamepad::create("/dev/uinput", number)?;
            reader = Some(
                NativeEventReader::open(gamepad.device.as_ref().unwrap()).map_err(system_error)?,
            );
            Ok(GrabbedGamepad(gamepad))
        })
        .expect("create and grab owned Xbox-like controller before sending reports");
    assert!(!slots.is_empty());
    let reader = reader
        .as_ref()
        .expect("slot creation opened its exact event node");
    reader.assert_identity("Amaterasu gamepad 0");
    let axes = [-32768, 32767, 73, 12345, -23456, 219];
    let expected = full_report(true, axes);
    assert_frame(reader.read_report(expected.len()), expected);
    reader.assert_kernel_state(true, axes);
    slots
        .apply(
            0,
            1,
            ControllerSnapshot::default(),
            -1,
            Instant::now(),
            |_| panic!("existing slot must reuse the owned native device"),
        )
        .expect("send native neutral report");
    let expected = full_report(false, [0; 6]);
    assert_frame(reader.read_report(expected.len()), expected);
    reader.assert_kernel_state(false, [0; 6]);
    slots
        .apply(
            0,
            1,
            ControllerSnapshot {
                buttons: 1 << 12,
                ..ControllerSnapshot::default()
            },
            -1,
            Instant::now(),
            |_| panic!("existing slot must reuse the owned native device"),
        )
        .expect("leave a held button for slot teardown");
    assert_frame(reader.read_report(1), vec![(EV_KEY, 0x130, 1)]);
    slots
        .stop()
        .expect("release and destroy native controller slot");
    assert!(slots.is_empty());
    reader.assert_disconnected();
    println!(
        "Qualified one grabbed Linux virtual Xbox-like slot: buttons/axes, kernel state, neutral report, and held-slot teardown. Game/client and Windows delivery remain unqualified."
    );
}

fn native_key(source: u8, state: KeyState) -> Event {
    Event::Key {
        key: Key::MoonlightVk {
            code: source,
            normalized: true,
            repeat_modifiers: 0,
            temporary_modifier: false,
        },
        state,
    }
}

fn native_temporary_modifier(source: u8, state: KeyState) -> Event {
    Event::Key {
        key: Key::MoonlightVk {
            code: source,
            normalized: true,
            repeat_modifiers: 0,
            temporary_modifier: true,
        },
        state,
    }
}

#[test]
#[ignore = "creates a real uinput keyboard; requires uinput and owned evdev access; grabs before emitting"]
fn native_uinput_keybinding_targets_keep_source_ownership_and_teardown() {
    FAKE_INPUT_ADAPTER.with(|adapter| assert!(adapter.borrow().is_none()));
    // Backend drops its owned device before reader releases the grab, including
    // panic unwinding. No other keyboard, pointer, or controller is created.
    let reader;
    let mut backend = LinuxBackend::new("/dev/uinput".into());
    let device = Device::create("/dev/uinput", true).expect("create owned keyboard");
    reader = NativeEventReader::open(&device).expect("grab exact keyboard before first report");
    backend.keyboard = Some(device);
    reader.assert_identity("Amaterasu keyboard");
    let custom = KeyBindings::from_flat(vec![
        0x41, 0x43, 0x42, 0x43, 0x43, 0x44, // A/B -> C, C -> D must not chain.
        0x10, 0x44, 0xa0, 0x44, // Built-in generic Shift precedes custom; left is custom.
        0xa5, 0x44, // Custom Right Alt precedes optional Windows alias.
        0, 0x41, 255, 0x41, // Reserved source bytes resolve to supported A.
    ])
    .unwrap();
    backend.set_keybindings(&custom);
    backend
        .inject(&native_key(0x41, KeyState::Down))
        .expect("first source presses mapped C");
    assert_frame(reader.read_report(1), vec![(EV_KEY, 46, 1)]);
    backend
        .inject(&native_key(0x42, KeyState::Down))
        .expect("second source owns the same mapped C");
    reader.assert_quiet();
    reader.assert_key_state(46, true);
    backend.set_keybindings(&KeyBindings::empty());
    backend
        .inject(&native_key(0x42, KeyState::Repeat))
        .expect("repeat retains C after mapping replacement");
    assert_frame(reader.read_report(1), vec![(EV_KEY, 46, 2)]);
    backend
        .inject(&native_key(0x41, KeyState::Up))
        .expect("first source releases only its ownership");
    reader.assert_quiet();
    reader.assert_key_state(46, true);
    backend
        .inject(&native_key(0x42, KeyState::Up))
        .expect("final source releases original C");
    assert_frame(reader.read_report(1), vec![(EV_KEY, 46, 0)]);
    reader.assert_key_state(46, false);

    backend.set_keybindings(&custom);
    backend.set_key_rightalt_to_key_win(true);
    for (source, expected_evdev) in [(0x10, 42), (0xa5, 32)] {
        backend
            .inject(&native_key(source, KeyState::Down))
            .expect("press priority-selected native destination");
        assert_frame(reader.read_report(1), vec![(EV_KEY, expected_evdev, 1)]);
        backend
            .inject(&native_key(source, KeyState::Up))
            .expect("release priority-selected native destination");
        assert_frame(reader.read_report(1), vec![(EV_KEY, expected_evdev, 0)]);
    }
    backend
        .inject(&native_key(0, KeyState::Down))
        .expect("reserved zero source maps to A");
    assert_frame(reader.read_report(1), vec![(EV_KEY, 30, 1)]);
    backend
        .inject(&native_key(255, KeyState::Down))
        .expect("reserved 255 source also owns A");
    reader.assert_quiet();
    backend
        .inject(&native_key(0, KeyState::Up))
        .expect("zero source cannot release 255's ownership");
    reader.assert_quiet();
    reader.assert_key_state(30, true);
    backend
        .inject(&native_key(255, KeyState::Up))
        .expect("255 source finally releases A");
    assert_frame(reader.read_report(1), vec![(EV_KEY, 30, 0)]);
    reader.assert_key_state(30, false);

    for (generic, left, right, left_evdev, right_evdev) in [
        (0x10, 0xa0, 0xa1, 42, 54),
        (0x11, 0xa2, 0xa3, 29, 97),
        (0x12, 0xa4, 0xa5, 56, 100),
    ] {
        backend.set_keybindings(&KeyBindings::from_flat(vec![0x41, right]).unwrap());
        backend
            .inject(&native_key(0x41, KeyState::Down))
            .expect("ordinary source presses mapped right modifier");
        assert_frame(reader.read_report(1), vec![(EV_KEY, right_evdev, 1)]);
        backend.set_keybindings(&KeyBindings::empty());
        backend
            .inject(&native_temporary_modifier(generic, KeyState::Down))
            .expect("temporary chord owns the saved right modifier plan");
        reader.assert_quiet();
        reader.assert_key_state(right_evdev, true);
        reader.assert_key_state(left_evdev, false);

        backend
            .inject(&native_key(left, KeyState::Down))
            .expect("real opposite-side modifier remains a distinct native key");
        assert_frame(reader.read_report(1), vec![(EV_KEY, left_evdev, 1)]);
        reader.assert_key_state(right_evdev, true);
        reader.assert_key_state(left_evdev, true);
        backend
            .inject(&native_key(left, KeyState::Up))
            .expect("real left modifier releases independently");
        assert_frame(reader.read_report(1), vec![(EV_KEY, left_evdev, 0)]);
        reader.assert_key_state(right_evdev, true);
        backend
            .inject(&native_key(0x41, KeyState::Repeat))
            .expect("remapped source repeats its original right modifier");
        assert_frame(reader.read_report(1), vec![(EV_KEY, right_evdev, 2)]);
        backend
            .inject(&native_key(0x41, KeyState::Up))
            .expect("temporary source retains ownership after real source release");
        reader.assert_quiet();
        reader.assert_key_state(right_evdev, true);
        backend
            .inject(&native_temporary_modifier(generic, KeyState::Up))
            .expect("final temporary owner releases the original right modifier");
        assert_frame(reader.read_report(1), vec![(EV_KEY, right_evdev, 0)]);
        reader.assert_key_state(right_evdev, false);
        reader.assert_key_state(left_evdev, false);
    }
    backend.set_keybindings(&custom);
    backend
        .inject(&native_key(0xa0, KeyState::Down))
        .expect("leave a mapped destination held for teardown");
    assert_frame(reader.read_report(1), vec![(EV_KEY, 32, 1)]);
    backend
        .stop()
        .expect("destroy owned keyboard while grabbed");
    reader.assert_disconnected();
    println!(
        "Qualified grabbed Linux keyboard mapping, target collisions, repeat/release across policy replacement, builtin/Right Alt priority, reserved source bytes, temporary Shift/Ctrl/Alt ownership of mapped right modifiers with distinct real left keys, and held-device teardown. Ordinary service-account access, Windows, and game/client delivery remain unqualified."
    );
}
