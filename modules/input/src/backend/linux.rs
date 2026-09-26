use super::Backend;
use super::owned_devices::{acquire_input_pair, recover_input_device, release_input_pair};
use crate::gamepad::{
    ControllerSnapshot, GAMEPAD_ABSOLUTE_AXES, GamepadDevice, GamepadSlots, NativeGamepadEvent,
    controller_state_events, gamepad_key_codes, possible_controller_state_after_write_failure,
    release_owned_device, replace_failed_device,
};
use crate::gamepad_profile::GamepadProfile;
use crate::model::{
    Axis, Button, Event, InputError, InputResult, Key, KeyState, PenTouchDevice,
    effective_virtual_key,
};
mod linux_pen_touch;
use linux_pen_touch::LinuxPenTouch;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};

// Linux UAPI constants from linux/input-event-codes.h and linux/uinput.h.
// libc's _IO/_IOW use the target architecture's ioctl encoding and ABI types.
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_PRESSURE: u16 = 0x18;
const ABS_TILT_X: u16 = 0x1a;
const ABS_TILT_Y: u16 = 0x1b;
const ABS_MT_TOUCH_MAJOR: u16 = 0x30;
const ABS_MT_TOUCH_MINOR: u16 = 0x31;
const ABS_MT_ORIENTATION: u16 = 0x34;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const ABS_MT_PRESSURE: u16 = 0x3a;
const BTN_TOOL_PEN: u16 = 0x140;
const BTN_TOOL_RUBBER: u16 = 0x141;
const BTN_TOUCH: u16 = 0x14a;
const BTN_STYLUS: u16 = 0x14b;
const BTN_STYLUS2: u16 = 0x14c;
const MAX_TOUCH_SLOTS: u8 = 10;
const INPUT_PROP_DIRECT: u32 = 1;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const REL_HWHEEL: u16 = 6;
const REL_WHEEL: u16 = 8;
const REL_HWHEEL_HI_RES: u16 = 12;
const REL_WHEEL_HI_RES: u16 = 11;
const WHEEL_DELTA: i32 = 120;
const UI_SET_EVBIT: libc::Ioctl = libc::_IOW::<libc::c_int>(b'U' as u32, 100);
const UI_SET_KEYBIT: libc::Ioctl = libc::_IOW::<libc::c_int>(b'U' as u32, 101);
const UI_SET_RELBIT: libc::Ioctl = libc::_IOW::<libc::c_int>(b'U' as u32, 102);
const UI_SET_ABSBIT: libc::Ioctl = libc::_IOW::<libc::c_int>(b'U' as u32, 103);
const UI_SET_PROPBIT: libc::Ioctl = libc::_IOW::<libc::c_int>(b'U' as u32, 110);
const UI_DEV_SETUP: libc::Ioctl = libc::_IOW::<libc::uinput_setup>(b'U' as u32, 3);
const UI_ABS_SETUP: libc::Ioctl = libc::_IOW::<libc::uinput_abs_setup>(b'U' as u32, 4);
const UI_DEV_CREATE: libc::Ioctl = libc::_IO(b'U' as u32, 1);
const UI_DEV_DESTROY: libc::Ioctl = libc::_IO(b'U' as u32, 2);

struct Device {
    file: File,
    created: bool,
}
impl Device {
    fn create(path: &str, keyboard: bool) -> InputResult<Self> {
        let file = open_uinput(path)?;
        let fd = file.as_raw_fd();
        set_bit(fd, UI_SET_EVBIT, u32::from(EV_KEY))?;
        if keyboard {
            for code in 1..=248 {
                if code != 240 {
                    set_bit(fd, UI_SET_KEYBIT, code)?;
                }
            }
        } else {
            for code in 0x110..=0x114 {
                set_bit(fd, UI_SET_KEYBIT, code)?;
            }
            set_bit(fd, UI_SET_EVBIT, u32::from(EV_REL))?;
            for code in [
                REL_X,
                REL_Y,
                REL_HWHEEL,
                REL_WHEEL,
                REL_HWHEEL_HI_RES,
                REL_WHEEL_HI_RES,
            ] {
                set_bit(fd, UI_SET_RELBIT, code)?;
            }
        }
        let name: &[u8] = if keyboard {
            b"Amaterasu keyboard"
        } else {
            b"Amaterasu pointer"
        };
        create_device(file, name)
    }

    fn create_pen(path: &str) -> InputResult<Self> {
        let file = open_uinput(path)?;
        let fd = file.as_raw_fd();
        set_bit(fd, UI_SET_EVBIT, u32::from(EV_KEY))?;
        for code in [
            BTN_TOOL_PEN,
            BTN_TOOL_RUBBER,
            BTN_TOUCH,
            BTN_STYLUS,
            BTN_STYLUS2,
        ] {
            set_bit(fd, UI_SET_KEYBIT, u32::from(code))?;
        }
        set_bit(fd, UI_SET_EVBIT, u32::from(EV_ABS))?;
        for (code, minimum, maximum) in [
            (ABS_X, 0, i32::from(u16::MAX)),
            (ABS_Y, 0, i32::from(u16::MAX)),
            (ABS_PRESSURE, 0, i32::from(u16::MAX)),
            (ABS_TILT_X, -90, 90),
            (ABS_TILT_Y, -90, 90),
        ] {
            set_absolute_axis(fd, code, minimum, maximum)?;
        }
        create_device(file, b"Amaterasu pen tablet")
    }

    fn create_touchscreen(path: &str) -> InputResult<Self> {
        let file = open_uinput(path)?;
        let fd = file.as_raw_fd();
        set_bit(fd, UI_SET_EVBIT, u32::from(EV_KEY))?;
        set_bit(fd, UI_SET_KEYBIT, u32::from(BTN_TOUCH))?;
        // INPUT_PROP_DIRECT identifies a touchscreen rather than a touchpad.
        set_bit(fd, UI_SET_PROPBIT, INPUT_PROP_DIRECT)?;
        set_bit(fd, UI_SET_EVBIT, u32::from(EV_ABS))?;
        for (code, minimum, maximum) in [
            (ABS_MT_SLOT, 0, i32::from(MAX_TOUCH_SLOTS - 1)),
            (ABS_MT_TRACKING_ID, 0, i32::MAX),
            (ABS_MT_POSITION_X, 0, i32::from(u16::MAX)),
            (ABS_MT_POSITION_Y, 0, i32::from(u16::MAX)),
            (ABS_MT_TOUCH_MAJOR, 0, i32::from(u16::MAX)),
            (ABS_MT_TOUCH_MINOR, 0, i32::from(u16::MAX)),
            (ABS_MT_ORIENTATION, -90, 90),
            (ABS_MT_PRESSURE, 0, i32::from(u16::MAX)),
        ] {
            set_absolute_axis(fd, code, minimum, maximum)?;
        }
        create_device(file, b"Amaterasu touchscreen")
    }

    fn create_gamepad(path: &str, controller_number: u8) -> InputResult<Self> {
        let file = open_uinput(path)?;
        let fd = file.as_raw_fd();
        set_bit(fd, UI_SET_EVBIT, u32::from(EV_KEY))?;
        for code in gamepad_key_codes() {
            set_bit(fd, UI_SET_KEYBIT, u32::from(code))?;
        }
        set_bit(fd, UI_SET_EVBIT, u32::from(EV_ABS))?;
        for (code, minimum, maximum) in GAMEPAD_ABSOLUTE_AXES {
            set_bit(fd, UI_SET_ABSBIT, u32::from(code))?;
            // SAFETY: uinput_abs_setup and input_absinfo are integer-only Linux
            // UAPI records. Every field is initialized before the kernel reads it.
            let setup = libc::uinput_abs_setup {
                code,
                absinfo: libc::input_absinfo {
                    value: 0,
                    minimum,
                    maximum,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                },
            };
            // SAFETY: fd is a live owned uinput descriptor; the pointer is valid
            // for the exact uinput_abs_setup size encoded in UI_ABS_SETUP.
            check(unsafe { libc::ioctl(fd, UI_ABS_SETUP, &setup) })?;
        }
        let name = format!("Amaterasu gamepad {controller_number}");
        create_device(file, name.as_bytes())
    }
    fn emit(&self, values: &[(u16, u16, i32)]) -> InputResult<()> {
        let mut frame: Vec<libc::input_event> = values
            .iter()
            .map(|(kind, code, value)| {
                // SAFETY: input_event consists entirely of integer timestamp/event
                // fields. Zero timestamps are explicitly permitted by uinput.
                let mut event: libc::input_event = unsafe { std::mem::zeroed() };
                event.type_ = *kind;
                event.code = *code;
                event.value = *value;
                event
            })
            .collect();
        // SAFETY: same integer-only input_event layout; this is SYN_REPORT.
        let mut sync: libc::input_event = unsafe { std::mem::zeroed() };
        sync.type_ = EV_SYN;
        frame.push(sync);
        write_frame(self.file.as_raw_fd(), &frame)
    }

    fn destroy(&mut self) -> InputResult<()> {
        if self.created {
            // SAFETY: this live uinput descriptor owns the virtual device created
            // by UI_DEV_CREATE; a successful ioctl destroys only that device.
            check(unsafe { libc::ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY) })?;
            self.created = false;
        }
        Ok(())
    }
}
impl Drop for Device {
    fn drop(&mut self) {
        // Best effort fallback for keyboard/pointer teardown and unrecoverable
        // object drops. Gamepads explicitly destroy before ownership is cleared.
        let _ = self.destroy();
    }
}

fn open_uinput(path: &str) -> InputResult<File> {
    let file = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(system_error)?;
    if !file
        .metadata()
        .map_err(system_error)?
        .file_type()
        .is_char_device()
    {
        return Err(InputError::new(
            "invalid_device",
            "uinput path must refer to a character device",
        ));
    }
    Ok(file)
}

fn uinput_pen_touch_available(path: &str) -> bool {
    let Ok(file) = open_uinput(path) else {
        return false;
    };
    let fd = file.as_raw_fd();
    // These capability ioctls only configure the probe descriptor. Closing it
    // discards that setup without creating or affecting any input device.
    set_bit(fd, UI_SET_EVBIT, u32::from(EV_ABS)).is_ok()
        && set_bit(fd, UI_SET_PROPBIT, INPUT_PROP_DIRECT).is_ok()
}

fn create_device(file: File, name: &[u8]) -> InputResult<Device> {
    // SAFETY: this Linux ABI packet has integer and byte-array fields; all-zero
    // is valid initialization. No pointer or driver-private payload is stored.
    let mut setup: libc::uinput_setup = unsafe { std::mem::zeroed() };
    setup.id.bustype = 0x06; // BUS_VIRTUAL, not a borrowed hardware USB identity.
    setup.id.version = 1;
    for (target, source) in setup.name.iter_mut().zip(name) {
        *target = *source as libc::c_char;
    }
    let fd = file.as_raw_fd();
    // SAFETY: fd is an owned uinput descriptor and setup remains alive with the
    // exact uinput_setup size encoded into UI_DEV_SETUP.
    check(unsafe { libc::ioctl(fd, UI_DEV_SETUP, &setup) })?;
    // SAFETY: this ioctl takes no pointer argument and creates the configured device.
    check(unsafe { libc::ioctl(fd, UI_DEV_CREATE) })?;
    Ok(Device {
        file,
        created: true,
    })
}

struct UinputGamepad {
    device: Option<Device>,
    path: String,
    controller_number: u8,
    state: ControllerSnapshot,
    recovery_required: bool,
}

impl UinputGamepad {
    fn create(path: &str, controller_number: u8) -> InputResult<Self> {
        Ok(Self {
            device: Some(Device::create_gamepad(path, controller_number)?),
            path: path.to_owned(),
            controller_number,
            state: ControllerSnapshot::default(),
            recovery_required: false,
        })
    }
}

impl GamepadDevice for UinputGamepad {
    fn update(&mut self, state: ControllerSnapshot) -> InputResult<()> {
        if self.recovery_required {
            return Err(InputError::new(
                "controller_unavailable",
                "uinput gamepad must detach after a failed report",
            ));
        }
        let device = self.device.as_ref().ok_or_else(|| {
            InputError::new("controller_unavailable", "uinput gamepad is reacquiring")
        })?;
        let events = controller_state_events(self.state, state);
        if !events.is_empty() {
            // A failed frame write may have reached the kernel partially. Keep
            // every possibly-held value so lease cleanup emits its neutral form.
            self.state = possible_controller_state_after_write_failure(self.state, state);
            self.recovery_required = true;
            let native_events = events
                .iter()
                .map(|event: &NativeGamepadEvent| (event.event_type, event.code, event.value))
                .collect::<Vec<_>>();
            device.emit(&native_events)?;
        }
        self.state = state;
        self.recovery_required = false;
        Ok(())
    }

    fn release(&mut self) -> InputResult<()> {
        let prior_state = self.state;
        let result = release_owned_device(
            &mut self.device,
            self.recovery_required,
            |device| {
                let events = controller_state_events(prior_state, ControllerSnapshot::default());
                self.state = possible_controller_state_after_write_failure(
                    prior_state,
                    ControllerSnapshot::default(),
                );
                if events.is_empty() {
                    return Ok(());
                }
                let native_events = events
                    .iter()
                    .map(|event| (event.event_type, event.code, event.value))
                    .collect::<Vec<_>>();
                device.emit(&native_events)
            },
            Device::destroy,
        );
        if result.is_ok() {
            self.state = ControllerSnapshot::default();
            self.recovery_required = false;
        }
        result
    }

    fn recover_after_update_failure(&mut self, _error: InputError) -> InputResult<()> {
        replace_failed_device(&mut self.device, Device::destroy, || {
            Device::create_gamepad(&self.path, self.controller_number)
        })?;
        self.state = ControllerSnapshot::default();
        self.recovery_required = false;
        Ok(())
    }
}

pub struct LinuxBackend {
    path: String,
    keyboard: Option<Device>,
    pointer: Option<Device>,
    keyboard_recovery_required: bool,
    pointer_recovery_required: bool,
    key_rightalt_to_key_win: bool,
    held_virtual_keycodes: HashMap<u8, u16>,
    vertical_scroll_remainder: i32,
    horizontal_scroll_remainder: i32,
    gamepads: GamepadSlots<UinputGamepad>,
    gamepad_probe: Option<Device>,
    pen_touch: LinuxPenTouch,
    controller_input_supported: bool,
    profile: GamepadProfile,
    effective_profile: Option<&'static str>,
    controller_unavailable_reason: Option<String>,
    back_button_timeout_ms: i32,
}
impl LinuxBackend {
    pub fn new(path: String) -> Self {
        Self {
            path,
            keyboard: None,
            pointer: None,
            keyboard_recovery_required: false,
            pointer_recovery_required: false,
            key_rightalt_to_key_win: false,
            held_virtual_keycodes: HashMap::new(),
            vertical_scroll_remainder: 0,
            horizontal_scroll_remainder: 0,
            gamepads: GamepadSlots::new(),
            gamepad_probe: None,
            pen_touch: LinuxPenTouch::new(),
            controller_input_supported: false,
            profile: GamepadProfile::Auto,
            effective_profile: None,
            controller_unavailable_reason: None,
            back_button_timeout_ms: -1,
        }
    }
}
impl Backend for LinuxBackend {
    fn name(&self) -> &'static str {
        "linux_uinput"
    }
    fn supported(&self) -> bool {
        true
    }
    fn absolute_pointer(&self) -> bool {
        false
    }
    fn pen_touch_supported(&self) -> bool {
        // UInput availability requires a writable character device (commonly
        // granted through a udev rule or membership in the uinput group).
        // An existing device remains usable for its lease even if the node or
        // its permissions disappear after creation. Compositor acceptance and
        // coordinate calibration need platform QA.
        self.pen_touch.has_device() || uinput_pen_touch_available(&self.path)
    }
    fn pen_touch_max_contacts(&self, device: PenTouchDevice) -> Option<usize> {
        Some(match device {
            PenTouchDevice::Pen => 1,
            PenTouchDevice::Touch => usize::from(MAX_TOUCH_SLOTS),
        })
    }
    fn controller_input_supported(&self) -> bool {
        self.controller_input_supported
    }
    fn effective_gamepad_profile(&self) -> Option<&'static str> {
        self.effective_profile
    }
    fn gamepad_unavailable_reason(&self) -> Option<&str> {
        self.controller_unavailable_reason
            .as_deref()
            .or(match self.profile {
                GamepadProfile::Auto | GamepadProfile::Xone => None,
                GamepadProfile::Ds4 => Some("Linux gamepad profile 'ds4' is recognized but not implemented by this backend"),
                GamepadProfile::Ds5 => Some("Linux gamepad profile 'ds5' is recognized but not implemented by this backend"),
                GamepadProfile::Switch => Some("Linux gamepad profile 'switch' is recognized but not implemented by this backend"),
            })
    }
    fn key_encoding(&self) -> &'static str {
        "linux_evdev"
    }
    fn validate(&self, event: &Event) -> InputResult<()> {
        event.validate()?;
        if matches!(event, Event::PenTouch { .. }) && !self.pen_touch_supported() {
            return Err(InputError::new(
                "pen_touch_unavailable",
                "Linux pen/touch requires permission to open the uinput character device",
            ));
        }
        if matches!(event, Event::Controller { .. }) && !self.controller_input_supported {
            return Err(InputError::new(
                "controller_unavailable",
                self.controller_unavailable_reason
                    .as_deref()
                    .unwrap_or("Linux uinput gamepad support was not available for this lease"),
            ));
        }
        if matches!(
            event,
            Event::Absolute { .. }
                | Event::Key {
                    key: Key::WindowsScan { .. },
                    ..
                }
        ) {
            return Err(InputError::new(
                "unsupported_event",
                "Linux requires evdev keys and relative pointer events; compositor-specific absolute mapping is unsupported",
            ));
        }
        if let Event::Key { key, state } = event
            && let Key::MoonlightVk {
                code, normalized, ..
            } = key
            && self
                .event_virtual_keycode(*code, *normalized, *state)
                .is_none()
        {
            return Err(InputError::new(
                "unsupported_event",
                "Linux cannot map this Moonlight virtual key to evdev",
            ));
        }
        Ok(())
    }
    fn start(&mut self) -> InputResult<()> {
        if self.keyboard.is_some()
            || self.pointer.is_some()
            || !self.gamepads.is_empty()
            || self.gamepad_probe.is_some()
            || !self.pen_touch.is_idle()
        {
            return Err(InputError::new(
                "backend_busy",
                "uinput devices are already active",
            ));
        }
        acquire_input_pair(&mut self.keyboard, &mut self.pointer, |keyboard| {
            Device::create(&self.path, keyboard)
        })?;
        self.vertical_scroll_remainder = 0;
        self.horizontal_scroll_remainder = 0;
        self.held_virtual_keycodes.clear();
        self.keyboard_recovery_required = false;
        self.pointer_recovery_required = false;
        self.controller_input_supported = false;
        self.effective_profile = None;
        self.controller_unavailable_reason = None;
        if profile_uses_uinput_backend(self.profile) {
            match Device::create_gamepad(&self.path, 0) {
                Ok(mut probe) => match probe.destroy() {
                    Ok(()) => {
                        self.controller_input_supported = true;
                        self.effective_profile = Some("xone");
                    }
                    Err(error) => {
                        self.controller_unavailable_reason = Some(format!(
                            "Linux uinput gamepad probe device could not be destroyed: {}",
                            error.message
                        ));
                        self.gamepad_probe = Some(probe);
                    }
                },
                Err(error) => {
                    self.controller_unavailable_reason = Some(error.message);
                }
            }
        } else {
            self.controller_unavailable_reason = Some(format!(
                "Linux gamepad profile '{}' is recognized but not implemented by this backend",
                self.profile.as_str()
            ));
        }
        Ok(())
    }
    fn inject(&mut self, event: &Event) -> InputResult<()> {
        if matches!(event, Event::PenTouch { .. }) {
            // Cleanup must still reach an owned device after the uinput path
            // becomes unavailable. The tracker validates event and identity.
            return self.pen_touch.inject(&self.path, event);
        }
        self.validate(event)?;
        if let Event::Controller {
            number,
            active_mask,
            buttons,
            left_trigger,
            right_trigger,
            left_x,
            left_y,
            right_x,
            right_y,
        } = event
        {
            let state = ControllerSnapshot {
                buttons: *buttons,
                left_trigger: *left_trigger,
                right_trigger: *right_trigger,
                left_x: *left_x,
                left_y: *left_y,
                right_x: *right_x,
                right_y: *right_y,
            };
            let (path, gamepads) = (&self.path, &mut self.gamepads);
            return gamepads
                .apply(
                    *number,
                    *active_mask,
                    state,
                    self.back_button_timeout_ms,
                    std::time::Instant::now(),
                    |slot| UinputGamepad::create(path, slot),
                )
                .map_err(|error| InputError::new("controller_unavailable", error.message));
        }
        if let Event::Key { key, state } = event {
            if self.keyboard_recovery_required {
                recover_input_device(
                    &mut self.keyboard,
                    &mut self.keyboard_recovery_required,
                    Device::destroy,
                    || Device::create(&self.path, true),
                )?;
                self.held_virtual_keycodes.clear();
            }
            let code = match key {
                Key::LinuxEvdev { code } => *code,
                Key::MoonlightVk {
                    code, normalized, ..
                } => self
                    .event_virtual_keycode(*code, *normalized, *state)
                    .ok_or_else(|| {
                        InputError::new("unsupported_event", "unmapped Moonlight key")
                    })?,
                Key::WindowsScan { .. } => {
                    return Err(InputError::new(
                        "unsupported_event",
                        "Linux requires evdev codes or normalized Moonlight keys",
                    ));
                }
            };
            let keyboard = self.keyboard.as_ref().ok_or_else(not_started)?;
            let identity = match key {
                Key::MoonlightVk { code, .. } => Some(*code),
                _ => None,
            };
            if let (
                Some(identity),
                Key::MoonlightVk {
                    code, normalized, ..
                },
            ) = (identity, key)
                && *state == KeyState::Down
            {
                // Retain the plan before the write; a failed write can have an
                // uncertain outcome and a later cleanup must use this mapping.
                self.held_virtual_keycodes.insert(
                    identity,
                    code_for_virtual_key(*code, *normalized, self.key_rightalt_to_key_win)?,
                );
            }
            let result = keyboard.emit(&[(
                EV_KEY,
                code,
                match state {
                    KeyState::Up => 0,
                    KeyState::Down => 1,
                    KeyState::Repeat => 2,
                },
            )]);
            if result.is_err() {
                self.keyboard_recovery_required = true;
            }
            result?;
            if let (Some(identity), KeyState::Up) = (identity, state) {
                self.held_virtual_keycodes.remove(&identity);
            }
            return Ok(());
        }
        if self.pointer_recovery_required {
            recover_input_device(
                &mut self.pointer,
                &mut self.pointer_recovery_required,
                Device::destroy,
                || Device::create(&self.path, false),
            )?;
            self.vertical_scroll_remainder = 0;
            self.horizontal_scroll_remainder = 0;
        }
        let pointer = self.pointer.as_ref().ok_or_else(not_started)?;
        let result = match event {
            Event::Button { button, pressed } => pointer.emit(&[(
                EV_KEY,
                match button {
                    Button::Left => 0x110,
                    Button::Right => 0x111,
                    Button::Middle => 0x112,
                    Button::Back => 0x113,
                    Button::Forward => 0x114,
                },
                i32::from(*pressed),
            )]),
            Event::Relative { dx, dy } => {
                pointer.emit(&[(EV_REL, REL_X, *dx), (EV_REL, REL_Y, *dy)])
            }
            Event::Scroll { axis, steps } => pointer.emit(&[(
                EV_REL,
                match axis {
                    Axis::Horizontal => REL_HWHEEL,
                    Axis::Vertical => REL_WHEEL,
                },
                *steps,
            )]),
            Event::ScrollUnits { axis, amount } => self.scroll_units(*axis, *amount),
            _ => {
                return Err(InputError::new(
                    "unsupported_event",
                    "unsupported uinput event",
                ));
            }
        };
        if result.is_err() {
            self.pointer_recovery_required = true;
        }
        result
    }
    fn stop(&mut self) -> InputResult<()> {
        let pen_touch_result = self.pen_touch.stop();
        let gamepad_result = self.gamepads.stop();
        // Keep each descriptor after a failed UI_DEV_DESTROY so a later lease
        // cleanup can retry the same device without reopening /dev/uinput.
        let probe_result =
            release_owned_device(&mut self.gamepad_probe, false, |_| Ok(()), Device::destroy);
        let input_result =
            release_input_pair(&mut self.keyboard, &mut self.pointer, Device::destroy);
        // A failed destroy leaves the descriptor owned by this lease. Keep its
        // recovery and event state until that exact descriptor is destroyed;
        // another stop can then retry without losing the failed-write marker.
        if self.keyboard.is_none() {
            self.held_virtual_keycodes.clear();
            self.keyboard_recovery_required = false;
        }
        if self.pointer.is_none() {
            self.pointer_recovery_required = false;
            self.vertical_scroll_remainder = 0;
            self.horizontal_scroll_remainder = 0;
        }
        self.controller_input_supported = false;
        self.effective_profile = None;
        self.controller_unavailable_reason = None;
        for result in [pen_touch_result, gamepad_result, probe_result, input_result] {
            result?;
        }
        Ok(())
    }

    fn set_key_rightalt_to_key_win(&mut self, enabled: bool) {
        self.key_rightalt_to_key_win = enabled;
    }

    fn advance(&mut self, now: std::time::Instant) -> InputResult<()> {
        // Timer-driven controller writes are retried by GamepadSlots while
        // their effective state remains dirty. A transient write failure must
        // not tear down the keyboard, pointer, or stream lease before retry.
        let _ = self.gamepads.advance(now);
        Ok(())
    }

    fn set_back_button_timeout(&mut self, timeout_ms: i32) {
        self.back_button_timeout_ms = timeout_ms;
        if timeout_ms < 0 {
            self.gamepads.disable_pending_back_timeouts();
        }
    }

    fn set_gamepad_profile(&mut self, profile: GamepadProfile) {
        self.profile = profile;
    }

    fn disable_controller_input(&mut self) -> InputResult<()> {
        self.gamepads.stop()
    }
}

fn profile_uses_uinput_backend(profile: GamepadProfile) -> bool {
    matches!(profile, GamepadProfile::Auto | GamepadProfile::Xone)
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn only_auto_and_xbox_like_uinput_profiles_select_the_linux_backend() {
        assert!(profile_uses_uinput_backend(GamepadProfile::Auto));
        assert!(profile_uses_uinput_backend(GamepadProfile::Xone));
        assert!(!profile_uses_uinput_backend(GamepadProfile::Ds4));
        assert!(!profile_uses_uinput_backend(GamepadProfile::Ds5));
        assert!(!profile_uses_uinput_backend(GamepadProfile::Switch));

        let mut backend = LinuxBackend::new("/dev/uinput".into());
        backend.set_gamepad_profile(GamepadProfile::Ds4);
        assert!(!backend.controller_input_supported());
        assert_eq!(backend.effective_gamepad_profile(), None);
        assert!(
            backend
                .gamepad_unavailable_reason()
                .unwrap()
                .contains("ds4")
        );
    }

    #[test]
    fn pen_touch_capability_is_false_when_uinput_cannot_be_opened() {
        let backend = LinuxBackend::new("/missing/uinput".into());
        assert!(!backend.pen_touch_supported());
    }

    #[test]
    fn pen_touch_contact_limits_match_the_native_uinput_devices() {
        let backend = LinuxBackend::new("/dev/uinput".into());
        assert_eq!(backend.pen_touch_max_contacts(PenTouchDevice::Pen), Some(1));
        assert_eq!(
            backend.pen_touch_max_contacts(PenTouchDevice::Touch),
            Some(usize::from(MAX_TOUCH_SLOTS))
        );
    }

    #[test]
    fn unsupported_event_does_not_attempt_pointer_recovery() {
        let mut backend = LinuxBackend::new("/missing/uinput".into());
        backend.pointer_recovery_required = true;
        let event = Event::Absolute { x: 10, y: 20 };
        assert_eq!(
            backend.inject(&event).unwrap_err().code,
            "unsupported_event"
        );
        assert!(backend.pointer_recovery_required);
        assert!(backend.pointer.is_none());
    }
}

impl LinuxBackend {
    fn event_virtual_keycode(&self, code: u8, normalized: bool, state: KeyState) -> Option<u16> {
        let identity = code;
        if matches!(state, KeyState::Up | KeyState::Repeat)
            && let Some(keycode) = self.held_virtual_keycodes.get(&identity)
        {
            return Some(*keycode);
        }
        code_for_virtual_key(code, normalized, self.key_rightalt_to_key_win)
    }

    fn scroll_units(&mut self, axis: Axis, amount: i16) -> InputResult<()> {
        let Self {
            pointer,
            vertical_scroll_remainder,
            horizontal_scroll_remainder,
            ..
        } = self;
        let pointer = pointer.as_ref().ok_or_else(not_started)?;
        let remainder = match axis {
            Axis::Horizontal => horizontal_scroll_remainder,
            Axis::Vertical => vertical_scroll_remainder,
        };
        emit_scroll_units(axis, amount, remainder, |events| pointer.emit(events))
    }
}

fn code_for_virtual_key(code: u8, _normalized: bool, key_rightalt_to_key_win: bool) -> Option<u16> {
    let host_code = effective_virtual_key(code, key_rightalt_to_key_win);
    amaterasu_gamestream::control::virtual_key_mapping(host_code)
        .map(|mapping| mapping.linux_evdev_code)
}

fn emit_scroll_units(
    axis: Axis,
    amount: i16,
    remainder: &mut i32,
    mut emit: impl FnMut(&[(u16, u16, i32)]) -> InputResult<()>,
) -> InputResult<()> {
    let ((high_resolution_code, high_resolution_amount), legacy, next_remainder) =
        scroll_unit_frame(axis, amount, *remainder);
    let mut events = [
        (EV_REL, high_resolution_code, high_resolution_amount),
        (EV_REL, 0, 0),
    ];
    let event_count = if let Some((legacy_code, legacy_steps)) = legacy {
        events[1] = (EV_REL, legacy_code, legacy_steps);
        2
    } else {
        1
    };
    emit(&events[..event_count])?;
    *remainder = next_remainder;
    Ok(())
}

fn scroll_unit_frame(
    axis: Axis,
    amount: i16,
    remainder: i32,
) -> ((u16, i32), Option<(u16, i32)>, i32) {
    let (high_resolution_code, legacy_code) = match axis {
        Axis::Horizontal => (REL_HWHEEL_HI_RES, REL_HWHEEL),
        Axis::Vertical => (REL_WHEEL_HI_RES, REL_WHEEL),
    };
    let total = remainder + i32::from(amount);
    let steps = total / WHEEL_DELTA;
    let remainder = total % WHEEL_DELTA;
    (
        (high_resolution_code, i32::from(amount)),
        (steps != 0).then_some((legacy_code, steps)),
        remainder,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PenTouchPhase;

    fn touch_event(phase: PenTouchPhase) -> Event {
        Event::PenTouch {
            pointer_id: 7,
            device: PenTouchDevice::Touch,
            phase,
            x: 10,
            y: 20,
            pressure: None,
            contact_area_major: None,
            contact_area_minor: None,
            rotation_degrees: None,
            pen_tool: None,
            pen_buttons: None,
            tilt: None,
        }
    }

    #[test]
    fn unavailable_uinput_path_does_not_block_cleanup_of_a_failed_down() {
        let mut backend = LinuxBackend::new("\0".into());
        let down = touch_event(PenTouchPhase::Down);
        assert!(backend.inject(&down).is_err());
        assert!(!backend.pen_touch.is_idle());
        backend.inject(&touch_event(PenTouchPhase::Cancel)).unwrap();
        assert!(backend.pen_touch.is_idle());
        assert!(!backend.pen_touch_supported());
    }

    #[test]
    fn gamepad_events_require_a_successful_lease_capability_probe() {
        let backend = LinuxBackend::new("/missing/uinput".into());
        let event = Event::Controller {
            number: 0,
            active_mask: 1,
            buttons: 0,
            left_trigger: 0,
            right_trigger: 0,
            left_x: 0,
            left_y: 0,
            right_x: 0,
            right_y: 0,
        };

        assert_eq!(
            backend.validate(&event).unwrap_err().code,
            "unsupported_event"
        );
        assert!(!backend.controller_input_supported());
    }

    #[test]
    fn right_alt_mapping_targets_left_meta_and_preserves_held_keyup_plan() {
        let left_meta = amaterasu_gamestream::control::virtual_key_mapping(0x5b)
            .expect("left Windows key has an evdev mapping")
            .linux_evdev_code;
        let right_alt = amaterasu_gamestream::control::virtual_key_mapping(0xa5)
            .expect("right Alt has an evdev mapping")
            .linux_evdev_code;
        assert_eq!(code_for_virtual_key(0xa5, true, true), Some(left_meta));
        assert_eq!(code_for_virtual_key(0xa5, false, true), Some(left_meta));
        assert_eq!(code_for_virtual_key(0xa5, true, false), Some(right_alt));

        let mut backend = LinuxBackend::new(String::new());
        backend.set_key_rightalt_to_key_win(true);
        let down = backend
            .event_virtual_keycode(0xa5, true, KeyState::Down)
            .unwrap();
        assert_eq!(down, left_meta);
        backend.held_virtual_keycodes.insert(0xa5, down);

        backend.set_key_rightalt_to_key_win(false);
        assert_eq!(
            backend.event_virtual_keycode(0xa5, false, KeyState::Up),
            Some(left_meta),
            "a setting change cannot change the native key selected at key-down"
        );
    }

    #[test]
    fn high_resolution_scroll_maps_raw_units_and_compatible_detents_per_axis() {
        let mut vertical = 0;
        let (hi_res, legacy, vertical) = scroll_unit_frame(Axis::Vertical, 60, vertical);
        assert_eq!(hi_res, (REL_WHEEL_HI_RES, 60));
        assert_eq!(legacy, None);
        assert_eq!(vertical, 60);
        let (hi_res, legacy, vertical) = scroll_unit_frame(Axis::Vertical, 60, vertical);
        assert_eq!(hi_res, (REL_WHEEL_HI_RES, 60));
        assert_eq!(legacy, Some((REL_WHEEL, 1)));
        assert_eq!(vertical, 0);

        let (hi_res, legacy, vertical) = scroll_unit_frame(Axis::Vertical, -60, 0);
        assert_eq!(hi_res, (REL_WHEEL_HI_RES, -60));
        assert_eq!(legacy, None);
        assert_eq!(vertical, -60);
        let (_, legacy, vertical) = scroll_unit_frame(Axis::Vertical, -60, vertical);
        assert_eq!(legacy, Some((REL_WHEEL, -1)));
        assert_eq!(vertical, 0);

        let (hi_res, legacy, horizontal) = scroll_unit_frame(Axis::Horizontal, -120, 0);
        assert_eq!(hi_res, (REL_HWHEEL_HI_RES, -120));
        assert_eq!(legacy, Some((REL_HWHEEL, -1)));
        assert_eq!(horizontal, 0);

        let (hi_res, legacy, horizontal) = scroll_unit_frame(Axis::Horizontal, 60, 0);
        assert_eq!(hi_res, (REL_HWHEEL_HI_RES, 60));
        assert_eq!(legacy, None);
        assert_eq!(horizontal, 60);
        let (_, legacy, horizontal) = scroll_unit_frame(Axis::Horizontal, 60, horizontal);
        assert_eq!(legacy, Some((REL_HWHEEL, 1)));
        assert_eq!(horizontal, 0);
    }

    #[test]
    fn failed_uinput_scroll_write_preserves_fractional_remainder() {
        let mut remainder = 60;
        let result = emit_scroll_units(Axis::Vertical, 60, &mut remainder, |events| {
            assert_eq!(
                events,
                &[(EV_REL, REL_WHEEL_HI_RES, 60), (EV_REL, REL_WHEEL, 1)]
            );
            Err(InputError::new("injection_failed", "test write failure"))
        });
        assert_eq!(result.unwrap_err().code, "injection_failed");
        assert_eq!(remainder, 60);

        emit_scroll_units(Axis::Vertical, 60, &mut remainder, |events| {
            assert_eq!(
                events,
                &[(EV_REL, REL_WHEEL_HI_RES, 60), (EV_REL, REL_WHEEL, 1)]
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(remainder, 0);
    }
}

fn write_frame(fd: RawFd, frame: &[libc::input_event]) -> InputResult<()> {
    let bytes = std::mem::size_of_val(frame);
    let mut written = 0;
    let mut interruptions = 0;
    while written < bytes {
        // SAFETY: frame owns initialized Linux input_event records. The byte
        // offset stays within their contiguous allocation; write retains no pointer.
        let result = unsafe {
            libc::write(
                fd,
                frame.as_ptr().cast::<u8>().add(written).cast(),
                bytes - written,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted && interruptions < 3 {
                interruptions += 1;
                continue;
            }
            return Err(system_error(error));
        }
        if result == 0
            || result as usize > bytes - written
            || !(result as usize).is_multiple_of(std::mem::size_of::<libc::input_event>())
        {
            return Err(InputError::new(
                "injection_failed",
                "uinput returned an incomplete event write",
            ));
        }
        written += result as usize;
    }
    Ok(())
}
fn set_bit(fd: RawFd, request: libc::Ioctl, bit: u32) -> InputResult<()> {
    // SAFETY: capability ioctls take a scalar bit number, not a pointer; fd is live.
    check(unsafe { libc::ioctl(fd, request, bit as libc::c_int) })
}
fn set_absolute_axis(fd: RawFd, code: u16, minimum: i32, maximum: i32) -> InputResult<()> {
    set_bit(fd, UI_SET_ABSBIT, u32::from(code))?;
    // SAFETY: uinput_abs_setup and input_absinfo are integer-only Linux UAPI
    // records, and all fields are initialized before the kernel reads them.
    let setup = libc::uinput_abs_setup {
        code,
        absinfo: libc::input_absinfo {
            value: 0,
            minimum,
            maximum,
            fuzz: 0,
            flat: 0,
            resolution: 0,
        },
    };
    // SAFETY: fd is a live owned uinput descriptor; setup stays valid for the
    // exact uinput_abs_setup size encoded by UI_ABS_SETUP.
    check(unsafe { libc::ioctl(fd, UI_ABS_SETUP, &setup) })
}
fn check(result: libc::c_int) -> InputResult<()> {
    if result < 0 {
        Err(system_error(std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}
fn system_error(error: impl std::fmt::Display) -> InputError {
    InputError::new("injection_failed", format!("uinput: {error}"))
}
fn not_started() -> InputError {
    InputError::new("backend_inactive", "uinput devices are not active")
}

#[cfg(target_arch = "x86_64")]
const _: () = {
    assert!(ABS_MT_TOUCH_MAJOR == 0x30);
    assert!(ABS_MT_TOUCH_MINOR == 0x31);
    assert!(ABS_MT_ORIENTATION == 0x34);
    assert!(BTN_TOOL_RUBBER == 0x141);
    assert!(BTN_STYLUS == 0x14b);
    assert!(BTN_STYLUS2 == 0x14c);
    assert!(std::mem::size_of::<libc::uinput_setup>() == 92);
    assert!(std::mem::size_of::<libc::input_event>() == 24);
    assert!(UI_DEV_SETUP == 0x405c_5503);
    assert!(UI_SET_EVBIT == 0x4004_5564);
    assert!(UI_SET_KEYBIT == 0x4004_5565);
    assert!(UI_SET_RELBIT == 0x4004_5566);
    assert!(UI_SET_ABSBIT == 0x4004_5567);
    assert!(UI_SET_PROPBIT == 0x4004_556e);
    assert!(UI_DEV_CREATE == 0x5501);
    assert!(UI_DEV_DESTROY == 0x5502);
    assert!(std::mem::size_of::<libc::uinput_abs_setup>() == 28);
    assert!(UI_ABS_SETUP == 0x401c_5504);
};
