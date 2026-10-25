use super::Backend;
use super::owned_devices::{acquire_input_pair, recover_input_device, release_input_pair};
use super::owned_keys::{HeldKeyPlans, HostModifier, OwnedKeyPlan};
use crate::feedback::{FeedbackProducer, FeedbackQueue};
use crate::gamepad::{
    ControllerSnapshot, GAMEPAD_ABSOLUTE_AXES, GamepadBatteryState, GamepadDevice,
    GamepadMotionType, GamepadSlots, GamepadTouchPhase, GamepadTouchReport, NativeGamepadEvent,
    controller_state_events, gamepad_key_codes, possible_controller_state_after_write_failure,
    release_owned_device, replace_failed_device,
};
use crate::gamepad_profile::GamepadProfile;
use crate::model::{
    Axis, Button, ControllerBatteryState, ControllerMotionType, ControllerTouchPhase, Event,
    InputError, InputResult, Key, KeyState, PenTouchDevice, is_temporary_modifier,
};
use amaterasu_gamestream::control::{ControllerMotionType as FeedbackMotionType, HostFeedback};
use amaterasu_input_policy::KeyBindings;
#[path = "linux_pen_touch.rs"]
mod linux_pen_touch;
use linux_pen_touch::LinuxPenTouch;
#[path = "linux_ds4.rs"]
mod linux_ds4;
use linux_ds4::Ds4Gamepad;
#[path = "linux_ds5.rs"]
mod linux_ds5;
use linux_ds5::Ds5Gamepad;
#[path = "linux_switch.rs"]
mod linux_switch;
use linux_switch::SwitchGamepad;
#[cfg(test)]
#[path = "linux_native_tests.rs"]
mod native_tests;
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};

// Linux UAPI constants from linux/input-event-codes.h and linux/uinput.h.
// libc's _IO/_IOW use the target architecture's ioctl encoding and ABI types.
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
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
const BTN_SWITCH_Z: u16 = 0x135;
const BTN_SOUTH: u16 = 0x130;
const BTN_EAST: u16 = 0x131;
const BTN_NORTH: u16 = 0x133;
const BTN_WEST: u16 = 0x134;
const BTN_TL: u16 = 0x136;
const BTN_TR: u16 = 0x137;
const BTN_TL2: u16 = 0x138;
const BTN_TR2: u16 = 0x139;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
const BTN_MODE: u16 = 0x13c;
const BTN_THUMBL: u16 = 0x13d;
const BTN_THUMBR: u16 = 0x13e;
const MAX_TOUCH_SLOTS: u8 = 10;
const INPUT_PROP_DIRECT: u32 = 1;
const BUS_USB: u16 = 0x03;
const BUS_VIRTUAL: u16 = 0x06;
const SWITCH_VENDOR_ID: u16 = 0x057e;
const SWITCH_PRODUCT_ID: u16 = 0x2009;
const SWITCH_VERSION: u16 = 0x8111;
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
    #[cfg(test)]
    fake: Option<(u8, std::rc::Rc<std::cell::RefCell<FakeInputAdapter>>)>,
}

#[cfg(test)]
#[derive(Default)]
struct FakeInputAdapter {
    next_id: u8,
    writes: Vec<u8>,
    frames: Vec<Vec<(u16, u16, i32)>>,
    switch_devices: Vec<FakeSwitchDeviceIdentity>,
    destroys: Vec<u8>,
    fail_write: Option<u8>,
    fail_destroy: Option<u8>,
    fail_create: bool,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct FakeSwitchDeviceIdentity {
    id: u8,
    name: Vec<u8>,
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[cfg(test)]
thread_local! {
    static FAKE_INPUT_ADAPTER: std::cell::RefCell<Option<std::rc::Rc<std::cell::RefCell<FakeInputAdapter>>>> =
        const { std::cell::RefCell::new(None) };
}

impl Device {
    #[cfg(test)]
    fn fake_create() -> Option<InputResult<Self>> {
        FAKE_INPUT_ADAPTER
            .with(|slot| slot.borrow().clone())
            .map(|adapter| {
                let id = {
                    let mut state = adapter.borrow_mut();
                    if std::mem::take(&mut state.fail_create) {
                        return Err(InputError::new("acquire_failed", "fake create failure"));
                    }
                    state.next_id += 1;
                    state.next_id
                };
                Ok(Self {
                    file: File::open("/dev/null").map_err(system_error)?,
                    created: true,
                    fake: Some((id, adapter)),
                })
            })
    }

    #[cfg(test)]
    fn fake_switch_create() -> Option<InputResult<Self>> {
        let result = Self::fake_create()?;
        if let Ok(device) = &result
            && let Some((id, adapter)) = &device.fake
        {
            adapter
                .borrow_mut()
                .switch_devices
                .push(FakeSwitchDeviceIdentity {
                    id: *id,
                    name: b"Sunshine Nintendo (virtual) pad".to_vec(),
                    bustype: BUS_USB,
                    vendor: SWITCH_VENDOR_ID,
                    product: SWITCH_PRODUCT_ID,
                    version: SWITCH_VERSION,
                });
        }
        Some(result)
    }

    fn create(path: &str, keyboard: bool) -> InputResult<Self> {
        #[cfg(test)]
        if let Some(device) = Self::fake_create() {
            return device;
        }
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
                set_bit(fd, UI_SET_RELBIT, u32::from(code))?;
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
        // EV_FF stays unadvertised until effects can be forwarded to a client.
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
        #[cfg(test)]
        if let Some(device) = Self::fake_create() {
            return device;
        }
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

    fn create_switch_gamepad(path: &str) -> InputResult<Self> {
        #[cfg(test)]
        if let Some(device) = Self::fake_switch_create() {
            return device;
        }
        let file = open_uinput(path)?;
        let fd = file.as_raw_fd();
        set_bit(fd, UI_SET_EVBIT, u32::from(EV_KEY))?;
        for code in [
            BTN_WEST,
            BTN_EAST,
            BTN_NORTH,
            BTN_SOUTH,
            BTN_THUMBL,
            BTN_THUMBR,
            BTN_TR,
            BTN_TL,
            BTN_TR2,
            BTN_TL2,
            BTN_SWITCH_Z,
            BTN_SELECT,
            BTN_MODE,
            BTN_START,
        ] {
            set_bit(fd, UI_SET_KEYBIT, u32::from(code))?;
        }
        set_bit(fd, UI_SET_EVBIT, u32::from(EV_ABS))?;
        set_absolute_axis(fd, ABS_HAT0X, -1, 1)?;
        set_absolute_axis(fd, ABS_HAT0Y, -1, 1)?;
        for code in [ABS_X, ABS_Y, ABS_RX, ABS_RY] {
            set_absolute_axis_with_calibration(
                fd,
                code,
                i32::from(i16::MIN),
                i32::from(i16::MAX),
                250,
                500,
            )?;
        }
        create_device_with_identity(
            file,
            b"Sunshine Nintendo (virtual) pad",
            UinputIdentity {
                bustype: BUS_USB,
                vendor: SWITCH_VENDOR_ID,
                product: SWITCH_PRODUCT_ID,
                version: SWITCH_VERSION,
            },
        )
    }
    fn emit(&self, values: &[(u16, u16, i32)]) -> InputResult<()> {
        let mut events_written = 0;
        self.emit_with_progress(values, &mut events_written)
    }

    fn emit_with_progress(
        &self,
        values: &[(u16, u16, i32)],
        events_written: &mut usize,
    ) -> InputResult<()> {
        #[cfg(test)]
        if let Some((id, adapter)) = &self.fake {
            let mut state = adapter.borrow_mut();
            state.writes.push(*id);
            state.frames.push(values.to_vec());
            if state.fail_write == Some(*id) {
                state.fail_write = None;
                return Err(InputError::new("injection_failed", "fake partial frame"));
            }
            *events_written = values.len();
            return Ok(());
        }
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
        write_frame(self.file.as_raw_fd(), &frame, events_written)
    }

    fn destroy(&mut self) -> InputResult<()> {
        if self.created {
            #[cfg(test)]
            if let Some((id, adapter)) = &self.fake {
                let mut state = adapter.borrow_mut();
                state.destroys.push(*id);
                if state.fail_destroy == Some(*id) {
                    state.fail_destroy = None;
                    return Err(InputError::new("release_failed", "fake destroy failure"));
                }
                self.created = false;
                return Ok(());
            }
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

const fn modifier_for_evdev(code: u16) -> Option<HostModifier> {
    match code {
        42 | 54 => Some(HostModifier::Shift),
        29 | 97 => Some(HostModifier::Control),
        56 | 100 => Some(HostModifier::Alt),
        _ => None,
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
    create_device_with_identity(
        file,
        name,
        UinputIdentity {
            bustype: BUS_VIRTUAL,
            vendor: 0,
            product: 0,
            version: 1,
        },
    )
}

struct UinputIdentity {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

fn create_device_with_identity(
    file: File,
    name: &[u8],
    identity: UinputIdentity,
) -> InputResult<Device> {
    // SAFETY: this Linux ABI packet has integer and byte-array fields; all-zero
    // is valid initialization. No pointer or driver-private payload is stored.
    let mut setup: libc::uinput_setup = unsafe { std::mem::zeroed() };
    setup.id.bustype = identity.bustype;
    setup.id.vendor = identity.vendor;
    setup.id.product = identity.product;
    setup.id.version = identity.version;
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
        #[cfg(test)]
        fake: None,
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
                // A failed neutral frame may have stopped before SYN_REPORT.
                // If destruction also fails, the next cleanup must detach this
                // descriptor directly instead of completing that partial frame.
                self.recovery_required = true;
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

enum LinuxGamepad {
    Xone(UinputGamepad),
    Ds4(Ds4Gamepad),
    Ds5(Ds5Gamepad),
    Switch(SwitchGamepad),
}

impl LinuxGamepad {
    fn create(
        path: &str,
        slot: u8,
        profile: GamepadProfile,
        back_as_touchpad_click: bool,
        ds5_randomize_mac: bool,
        feedback: FeedbackProducer,
    ) -> InputResult<Self> {
        match profile {
            GamepadProfile::Auto | GamepadProfile::Xone => {
                Ok(Self::Xone(UinputGamepad::create(path, slot)?))
            }
            GamepadProfile::Ds4 => Ok(Self::Ds4(Ds4Gamepad::create(
                slot,
                back_as_touchpad_click,
                feedback.clone(),
            )?)),
            GamepadProfile::Ds5 => Ok(Self::Ds5(Ds5Gamepad::create(
                slot,
                ds5_randomize_mac,
                feedback,
            )?)),
            GamepadProfile::Switch => Ok(Self::Switch(SwitchGamepad::create(path, slot)?)),
        }
    }
}

impl GamepadDevice for LinuxGamepad {
    fn update(&mut self, state: ControllerSnapshot) -> InputResult<()> {
        match self {
            Self::Xone(device) => device.update(state),
            Self::Ds4(device) => device.update(state),
            Self::Ds5(device) => device.update(state),
            Self::Switch(device) => device.update(state),
        }
    }

    fn set_motion(&mut self, motion_type: GamepadMotionType, values: [f32; 3]) -> InputResult<()> {
        match self {
            Self::Xone(device) => device.set_motion(motion_type, values),
            Self::Ds4(device) => device.set_motion(motion_type, values),
            Self::Ds5(device) => device.set_motion(motion_type, values),
            Self::Switch(device) => device.set_motion(motion_type, values),
        }
    }

    fn set_touch(&mut self, touch: GamepadTouchReport) -> InputResult<()> {
        match self {
            Self::Xone(device) => device.set_touch(touch),
            Self::Ds4(device) => device.set_touch(touch),
            Self::Ds5(device) => device.set_touch(touch),
            Self::Switch(device) => device.set_touch(touch),
        }
    }

    fn set_battery(
        &mut self,
        state: GamepadBatteryState,
        percentage: Option<u8>,
    ) -> InputResult<()> {
        match self {
            Self::Xone(device) => device.set_battery(state, percentage),
            Self::Ds4(device) => device.set_battery(state, percentage),
            Self::Ds5(device) => device.set_battery(state, percentage),
            Self::Switch(device) => device.set_battery(state, percentage),
        }
    }

    fn recover_after_update_failure(&mut self, error: InputError) -> InputResult<()> {
        match self {
            Self::Xone(device) => device.recover_after_update_failure(error),
            Self::Ds4(device) => device.recover_after_update_failure(error),
            Self::Ds5(device) => device.recover_after_update_failure(error),
            Self::Switch(device) => device.recover_after_update_failure(error),
        }
    }

    fn set_ds4_back_as_touchpad_click(&mut self, enabled: bool) -> bool {
        match self {
            Self::Xone(device) => device.set_ds4_back_as_touchpad_click(enabled),
            Self::Ds4(device) => device.set_ds4_back_as_touchpad_click(enabled),
            Self::Ds5(_) => false,
            Self::Switch(_) => false,
        }
    }

    fn release(&mut self) -> InputResult<()> {
        match self {
            Self::Xone(device) => device.release(),
            Self::Ds4(device) => device.release(),
            Self::Ds5(device) => device.release(),
            Self::Switch(device) => device.release(),
        }
    }
}

const CONTROLLER_TYPE_UNKNOWN: u8 = 0;
const CONTROLLER_TYPE_XBOX: u8 = 1;
const CONTROLLER_TYPE_PLAYSTATION: u8 = 2;
const CONTROLLER_TYPE_NINTENDO: u8 = 3;
const CONTROLLER_CAP_TOUCHPAD: u32 = 0x0000_0008;
const CONTROLLER_CAP_ACCELEROMETER: u32 = 0x0000_0010;
const CONTROLLER_CAP_GYROSCOPE: u32 = 0x0000_0020;

pub struct LinuxBackend {
    path: String,
    keyboard: Option<Device>,
    pointer: Option<Device>,
    keyboard_recovery_required: bool,
    pointer_recovery_required: bool,
    key_rightalt_to_key_win: bool,
    keybindings: KeyBindings,
    held_virtual_keycodes: HeldKeyPlans<u16>,
    keyboard_retired: bool,
    pointer_retired: bool,
    vertical_scroll_remainder: i32,
    horizontal_scroll_remainder: i32,
    gamepads: GamepadSlots<LinuxGamepad>,
    gamepad_probe: Option<Device>,
    ds4_probe: Option<Ds4Gamepad>,
    ds5_probe: Option<Ds5Gamepad>,
    switch_probe: Option<Device>,
    xone_available: bool,
    ds4_available: bool,
    ds5_available: bool,
    switch_available: bool,
    xone_unavailable_reason: Option<String>,
    ds4_unavailable_reason: Option<String>,
    ds5_unavailable_reason: Option<String>,
    switch_unavailable_reason: Option<String>,
    controller_routes: [Option<GamepadProfile>; 16],
    controller_capabilities: [u32; 16],
    feedback_queue: FeedbackQueue,
    feedback_generation: u64,
    pen_touch: LinuxPenTouch,
    controller_input_supported: bool,
    profile: GamepadProfile,
    effective_profile: Option<&'static str>,
    controller_unavailable_reason: Option<String>,
    back_button_timeout_ms: i32,
    ds4_back_as_touchpad_click: bool,
    motion_as_ds4: bool,
    touchpad_as_ds4: bool,
    ds5_inputtino_randomize_mac: bool,
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
            keybindings: KeyBindings::default(),
            held_virtual_keycodes: HeldKeyPlans::new(),
            keyboard_retired: false,
            pointer_retired: false,
            vertical_scroll_remainder: 0,
            horizontal_scroll_remainder: 0,
            gamepads: GamepadSlots::new(),
            gamepad_probe: None,
            ds4_probe: None,
            ds5_probe: None,
            switch_probe: None,
            xone_available: false,
            ds4_available: false,
            ds5_available: false,
            switch_available: false,
            xone_unavailable_reason: None,
            ds4_unavailable_reason: None,
            ds5_unavailable_reason: None,
            switch_unavailable_reason: None,
            controller_routes: [None; 16],
            controller_capabilities: [0; 16],
            feedback_queue: FeedbackQueue::default(),
            feedback_generation: 0,
            pen_touch: LinuxPenTouch::new(),
            controller_input_supported: false,
            profile: GamepadProfile::Auto,
            effective_profile: None,
            controller_unavailable_reason: None,
            back_button_timeout_ms: -1,
            ds4_back_as_touchpad_click: true,
            motion_as_ds4: true,
            touchpad_as_ds4: true,
            ds5_inputtino_randomize_mac: true,
        }
    }

    fn route_profile_for_arrival(&self, controller_type: u8, capabilities: u32) -> GamepadProfile {
        select_controller_profile(
            self.profile,
            controller_type,
            capabilities,
            self.motion_as_ds4,
            self.touchpad_as_ds4,
        )
    }

    fn profile_for_controller(&self, number: u16) -> GamepadProfile {
        self.controller_routes[usize::from(number)].unwrap_or(match self.profile {
            GamepadProfile::Auto => GamepadProfile::Xone,
            profile => profile,
        })
    }

    fn feedback_producer(&self, number: u16) -> FeedbackProducer {
        self.feedback_queue.producer(
            self.feedback_generation,
            number,
            self.controller_capabilities[usize::from(number)],
        )
    }

    fn activate_motion(&self, number: u16, profile: GamepadProfile) {
        if !matches!(profile, GamepadProfile::Ds4 | GamepadProfile::Ds5) {
            return;
        }
        let capabilities = self.controller_capabilities[usize::from(number)];
        let producer = self.feedback_producer(number);
        for (capability, motion_type) in [
            (CONTROLLER_CAP_ACCELEROMETER, FeedbackMotionType::Accel),
            (CONTROLLER_CAP_GYROSCOPE, FeedbackMotionType::Gyro),
        ] {
            if capabilities & capability != 0 {
                producer.push(HostFeedback::Motion {
                    controller: number,
                    report_rate_hz: 250,
                    motion_type,
                });
            }
        }
    }

    fn profile_available_for_lease(&self, profile: GamepadProfile) -> bool {
        match profile {
            GamepadProfile::Auto => false,
            GamepadProfile::Xone => self.xone_available,
            GamepadProfile::Ds4 => self.ds4_available,
            GamepadProfile::Ds5 => self.ds5_available,
            GamepadProfile::Switch => self.switch_available,
        }
    }

    fn profile_unavailable_error(&self, profile: GamepadProfile) -> InputError {
        let reason = match profile {
            GamepadProfile::Xone => self.xone_unavailable_reason.as_ref(),
            GamepadProfile::Ds4 => self.ds4_unavailable_reason.as_ref(),
            GamepadProfile::Ds5 => self.ds5_unavailable_reason.as_ref(),
            GamepadProfile::Switch => self.switch_unavailable_reason.as_ref(),
            GamepadProfile::Auto => None,
        };
        reason.map_or_else(
            || unavailable_profile(profile),
            |message| InputError::new("controller_unavailable", message.clone()),
        )
    }

    fn refresh_controller_status(&mut self) {
        self.controller_input_supported = match self.profile {
            GamepadProfile::Auto => {
                self.xone_available || self.ds5_available || self.switch_available
            }
            GamepadProfile::Xone => self.xone_available,
            GamepadProfile::Ds4 => self.ds4_available,
            GamepadProfile::Ds5 => self.ds5_available,
            GamepadProfile::Switch => self.switch_available,
        };
        self.effective_profile = match self.profile {
            GamepadProfile::Auto => Some("auto"),
            profile if self.profile_available_for_lease(profile) => Some(profile.as_str()),
            _ => None,
        };
        self.controller_unavailable_reason = if self.controller_input_supported {
            None
        } else {
            match self.profile {
                GamepadProfile::Auto => self
                    .ds5_unavailable_reason
                    .clone()
                    .or_else(|| self.switch_unavailable_reason.clone())
                    .or_else(|| self.xone_unavailable_reason.clone())
                    .or_else(|| {
                        Some("Linux Auto has no available Xone, DS5, or Switch backend".into())
                    }),
                GamepadProfile::Xone => self.xone_unavailable_reason.clone(),
                GamepadProfile::Ds4 => self.ds4_unavailable_reason.clone(),
                GamepadProfile::Ds5 => self.ds5_unavailable_reason.clone(),
                GamepadProfile::Switch => Some(unavailable_profile(GamepadProfile::Switch).message),
            }
        };
    }

    fn require_profile_for_controller(&self, number: u16) -> InputResult<GamepadProfile> {
        let profile = self.profile_for_controller(number);
        if self.profile_available_for_lease(profile) {
            Ok(profile)
        } else {
            Err(self.profile_unavailable_error(profile))
        }
    }

    fn route_controller_arrival(
        &mut self,
        number: u16,
        controller_type: u8,
        capabilities: u32,
    ) -> InputResult<()> {
        let route_index = usize::from(number);
        if route_index >= self.controller_routes.len() {
            return Err(InputError::new(
                "invalid_event",
                "controller number must be in the range 0..16",
            ));
        }
        self.gamepads.remove(number)?;
        let profile = self.route_profile_for_arrival(controller_type, capabilities);
        self.controller_routes[route_index] = Some(profile);
        self.controller_capabilities[route_index] = capabilities;
        if !self.profile_available_for_lease(profile) {
            return Err(self.profile_unavailable_error(profile));
        }
        self.activate_motion(number, profile);

        let back_as_touchpad_click = self.ds4_back_as_touchpad_click;
        let ds5_randomize_mac = self.ds5_inputtino_randomize_mac;
        let feedback = self.feedback_producer(number);
        let path = &self.path;
        self.gamepads.ensure(number, |slot| {
            LinuxGamepad::create(
                path,
                slot,
                profile,
                back_as_touchpad_click,
                ds5_randomize_mac,
                feedback.clone(),
            )
        })
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
    fn supported_gamepad_profiles(&self) -> &'static [&'static str] {
        &["auto", "xone", "ds4", "ds5", "switch"]
    }
    fn gamepad_unavailable_reason(&self) -> Option<&str> {
        self.controller_unavailable_reason.as_deref()
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
        if matches!(
            event,
            Event::Controller { .. }
                | Event::ControllerArrival { .. }
                | Event::ControllerTouch { .. }
                | Event::ControllerMotion { .. }
                | Event::ControllerBattery { .. }
        ) && !self.controller_input_supported
        {
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
            || self.ds4_probe.is_some()
            || self.ds5_probe.is_some()
            || self.switch_probe.is_some()
            || !self.pen_touch.is_idle()
            || self.keyboard_retired
            || self.pointer_retired
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
        self.keyboard_retired = false;
        self.pointer_retired = false;
        self.keyboard_recovery_required = false;
        self.pointer_recovery_required = false;
        self.controller_input_supported = false;
        self.effective_profile = None;
        self.controller_unavailable_reason = None;
        self.xone_available = false;
        self.ds4_available = false;
        self.ds5_available = false;
        self.switch_available = false;
        self.xone_unavailable_reason = None;
        self.ds4_unavailable_reason = None;
        self.ds5_unavailable_reason = None;
        self.switch_unavailable_reason = None;
        self.controller_routes = [None; 16];
        self.controller_capabilities = [0; 16];

        if matches!(self.profile, GamepadProfile::Auto | GamepadProfile::Xone) {
            match Device::create_gamepad(&self.path, 0) {
                Ok(mut probe) => match probe.destroy() {
                    Ok(()) => self.xone_available = true,
                    Err(error) => {
                        self.xone_unavailable_reason = Some(format!(
                            "Linux uinput Xone probe device could not be destroyed: {}",
                            error.message
                        ));
                        self.gamepad_probe = Some(probe);
                    }
                },
                Err(error) => {
                    self.xone_unavailable_reason = Some(error.message);
                }
            }
        }

        if matches!(self.profile, GamepadProfile::Auto | GamepadProfile::Ds5) {
            match Ds5Gamepad::create(
                0,
                self.ds5_inputtino_randomize_mac,
                self.feedback_producer(0),
            ) {
                Ok(mut probe) => match probe.release() {
                    Ok(()) => self.ds5_available = true,
                    Err(error) => {
                        self.ds5_unavailable_reason = Some(format!(
                            "Linux UHID DS5 probe could not be destroyed: {}",
                            error.message
                        ));
                        self.ds5_probe = Some(probe);
                    }
                },
                Err(error) => {
                    self.ds5_unavailable_reason = Some(error.message);
                }
            }
        }

        if self.profile == GamepadProfile::Ds4 {
            match Ds4Gamepad::create(
                0,
                self.ds4_back_as_touchpad_click,
                self.feedback_producer(0),
            ) {
                Ok(mut probe) => match probe.release() {
                    Ok(()) => self.ds4_available = true,
                    Err(error) => {
                        self.ds4_unavailable_reason = Some(format!(
                            "Linux UHID DS4 probe could not be destroyed: {}",
                            error.message
                        ));
                        self.ds4_probe = Some(probe);
                    }
                },
                Err(error) => {
                    self.ds4_unavailable_reason = Some(error.message);
                }
            }
        }

        if matches!(self.profile, GamepadProfile::Auto | GamepadProfile::Switch) {
            match Device::create_switch_gamepad(&self.path) {
                Ok(mut probe) => match probe.destroy() {
                    Ok(()) => self.switch_available = true,
                    Err(error) => {
                        self.switch_unavailable_reason = Some(format!(
                            "Linux uinput Switch probe device could not be destroyed: {}",
                            error.message
                        ));
                        self.switch_probe = Some(probe);
                    }
                },
                Err(error) => {
                    self.switch_unavailable_reason = Some(error.message);
                }
            }
        }

        self.refresh_controller_status();
        Ok(())
    }
    fn inject(&mut self, event: &Event) -> InputResult<()> {
        if matches!(event, Event::PenTouch { .. }) {
            // Cleanup must still reach an owned device after the uinput path
            // becomes unavailable. The tracker validates event and identity.
            return self.pen_touch.inject(&self.path, event);
        }
        self.validate(event)?;
        if let Event::ControllerArrival {
            number,
            controller_type,
            capabilities,
            supported_buttons: _,
        } = event
        {
            return self.route_controller_arrival(*number, *controller_type, *capabilities);
        }
        if let Event::ControllerMotion {
            number,
            motion_type,
            x,
            y,
            z,
        } = event
        {
            let profile = self.require_profile_for_controller(*number)?;
            let motion_type = match motion_type {
                ControllerMotionType::Accel => GamepadMotionType::Accel,
                ControllerMotionType::Gyro => GamepadMotionType::Gyro,
            };
            let back_as_touchpad_click = self.ds4_back_as_touchpad_click;
            let ds5_randomize_mac = self.ds5_inputtino_randomize_mac;
            let feedback = self.feedback_producer(*number);
            let path = &self.path;
            let gamepads = &mut self.gamepads;
            gamepads.ensure(*number, |slot| {
                LinuxGamepad::create(
                    path,
                    slot,
                    profile,
                    back_as_touchpad_click,
                    ds5_randomize_mac,
                    feedback.clone(),
                )
            })?;
            return gamepads.set_motion(*number, motion_type, [*x, *y, *z]);
        }
        if let Event::ControllerTouch {
            number,
            touchpad_index,
            pointer_id,
            phase,
            x,
            y,
            pressure,
        } = event
        {
            let profile = self.require_profile_for_controller(*number)?;
            let phase = match phase {
                ControllerTouchPhase::Hover => GamepadTouchPhase::Hover,
                ControllerTouchPhase::Down => GamepadTouchPhase::Down,
                ControllerTouchPhase::Up => GamepadTouchPhase::Up,
                ControllerTouchPhase::Move => GamepadTouchPhase::Move,
                ControllerTouchPhase::Cancel => GamepadTouchPhase::Cancel,
                ControllerTouchPhase::ButtonOnly => GamepadTouchPhase::ButtonOnly,
                ControllerTouchPhase::HoverLeave => GamepadTouchPhase::HoverLeave,
                ControllerTouchPhase::CancelAll => GamepadTouchPhase::CancelAll,
            };
            let back_as_touchpad_click = self.ds4_back_as_touchpad_click;
            let ds5_randomize_mac = self.ds5_inputtino_randomize_mac;
            let feedback = self.feedback_producer(*number);
            let path = &self.path;
            let gamepads = &mut self.gamepads;
            gamepads.ensure(*number, |slot| {
                LinuxGamepad::create(
                    path,
                    slot,
                    profile,
                    back_as_touchpad_click,
                    ds5_randomize_mac,
                    feedback.clone(),
                )
            })?;
            return gamepads.set_touch(
                *number,
                GamepadTouchReport {
                    touchpad_index: *touchpad_index,
                    pointer_id: *pointer_id,
                    phase,
                    x: *x,
                    y: *y,
                    pressure: *pressure,
                },
            );
        }
        if let Event::ControllerBattery {
            number,
            state,
            percentage,
        } = event
        {
            let profile = self.require_profile_for_controller(*number)?;
            let state = match state {
                ControllerBatteryState::Unknown => GamepadBatteryState::Unknown,
                ControllerBatteryState::NotPresent => GamepadBatteryState::NotPresent,
                ControllerBatteryState::Charging => GamepadBatteryState::Charging,
                ControllerBatteryState::Discharging => GamepadBatteryState::Discharging,
                ControllerBatteryState::NotCharging => GamepadBatteryState::NotCharging,
                ControllerBatteryState::Full => GamepadBatteryState::Full,
            };
            let back_as_touchpad_click = self.ds4_back_as_touchpad_click;
            let ds5_randomize_mac = self.ds5_inputtino_randomize_mac;
            let feedback = self.feedback_producer(*number);
            let path = &self.path;
            let gamepads = &mut self.gamepads;
            gamepads.ensure(*number, |slot| {
                LinuxGamepad::create(
                    path,
                    slot,
                    profile,
                    back_as_touchpad_click,
                    ds5_randomize_mac,
                    feedback.clone(),
                )
            })?;
            return gamepads.set_battery(*number, state, *percentage);
        }
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
            for slot in 0..self.controller_routes.len() {
                if active_mask & (1 << slot) == 0 {
                    self.controller_routes[slot] = None;
                    self.controller_capabilities[slot] = 0;
                }
            }
            let profile = self.profile_for_controller(*number);
            if active_mask & (1 << *number) != 0 && !self.profile_available_for_lease(profile) {
                return Err(self.profile_unavailable_error(profile));
            }
            let state = ControllerSnapshot {
                buttons: *buttons,
                left_trigger: *left_trigger,
                right_trigger: *right_trigger,
                left_x: *left_x,
                left_y: *left_y,
                right_x: *right_x,
                right_y: *right_y,
            };
            let back_as_touchpad_click = self.ds4_back_as_touchpad_click;
            let ds5_randomize_mac = self.ds5_inputtino_randomize_mac;
            let feedback = self.feedback_producer(*number);
            let (path, gamepads) = (&self.path, &mut self.gamepads);
            return gamepads
                .apply(
                    *number,
                    *active_mask,
                    state,
                    self.back_button_timeout_ms,
                    std::time::Instant::now(),
                    |slot| {
                        LinuxGamepad::create(
                            path,
                            slot,
                            profile,
                            back_as_touchpad_click,
                            ds5_randomize_mac,
                            feedback.clone(),
                        )
                    },
                )
                .map_err(|error| {
                    if error.code == "release_failed" {
                        error
                    } else {
                        InputError::new("controller_unavailable", error.message)
                    }
                });
        }
        if let Event::Key { key, state } = event {
            if self.keyboard_retired {
                return if *state == KeyState::Up {
                    Ok(())
                } else {
                    Err(not_started())
                };
            }
            if self.keyboard_recovery_required && *state == KeyState::Up {
                retire_uncertain_input_device(
                    &mut self.keyboard,
                    &mut self.keyboard_recovery_required,
                    &mut self.keyboard_retired,
                )?;
                self.held_virtual_keycodes.clear();
                return Ok(());
            }
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
            let emit = |code| {
                keyboard.emit(&[(
                    EV_KEY,
                    code,
                    match state {
                        KeyState::Up => 0,
                        KeyState::Down => 1,
                        KeyState::Repeat => 2,
                    },
                )])
            };
            let result = if let Some(identity) = identity {
                self.held_virtual_keycodes.apply(
                    identity,
                    *state,
                    OwnedKeyPlan {
                        target: code,
                        native: code,
                        modifier: modifier_for_evdev(code),
                    },
                    is_temporary_modifier(key),
                    emit,
                )
            } else {
                emit(code)
            };
            if result.is_err() {
                self.keyboard_recovery_required = true;
            }
            result?;
            return Ok(());
        }
        if self.pointer_retired {
            return if matches!(event, Event::Button { pressed: false, .. }) {
                Ok(())
            } else {
                Err(not_started())
            };
        }
        if self.pointer_recovery_required && matches!(event, Event::Button { pressed: false, .. }) {
            retire_uncertain_input_device(
                &mut self.pointer,
                &mut self.pointer_recovery_required,
                &mut self.pointer_retired,
            )?;
            self.vertical_scroll_remainder = 0;
            self.horizontal_scroll_remainder = 0;
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
        // Keep each failed probe detach owned so cleanup can retry that same
        // descriptor instead of silently leaking it.
        let probe_result =
            release_owned_device(&mut self.gamepad_probe, false, |_| Ok(()), Device::destroy);
        let ds4_probe_result = self
            .ds4_probe
            .as_mut()
            .map(Ds4Gamepad::release)
            .unwrap_or(Ok(()));
        if ds4_probe_result.is_ok() {
            self.ds4_probe = None;
        }
        let ds5_probe_result = self
            .ds5_probe
            .as_mut()
            .map(Ds5Gamepad::release)
            .unwrap_or(Ok(()));
        if ds5_probe_result.is_ok() {
            self.ds5_probe = None;
        }
        let switch_probe_result =
            release_owned_device(&mut self.switch_probe, false, |_| Ok(()), Device::destroy);
        // A failed native destroy leaves the exact descriptor owned by this
        // lease for a later stop call to retry.
        let input_result =
            release_input_pair(&mut self.keyboard, &mut self.pointer, Device::destroy);
        if self.keyboard.is_none() {
            self.held_virtual_keycodes.clear();
            self.keyboard_recovery_required = false;
            self.keyboard_retired = false;
        }
        if self.pointer.is_none() {
            self.pointer_recovery_required = false;
            self.pointer_retired = false;
            self.vertical_scroll_remainder = 0;
            self.horizontal_scroll_remainder = 0;
        }
        self.controller_input_supported = false;
        self.effective_profile = None;
        self.controller_unavailable_reason = None;
        self.xone_available = false;
        self.ds4_available = false;
        self.ds5_available = false;
        self.switch_available = false;
        if ds4_probe_result.is_ok() {
            self.ds4_unavailable_reason = None;
        }
        if ds5_probe_result.is_err() {
            self.ds5_unavailable_reason = ds5_probe_result
                .as_ref()
                .err()
                .map(|error| error.message.clone());
        } else {
            self.ds5_unavailable_reason = None;
        }
        if let Err(error) = &switch_probe_result {
            self.switch_unavailable_reason = Some(error.message.clone());
        } else {
            self.switch_unavailable_reason = None;
        }
        if self.gamepads.is_empty()
            && self.ds4_probe.is_none()
            && self.ds5_probe.is_none()
            && self.switch_probe.is_none()
            && self.gamepad_probe.is_none()
        {
            self.controller_routes = [None; 16];
            self.controller_capabilities = [0; 16];
        }
        for result in [
            pen_touch_result,
            gamepad_result,
            probe_result,
            ds4_probe_result,
            ds5_probe_result,
            switch_probe_result,
            input_result,
        ] {
            result?;
        }
        Ok(())
    }

    fn set_key_rightalt_to_key_win(&mut self, enabled: bool) {
        self.key_rightalt_to_key_win = enabled;
    }

    fn set_keybindings(&mut self, keybindings: &KeyBindings) {
        self.keybindings = keybindings.clone();
    }

    fn advance(&mut self, now: std::time::Instant) -> InputResult<()> {
        // A failed probe teardown keeps the original device owned until the
        // retry succeeds; only then is that profile reported available.
        if let Some(probe) = self.gamepad_probe.as_mut()
            && probe.destroy().is_ok()
        {
            self.gamepad_probe = None;
            self.xone_available = true;
            self.xone_unavailable_reason = None;
        }
        if let Some(probe) = self.ds4_probe.as_mut()
            && probe.release().is_ok()
        {
            self.ds4_probe = None;
            self.ds4_available = true;
            self.ds4_unavailable_reason = None;
        }
        if let Some(probe) = self.ds5_probe.as_mut()
            && probe.release().is_ok()
        {
            self.ds5_probe = None;
            self.ds5_available = true;
            self.ds5_unavailable_reason = None;
        }
        if let Some(probe) = self.switch_probe.as_mut()
            && probe.destroy().is_ok()
        {
            self.switch_probe = None;
            self.switch_available = true;
            self.switch_unavailable_reason = None;
        }
        self.refresh_controller_status();
        // Timer-driven controller reports are retried while their current
        // state stays dirty, without tearing down unrelated input devices.
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

    fn set_ds4_back_as_touchpad_click(&mut self, enabled: bool) {
        self.ds4_back_as_touchpad_click = enabled;
        self.gamepads.set_ds4_back_as_touchpad_click(enabled);
    }

    fn set_motion_as_ds4(&mut self, enabled: bool) {
        self.motion_as_ds4 = enabled;
    }

    fn set_touchpad_as_ds4(&mut self, enabled: bool) {
        self.touchpad_as_ds4 = enabled;
    }

    fn set_ds5_inputtino_randomize_mac(&mut self, enabled: bool) {
        self.ds5_inputtino_randomize_mac = enabled;
    }

    fn set_feedback_queue(&mut self, queue: FeedbackQueue) {
        self.feedback_queue = queue;
    }

    fn set_feedback_generation(&mut self, generation: u64) {
        self.feedback_generation = generation;
    }

    fn disable_controller_input(&mut self) -> InputResult<()> {
        let result = self.gamepads.stop();
        if result.is_ok() {
            self.controller_routes = [None; 16];
            self.controller_capabilities = [0; 16];
        }
        result
    }
}

fn retire_uncertain_input_device(
    device: &mut Option<Device>,
    recovery_required: &mut bool,
    retired: &mut bool,
) -> InputResult<()> {
    if let Some(owned) = device.as_mut() {
        owned.destroy()?;
        *device = None;
    }
    *recovery_required = false;
    *retired = true;
    Ok(())
}

#[cfg(test)]
fn profile_uses_uinput_backend(profile: GamepadProfile) -> bool {
    matches!(
        profile,
        GamepadProfile::Auto | GamepadProfile::Xone | GamepadProfile::Switch
    )
}

fn select_controller_profile(
    requested: GamepadProfile,
    controller_type: u8,
    capabilities: u32,
    motion_as_ds4: bool,
    touchpad_as_ds4: bool,
) -> GamepadProfile {
    if requested != GamepadProfile::Auto {
        return requested;
    }
    match controller_type {
        CONTROLLER_TYPE_XBOX => GamepadProfile::Xone,
        CONTROLLER_TYPE_PLAYSTATION => GamepadProfile::Ds5,
        CONTROLLER_TYPE_NINTENDO => GamepadProfile::Switch,
        CONTROLLER_TYPE_UNKNOWN => {
            if (motion_as_ds4
                && capabilities & (CONTROLLER_CAP_ACCELEROMETER | CONTROLLER_CAP_GYROSCOPE) != 0)
                || (touchpad_as_ds4 && capabilities & CONTROLLER_CAP_TOUCHPAD != 0)
            {
                GamepadProfile::Ds5
            } else {
                GamepadProfile::Xone
            }
        }
        _ => GamepadProfile::Xone,
    }
}

fn unavailable_profile(profile: GamepadProfile) -> InputError {
    InputError::new(
        "controller_unavailable",
        format!(
            "Linux gamepad profile '{}' is unavailable for this input lease",
            profile.as_str()
        ),
    )
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    const ABS_Z: u16 = 0x02;
    const ABS_RZ: u16 = 0x05;

    struct FakeInputGuard;

    impl FakeInputGuard {
        fn install() -> std::rc::Rc<std::cell::RefCell<FakeInputAdapter>> {
            let adapter = std::rc::Rc::new(std::cell::RefCell::new(FakeInputAdapter::default()));
            FAKE_INPUT_ADAPTER.with(|slot| {
                assert!(slot.borrow_mut().replace(adapter.clone()).is_none());
            });
            adapter
        }
    }

    impl Drop for FakeInputGuard {
        fn drop(&mut self) {
            FAKE_INPUT_ADAPTER.with(|slot| {
                slot.borrow_mut().take();
            });
        }
    }

    #[test]
    fn xone_and_switch_use_uinput_while_sony_profiles_use_native_hid() {
        assert!(profile_uses_uinput_backend(GamepadProfile::Auto));
        assert!(profile_uses_uinput_backend(GamepadProfile::Xone));
        assert!(!profile_uses_uinput_backend(GamepadProfile::Ds4));
        assert!(!profile_uses_uinput_backend(GamepadProfile::Ds5));
        assert!(profile_uses_uinput_backend(GamepadProfile::Switch));
        assert!(GamepadProfile::implemented_profiles().contains(&"ds4"));
    }

    #[test]
    fn automatic_selector_preserves_pinned_device_type_and_capability_precedence() {
        let auto = GamepadProfile::Auto;
        assert_eq!(
            select_controller_profile(
                auto,
                CONTROLLER_TYPE_XBOX,
                CONTROLLER_CAP_ACCELEROMETER | CONTROLLER_CAP_TOUCHPAD,
                true,
                true,
            ),
            GamepadProfile::Xone
        );
        assert_eq!(
            select_controller_profile(auto, CONTROLLER_TYPE_PLAYSTATION, 0, false, false),
            GamepadProfile::Ds5
        );
        assert_eq!(
            select_controller_profile(auto, CONTROLLER_TYPE_NINTENDO, 0, true, true),
            GamepadProfile::Switch
        );
        assert_eq!(
            select_controller_profile(
                auto,
                CONTROLLER_TYPE_UNKNOWN,
                CONTROLLER_CAP_GYROSCOPE | CONTROLLER_CAP_TOUCHPAD,
                true,
                true,
            ),
            GamepadProfile::Ds5
        );
        assert_eq!(
            select_controller_profile(
                auto,
                CONTROLLER_TYPE_UNKNOWN,
                CONTROLLER_CAP_TOUCHPAD,
                false,
                true,
            ),
            GamepadProfile::Ds5
        );
        assert_eq!(
            select_controller_profile(auto, CONTROLLER_TYPE_UNKNOWN, 0, true, true),
            GamepadProfile::Xone
        );
        assert_eq!(
            select_controller_profile(
                GamepadProfile::Ds4,
                CONTROLLER_TYPE_PLAYSTATION,
                CONTROLLER_CAP_GYROSCOPE,
                true,
                true,
            ),
            GamepadProfile::Ds4
        );
    }

    #[test]
    fn explicit_ds4_probe_and_slot_route_use_synthetic_uhid_without_hardware() {
        let _input_guard = FakeInputGuard;
        let _input_adapter = FakeInputGuard::install();
        let (uhid_guard, uhid_adapter) = linux_ds4::FakeUhidGuard::install();
        let mut backend = LinuxBackend::new("/dev/uinput".into());
        backend.set_gamepad_profile(GamepadProfile::Ds4);
        backend.start().unwrap();
        assert!(backend.controller_input_supported());
        assert_eq!(backend.effective_gamepad_profile(), Some("ds4"));
        assert!(backend.gamepad_unavailable_reason().is_none());

        backend
            .inject(&Event::ControllerArrival {
                number: 2,
                controller_type: CONTROLLER_TYPE_PLAYSTATION,
                capabilities: CONTROLLER_CAP_TOUCHPAD
                    | CONTROLLER_CAP_ACCELEROMETER
                    | CONTROLLER_CAP_GYROSCOPE,
                supported_buttons: u32::MAX,
            })
            .unwrap();
        backend
            .inject(&Event::ControllerTouch {
                number: 2,
                touchpad_index: 0,
                pointer_id: 0,
                phase: ControllerTouchPhase::Down,
                x: 0.5,
                y: 0.25,
                pressure: 1.0,
            })
            .unwrap();
        backend
            .inject(&Event::ControllerMotion {
                number: 2,
                motion_type: ControllerMotionType::Accel,
                x: 0.0,
                y: 9.80665,
                z: 0.0,
            })
            .unwrap();
        let unsupported_touch = Event::ControllerTouch {
            number: 2,
            touchpad_index: 1,
            pointer_id: 0,
            phase: ControllerTouchPhase::Down,
            x: 0.5,
            y: 0.25,
            pressure: 1.0,
        };
        assert_eq!(
            backend.inject(&unsupported_touch).unwrap_err().code,
            "unsupported_event"
        );
        assert_eq!(uhid_adapter.borrow().created.len(), 2);
        backend
            .inject(&Event::ControllerBattery {
                number: 2,
                state: ControllerBatteryState::Discharging,
                percentage: Some(69),
            })
            .unwrap();
        backend
            .inject(&Event::Controller {
                number: 2,
                active_mask: 1 << 2,
                buttons: 1 << 5,
                left_trigger: 0,
                right_trigger: 0,
                left_x: 0,
                left_y: 0,
                right_x: 0,
                right_y: 0,
            })
            .unwrap();

        let active_id = uhid_adapter.borrow().next_id;
        let reports = uhid_adapter.borrow().reports.clone();
        assert!(reports.iter().any(|(id, report)| {
            *id == active_id && report[7] & 0x02 != 0 && report[6] & 0x10 != 0
        }));
        assert!(
            reports
                .iter()
                .any(|(id, report)| { *id == active_id && report[35..39] == [1, 192, 0xb3, 14] })
        );
        assert!(
            reports
                .iter()
                .any(|(id, report)| { *id == active_id && report[19..25] == [0, 0, 0, 32, 0, 0] })
        );
        assert!(
            reports
                .iter()
                .any(|(id, report)| { *id == active_id && report[30] == 6 })
        );

        backend.stop().unwrap();
        assert!(uhid_adapter.borrow().destroyed.contains(&active_id));
        drop(uhid_guard);
    }

    #[test]
    fn explicit_ds5_probe_and_slot_route_publish_reports_and_release_cleanly() {
        let _input_guard = FakeInputGuard;
        let _input_adapter = FakeInputGuard::install();
        let (uhid_guard, uhid_adapter) = linux_ds5::FakeUhidGuard::install();
        let mut backend = LinuxBackend::new("/dev/uinput".into());
        backend.set_gamepad_profile(GamepadProfile::Ds5);
        backend.set_ds5_inputtino_randomize_mac(false);
        backend.start().unwrap();
        assert!(backend.controller_input_supported());
        assert_eq!(backend.effective_gamepad_profile(), Some("ds5"));
        assert!(backend.gamepad_unavailable_reason().is_none());

        backend
            .inject(&Event::ControllerArrival {
                number: 2,
                controller_type: CONTROLLER_TYPE_PLAYSTATION,
                capabilities: CONTROLLER_CAP_TOUCHPAD
                    | CONTROLLER_CAP_ACCELEROMETER
                    | CONTROLLER_CAP_GYROSCOPE,
                supported_buttons: u32::MAX,
            })
            .unwrap();
        backend
            .inject(&Event::ControllerTouch {
                number: 2,
                touchpad_index: 0,
                pointer_id: 0,
                phase: ControllerTouchPhase::Down,
                x: 0.5,
                y: 0.25,
                pressure: 1.0,
            })
            .unwrap();
        backend
            .inject(&Event::ControllerMotion {
                number: 2,
                motion_type: ControllerMotionType::Accel,
                x: 0.0,
                y: 9.80665,
                z: 0.0,
            })
            .unwrap();
        backend
            .inject(&Event::ControllerMotion {
                number: 2,
                motion_type: ControllerMotionType::Gyro,
                x: 0.0,
                y: 0.0,
                z: 90.0,
            })
            .unwrap();
        backend
            .inject(&Event::ControllerBattery {
                number: 2,
                state: ControllerBatteryState::Discharging,
                percentage: Some(69),
            })
            .unwrap();
        backend
            .inject(&Event::Controller {
                number: 2,
                active_mask: 1 << 2,
                buttons: (1 << 12) | (1 << 13) | (1 << 8),
                left_trigger: 10,
                right_trigger: 20,
                left_x: i16::MIN,
                left_y: i16::MAX,
                right_x: 0,
                right_y: 0,
            })
            .unwrap();

        let adapter = uhid_adapter.borrow();
        assert_eq!(adapter.created.len(), 2);
        assert_eq!(adapter.created[1].0, 2);
        assert_eq!(&adapter.created[1].1[132..149], b"02:00:00:00:00:02");
        let active_id = adapter.reports.last().unwrap().0;
        assert!(
            adapter.reports.iter().any(|(id, report)| {
                *id == active_id && report[1..7] == [0, 0, 128, 128, 10, 20]
            })
        );
        assert!(adapter.reports.iter().any(|(id, report)| {
            *id == active_id && report[16..22] == [0, 0, 0, 0, 0xa0, 0x05]
        }));
        assert!(
            adapter
                .reports
                .iter()
                .any(|(id, report)| { *id == active_id && report[22..28] == [0, 0, 0, 32, 0, 0] })
        );
        assert!(adapter.reports.iter().any(|(id, report)| {
            *id == active_id && report[33..37] == [1, 0xc0, 0xe3, 0x10] && report[53] == 0x06
        }));
        drop(adapter);

        backend.stop().unwrap();
        assert!(uhid_adapter.borrow().destroyed.contains(&1));
        assert!(uhid_adapter.borrow().destroyed.contains(&2));
        drop(uhid_guard);
    }

    #[test]
    fn arrival_profile_change_detaches_xone_before_selecting_ds5() {
        let _input_guard = FakeInputGuard;
        let input_adapter = FakeInputGuard::install();
        let (_uhid_guard, uhid_adapter) = linux_ds5::FakeUhidGuard::install();
        let mut backend = LinuxBackend::new("/dev/uinput".into());
        backend.start().unwrap();

        backend
            .inject(&Event::ControllerArrival {
                number: 0,
                controller_type: CONTROLLER_TYPE_XBOX,
                capabilities: 0,
                supported_buttons: u32::MAX,
            })
            .unwrap();
        assert_eq!(backend.controller_routes[0], Some(GamepadProfile::Xone));
        assert_eq!(input_adapter.borrow().next_id, 5);

        backend
            .inject(&Event::ControllerArrival {
                number: 0,
                controller_type: CONTROLLER_TYPE_PLAYSTATION,
                capabilities: CONTROLLER_CAP_TOUCHPAD,
                supported_buttons: u32::MAX,
            })
            .unwrap();
        assert_eq!(backend.controller_routes[0], Some(GamepadProfile::Ds5));
        assert!(input_adapter.borrow().destroys.contains(&5));
        assert_eq!(input_adapter.borrow().next_id, 5);
        assert_eq!(uhid_adapter.borrow().created.len(), 2);

        backend.stop().unwrap();
        assert!(uhid_adapter.borrow().destroyed.contains(&2));
    }

    #[test]
    fn auto_playstation_arrival_uses_ds5_and_reports_auto_as_effective_profile() {
        let _input_guard = FakeInputGuard;
        let input_adapter = FakeInputGuard::install();
        let (_uhid_guard, uhid_adapter) = linux_ds5::FakeUhidGuard::install();
        let mut backend = LinuxBackend::new("/dev/uinput".into());
        backend.start().unwrap();
        assert!(backend.controller_input_supported());
        assert_eq!(backend.effective_gamepad_profile(), Some("auto"));
        backend
            .inject(&Event::ControllerArrival {
                number: 0,
                controller_type: CONTROLLER_TYPE_PLAYSTATION,
                capabilities: CONTROLLER_CAP_TOUCHPAD,
                supported_buttons: u32::MAX,
            })
            .unwrap();
        assert_eq!(backend.controller_routes[0], Some(GamepadProfile::Ds5));
        backend
            .inject(&Event::Controller {
                number: 0,
                active_mask: 1,
                buttons: 0,
                left_trigger: 0,
                right_trigger: 0,
                left_x: 0,
                left_y: 0,
                right_x: 0,
                right_y: 0,
            })
            .unwrap();
        assert_eq!(input_adapter.borrow().next_id, 4);
        assert_eq!(uhid_adapter.borrow().created.len(), 2);
        backend.stop().unwrap();
        assert!(uhid_adapter.borrow().destroyed.contains(&2));
    }

    #[test]
    fn explicit_switch_probe_and_slot_route_use_synthetic_uinput() {
        let _input_guard = FakeInputGuard;
        let input_adapter = FakeInputGuard::install();
        let mut backend = LinuxBackend::new("/dev/uinput".into());
        backend.set_gamepad_profile(GamepadProfile::Switch);
        backend.start().unwrap();
        assert!(backend.controller_input_supported());
        assert_eq!(backend.effective_gamepad_profile(), Some("switch"));
        assert!(backend.supported_gamepad_profiles().contains(&"switch"));
        assert_eq!(input_adapter.borrow().destroys, [3]);

        backend
            .inject(&Event::ControllerArrival {
                number: 2,
                controller_type: CONTROLLER_TYPE_NINTENDO,
                capabilities: 0,
                supported_buttons: u32::MAX,
            })
            .unwrap();
        assert_eq!(backend.controller_routes[2], Some(GamepadProfile::Switch));
        assert_eq!(input_adapter.borrow().switch_devices.len(), 2);

        for event in [
            Event::ControllerMotion {
                number: 2,
                motion_type: ControllerMotionType::Gyro,
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
            Event::ControllerTouch {
                number: 2,
                touchpad_index: 0,
                pointer_id: 0,
                phase: ControllerTouchPhase::Down,
                x: 0.5,
                y: 0.5,
                pressure: 1.0,
            },
            Event::ControllerBattery {
                number: 2,
                state: ControllerBatteryState::Discharging,
                percentage: Some(50),
            },
        ] {
            assert_eq!(
                backend.inject(&event).unwrap_err().code,
                "unsupported_event"
            );
        }

        backend
            .inject(&Event::Controller {
                number: 2,
                active_mask: 1 << 2,
                buttons: (1 << 12) | (1 << 13) | (1 << 21),
                left_trigger: 1,
                right_trigger: 0,
                left_x: -1234,
                left_y: 2345,
                right_x: i16::MAX,
                right_y: i16::MIN,
            })
            .unwrap();
        let adapter = input_adapter.borrow();
        assert_eq!(adapter.switch_devices[0].id, 3);
        assert_eq!(adapter.switch_devices[1].id, 4);
        for identity in &adapter.switch_devices {
            assert_eq!(identity.name, b"Sunshine Nintendo (virtual) pad");
            assert_eq!(identity.bustype, BUS_USB);
            assert_eq!(identity.vendor, SWITCH_VENDOR_ID);
            assert_eq!(identity.product, SWITCH_PRODUCT_ID);
            assert_eq!(identity.version, SWITCH_VERSION);
        }
        assert!(adapter.frames.iter().any(|frame| {
            frame.contains(&(EV_KEY, BTN_EAST, 1))
                && frame.contains(&(EV_KEY, BTN_SOUTH, 1))
                && frame.contains(&(EV_KEY, BTN_SWITCH_Z, 1))
                && frame.contains(&(EV_KEY, BTN_TL2, 1))
                && frame.contains(&(EV_ABS, ABS_X, -1234))
                && frame.contains(&(EV_ABS, ABS_Y, -2345))
                && frame.contains(&(EV_ABS, ABS_RX, i32::from(i16::MAX)))
                && frame.contains(&(EV_ABS, ABS_RY, -i32::from(i16::MIN)))
        }));
        assert!(
            !adapter
                .frames
                .iter()
                .flatten()
                .any(|(kind, code, _)| { *kind == EV_ABS && (*code == ABS_Z || *code == ABS_RZ) })
        );
        drop(adapter);
        backend.stop().unwrap();
        assert_eq!(input_adapter.borrow().destroys, [3, 4, 1, 2]);
    }

    #[test]
    fn auto_nintendo_arrival_uses_switch_without_falling_back_to_xone_or_ds5() {
        let _input_guard = FakeInputGuard;
        let input_adapter = FakeInputGuard::install();
        let (_uhid_guard, uhid_adapter) = linux_ds5::FakeUhidGuard::install();
        let mut backend = LinuxBackend::new("/dev/uinput".into());
        backend.start().unwrap();
        assert!(backend.controller_input_supported());
        assert_eq!(backend.effective_gamepad_profile(), Some("auto"));

        backend
            .inject(&Event::ControllerArrival {
                number: 0,
                controller_type: CONTROLLER_TYPE_NINTENDO,
                capabilities: 0,
                supported_buttons: u32::MAX,
            })
            .unwrap();
        assert_eq!(backend.controller_routes[0], Some(GamepadProfile::Switch));
        backend
            .inject(&Event::Controller {
                number: 0,
                active_mask: 1,
                buttons: 1 << 12,
                left_trigger: 0,
                right_trigger: 0,
                left_x: 0,
                left_y: 0,
                right_x: 0,
                right_y: 0,
            })
            .unwrap();
        let adapter = input_adapter.borrow();
        assert_eq!(adapter.switch_devices.len(), 2);
        assert_eq!(adapter.switch_devices[1].id, 5);
        assert!(
            adapter
                .frames
                .iter()
                .any(|frame| { frame.contains(&(EV_KEY, BTN_EAST, 1)) })
        );
        drop(adapter);
        assert_eq!(uhid_adapter.borrow().created.len(), 1);
        assert_eq!(uhid_adapter.borrow().destroyed.len(), 1);
        backend.stop().unwrap();
        assert!(input_adapter.borrow().destroys.contains(&5));
        assert_eq!(uhid_adapter.borrow().destroyed.len(), 1);
    }

    #[test]
    fn auto_nintendo_stays_unavailable_until_its_failed_switch_probe_is_destroyed() {
        let _input_guard = FakeInputGuard;
        let input_adapter = FakeInputGuard::install();
        let (_uhid_guard, _uhid_adapter) = linux_ds5::FakeUhidGuard::install();
        input_adapter.borrow_mut().fail_destroy = Some(4);
        let mut backend = LinuxBackend::new("/dev/uinput".into());
        backend.start().unwrap();
        assert!(backend.controller_input_supported());
        assert!(!backend.switch_available);
        assert_eq!(backend.effective_gamepad_profile(), Some("auto"));

        let error = backend
            .inject(&Event::ControllerArrival {
                number: 0,
                controller_type: CONTROLLER_TYPE_NINTENDO,
                capabilities: 0,
                supported_buttons: u32::MAX,
            })
            .unwrap_err();
        assert_eq!(error.code, "controller_unavailable");
        assert!(error.message.contains("Switch"));
        assert_eq!(backend.controller_routes[0], Some(GamepadProfile::Switch));
        assert_eq!(input_adapter.borrow().next_id, 4);

        backend.advance(std::time::Instant::now()).unwrap();
        assert!(backend.switch_available);
        backend
            .inject(&Event::Controller {
                number: 0,
                active_mask: 1,
                buttons: 1 << 12,
                left_trigger: 0,
                right_trigger: 0,
                left_x: 0,
                left_y: 0,
                right_x: 0,
                right_y: 0,
            })
            .unwrap();
        assert_eq!(input_adapter.borrow().switch_devices.last().unwrap().id, 5);
        assert!(
            input_adapter
                .borrow()
                .frames
                .iter()
                .any(|frame| { frame.contains(&(EV_KEY, BTN_EAST, 1)) })
        );
        backend.stop().unwrap();
    }

    #[test]
    fn switch_failed_report_detaches_before_recreating_the_same_profile() {
        let _input_guard = FakeInputGuard;
        let input_adapter = FakeInputGuard::install();
        let mut backend = LinuxBackend::new("/dev/uinput".into());
        backend.set_gamepad_profile(GamepadProfile::Switch);
        backend.start().unwrap();
        backend
            .inject(&Event::ControllerArrival {
                number: 1,
                controller_type: CONTROLLER_TYPE_NINTENDO,
                capabilities: 0,
                supported_buttons: u32::MAX,
            })
            .unwrap();

        {
            let mut adapter = input_adapter.borrow_mut();
            adapter.fail_write = Some(4);
            adapter.fail_destroy = Some(4);
        }
        let report = Event::Controller {
            number: 1,
            active_mask: 1 << 1,
            buttons: 1 << 12,
            left_trigger: 0,
            right_trigger: 0,
            left_x: 0,
            left_y: 0,
            right_x: 0,
            right_y: 0,
        };
        assert_eq!(backend.inject(&report).unwrap_err().code, "release_failed");
        assert_eq!(input_adapter.borrow().destroys, [3, 4]);

        backend.advance(std::time::Instant::now()).unwrap();
        let adapter = input_adapter.borrow();
        assert_eq!(adapter.switch_devices.len(), 3);
        assert_eq!(
            adapter
                .switch_devices
                .iter()
                .map(|d| d.id)
                .collect::<Vec<_>>(),
            [3, 4, 5]
        );
        assert_eq!(adapter.destroys, [3, 4, 4]);
        assert!(
            adapter
                .frames
                .iter()
                .any(|frame| { frame.contains(&(EV_KEY, BTN_EAST, 1)) })
        );
        drop(adapter);
        backend.stop().unwrap();
        assert_eq!(input_adapter.borrow().destroys, [3, 4, 4, 5, 1, 2]);
    }

    #[test]
    fn auto_playstation_does_not_fall_back_when_ds5_probe_is_unavailable() {
        let _input_guard = FakeInputGuard;
        let input_adapter = FakeInputGuard::install();
        let mut backend = LinuxBackend::new("/dev/uinput".into());
        backend.start().unwrap();
        assert!(backend.controller_input_supported());
        assert_eq!(backend.effective_gamepad_profile(), Some("auto"));
        let error = backend
            .inject(&Event::ControllerArrival {
                number: 0,
                controller_type: CONTROLLER_TYPE_PLAYSTATION,
                capabilities: CONTROLLER_CAP_TOUCHPAD,
                supported_buttons: u32::MAX,
            })
            .unwrap_err();
        assert_eq!(error.code, "controller_unavailable");
        assert!(error.message.contains("DS5"));
        assert_eq!(backend.controller_routes[0], Some(GamepadProfile::Ds5));
        assert_eq!(input_adapter.borrow().next_id, 4);
        backend.stop().unwrap();
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

    #[test]
    fn keybindings_apply_once_and_reserved_sources_require_supported_destinations() {
        let mut backend = LinuxBackend::new("/missing/uinput".into());
        let custom: KeyBindings = serde_json::from_value(serde_json::json!([
            0x10, 0x41, 0xa0, 0x45, 0x41, 0x42, 0x42, 0x43, 0xa5, 0x44, 0, 0x41, 255, 0x41,
        ]))
        .unwrap();
        backend.set_keybindings(&custom);
        backend.set_key_rightalt_to_key_win(true);
        assert_eq!(
            backend.event_virtual_keycode(0x10, true, KeyState::Down),
            Some(42)
        );
        assert_eq!(
            backend.event_virtual_keycode(0xa0, true, KeyState::Down),
            Some(18)
        );
        assert_eq!(
            backend.event_virtual_keycode(0x41, true, KeyState::Down),
            Some(48)
        );
        assert_eq!(
            backend.event_virtual_keycode(0xa5, true, KeyState::Down),
            Some(32)
        );
        assert_eq!(
            backend.event_virtual_keycode(0, true, KeyState::Down),
            Some(30)
        );
        assert_eq!(
            backend.event_virtual_keycode(255, true, KeyState::Down),
            Some(30)
        );
        backend.set_keybindings(
            &serde_json::from_value(serde_json::json!([0x41, 0, 0x42, 255])).unwrap(),
        );
        assert_eq!(
            backend.event_virtual_keycode(0x41, true, KeyState::Down),
            None
        );
        assert_eq!(
            backend.event_virtual_keycode(0x42, false, KeyState::Down),
            None
        );
    }
}

impl LinuxBackend {
    fn event_virtual_keycode(&self, code: u8, normalized: bool, _state: KeyState) -> Option<u16> {
        let identity = code;
        if let Some(plan) = self.held_virtual_keycodes.held(identity) {
            return Some(plan.native);
        }
        code_for_virtual_key(
            code,
            normalized,
            self.key_rightalt_to_key_win,
            &self.keybindings,
        )
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

fn code_for_virtual_key(
    code: u8,
    _normalized: bool,
    key_rightalt_to_key_win: bool,
    keybindings: &KeyBindings,
) -> Option<u16> {
    let host_code = keybindings.resolve(code, key_rightalt_to_key_win);
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
    use crate::controller::Controller;
    use crate::model::{Operation, PenTouchPhase};

    struct FakeAdapterGuard;

    impl FakeAdapterGuard {
        fn install() -> std::rc::Rc<std::cell::RefCell<FakeInputAdapter>> {
            let adapter = std::rc::Rc::new(std::cell::RefCell::new(FakeInputAdapter::default()));
            FAKE_INPUT_ADAPTER.with(|slot| {
                assert!(slot.borrow_mut().replace(adapter.clone()).is_none());
            });
            adapter
        }
    }

    impl Drop for FakeAdapterGuard {
        fn drop(&mut self) {
            FAKE_INPUT_ADAPTER.with(|slot| {
                slot.borrow_mut().take();
            });
        }
    }

    fn moonlight_key(code: u8, state: KeyState) -> Event {
        Event::Key {
            key: Key::MoonlightVk {
                code,
                normalized: true,
                repeat_modifiers: 0,
                temporary_modifier: false,
            },
            state,
        }
    }

    fn temporary_modifier(code: u8, state: KeyState) -> Event {
        Event::Key {
            key: Key::MoonlightVk {
                code,
                normalized: true,
                repeat_modifiers: 0,
                temporary_modifier: true,
            },
            state,
        }
    }

    #[test]
    fn temporary_modifiers_reuse_remapped_right_host_keys_without_merging_real_left_keys() {
        for (generic, left, right, left_evdev, right_evdev, group) in [
            (0x10, 0xa0, 0xa1, 42, 54, HostModifier::Shift),
            (0x11, 0xa2, 0xa3, 29, 97, HostModifier::Control),
            (0x12, 0xa4, 0xa5, 56, 100, HostModifier::Alt),
        ] {
            let adapter = FakeAdapterGuard::install();
            let _guard = FakeAdapterGuard;
            let mut backend = LinuxBackend::new("/missing/uinput".into());
            backend.set_gamepad_profile(GamepadProfile::Ds4);
            backend.start().unwrap();
            backend
                .set_keybindings(&KeyBindings::from_flat(vec![0x41, right, right, 0x44]).unwrap());
            backend
                .inject(&moonlight_key(0x41, KeyState::Down))
                .unwrap();
            let first = backend.held_virtual_keycodes.held(0x41).unwrap();
            assert_eq!(first.native, right_evdev);
            assert_eq!(first.modifier, Some(group));
            // Source A no longer maps to a modifier, but its saved native plan does.
            backend.set_keybindings(&KeyBindings::empty());
            let synthetic_down = temporary_modifier(generic, KeyState::Down);
            backend.inject(&synthetic_down).unwrap();
            assert!(is_temporary_modifier(match &synthetic_down {
                Event::Key { key, .. } => key,
                _ => unreachable!(),
            }));
            assert_eq!(backend.held_virtual_keycodes.held(generic), Some(first));
            assert_eq!(adapter.borrow().writes, [1]);

            backend
                .inject(&moonlight_key(left, KeyState::Down))
                .unwrap();
            assert_eq!(backend.held_virtual_keycodes.get(&left), Some(&left_evdev));
            assert_eq!(backend.held_virtual_keycodes.len(), 3);
            backend.inject(&moonlight_key(left, KeyState::Up)).unwrap();
            backend
                .inject(&temporary_modifier(generic, KeyState::Up))
                .unwrap();
            assert_eq!(backend.held_virtual_keycodes.held(0x41), Some(first));
            assert!(!backend.held_virtual_keycodes.contains_key(&generic));
            backend
                .inject(&moonlight_key(0x41, KeyState::Repeat))
                .unwrap();
            backend.inject(&moonlight_key(0x41, KeyState::Up)).unwrap();
            assert!(backend.held_virtual_keycodes.is_empty());
            assert_eq!(
                adapter.borrow().frames,
                [
                    vec![(EV_KEY, right_evdev, 1)],
                    vec![(EV_KEY, left_evdev, 1)],
                    vec![(EV_KEY, left_evdev, 0)],
                    vec![(EV_KEY, right_evdev, 2)],
                    vec![(EV_KEY, right_evdev, 0)],
                ]
            );
            backend.stop().unwrap();
        }
    }

    #[test]
    fn failed_temporary_modifier_release_keeps_the_shared_plan_until_exact_device_cleanup() {
        let adapter = FakeAdapterGuard::install();
        let _guard = FakeAdapterGuard;
        let mut backend = LinuxBackend::new("/missing/uinput".into());
        backend.set_gamepad_profile(GamepadProfile::Ds4);
        backend.start().unwrap();
        backend.set_keybindings(&KeyBindings::from_flat(vec![0x41, 0xa3]).unwrap());
        backend
            .inject(&moonlight_key(0x41, KeyState::Down))
            .unwrap();
        let first = backend.held_virtual_keycodes.held(0x41).unwrap();
        backend
            .inject(&temporary_modifier(0x11, KeyState::Down))
            .unwrap();
        backend.inject(&moonlight_key(0x41, KeyState::Up)).unwrap();
        assert_eq!(adapter.borrow().writes, [1]);
        assert_eq!(backend.held_virtual_keycodes.held(0x11), Some(first));
        backend.set_keybindings(&KeyBindings::empty());
        adapter.borrow_mut().fail_write = Some(1);
        let release = temporary_modifier(0x11, KeyState::Up);
        assert_eq!(
            backend.inject(&release).unwrap_err().code,
            "injection_failed"
        );
        assert_eq!(backend.held_virtual_keycodes.held(0x11), Some(first));
        assert!(backend.keyboard_recovery_required);
        adapter.borrow_mut().fail_destroy = Some(1);
        assert_eq!(backend.inject(&release).unwrap_err().code, "release_failed");
        assert_eq!(backend.held_virtual_keycodes.held(0x11), Some(first));
        assert_eq!(adapter.borrow().next_id, 2);
        backend.inject(&release).unwrap();
        assert!(backend.held_virtual_keycodes.is_empty());
        assert!(backend.keyboard.is_none());
        assert_eq!(adapter.borrow().next_id, 2);
        assert_eq!(
            adapter.borrow().frames,
            [vec![(EV_KEY, 97, 1)], vec![(EV_KEY, 97, 0)]]
        );
        backend.stop().unwrap();
    }

    fn pointer_button(button: Button, pressed: bool) -> Event {
        Event::Button { button, pressed }
    }

    #[test]
    fn remapped_sources_keep_target_ownership_until_final_release() {
        let adapter = FakeAdapterGuard::install();
        let _guard = FakeAdapterGuard;
        let mut backend = LinuxBackend::new("/missing/uinput".into());
        backend.set_gamepad_profile(GamepadProfile::Ds4);
        backend.start().unwrap();
        backend.set_keybindings(
            &serde_json::from_value(serde_json::json!([0x41, 0x43, 0x42, 0x43])).unwrap(),
        );
        backend
            .inject(&moonlight_key(0x41, KeyState::Down))
            .unwrap();
        backend
            .inject(&moonlight_key(0x42, KeyState::Down))
            .unwrap();
        assert_eq!(adapter.borrow().writes, [1]);
        assert_eq!(backend.held_virtual_keycodes.get(&0x41), Some(&46));
        assert_eq!(backend.held_virtual_keycodes.get(&0x42), Some(&46));
        backend.set_keybindings(
            &serde_json::from_value(serde_json::json!([0x41, 0x44, 0x42, 0x45])).unwrap(),
        );
        backend
            .inject(&moonlight_key(0x42, KeyState::Repeat))
            .unwrap();
        backend.inject(&moonlight_key(0x41, KeyState::Up)).unwrap();
        assert_eq!(adapter.borrow().writes, [1, 1]);
        assert_eq!(backend.held_virtual_keycodes.get(&0x42), Some(&46));
        backend.inject(&moonlight_key(0x42, KeyState::Up)).unwrap();
        assert_eq!(adapter.borrow().writes, [1, 1, 1]);
        assert!(backend.held_virtual_keycodes.is_empty());
        backend.stop().unwrap();
    }

    fn acquire_fake_lease(controller: &mut Controller<LinuxBackend>) -> String {
        let response = controller.handle(
            "owner",
            Operation::Acquire {
                operation_id: None,
                ttl_ms: Some(5000),
                back_button_timeout: None,
                controller: None,
                gamepad: Some(GamepadProfile::Ds4),
                ds4_back_as_touchpad_click: None,
                motion_as_ds4: None,
                touchpad_as_ds4: None,
                ds5_inputtino_randomize_mac: None,
                native_pen_touch: None,
                keyboard: None,
                mouse: None,
                high_resolution_scrolling: None,
                always_send_scancodes: None,
                key_rightalt_to_key_win: None,
                keybindings: None,
                key_repeat_delay: None,
                key_repeat_frequency: None,
            },
            std::time::Instant::now(),
            &mut || Ok(()),
        );
        assert_eq!(response["ok"], true);
        response["lease"].as_str().unwrap().to_owned()
    }

    fn fake_batch(
        controller: &mut Controller<LinuxBackend>,
        lease: &str,
        events: Vec<Event>,
    ) -> serde_json::Value {
        controller.handle(
            "owner",
            Operation::Batch {
                lease: lease.into(),
                events,
            },
            std::time::Instant::now(),
            &mut || Ok(()),
        )
    }

    fn gamepad_event(active_mask: u16, buttons: u32) -> Event {
        Event::Controller {
            number: 0,
            active_mask,
            buttons,
            left_trigger: 0,
            right_trigger: 0,
            left_x: 0,
            left_y: 0,
            right_x: 0,
            right_y: 0,
        }
    }

    #[test]
    fn failed_neutral_frame_and_destroy_retry_detaches_before_reacquisition() {
        let adapter = FakeAdapterGuard::install();
        let _guard = FakeAdapterGuard;
        let mut backend = LinuxBackend::new("/missing/uinput".into());
        backend.set_gamepad_profile(GamepadProfile::Xone);
        backend.start().unwrap();
        assert_eq!(backend.effective_gamepad_profile(), Some("xone"));
        backend.inject(&gamepad_event(1, 1 << 12)).unwrap();
        assert_eq!(adapter.borrow().writes, [4]);

        {
            let mut fake = adapter.borrow_mut();
            fake.fail_write = Some(4);
            fake.fail_destroy = Some(4);
        }
        assert_eq!(
            backend.inject(&gamepad_event(0, 0)).unwrap_err().code,
            "release_failed"
        );
        assert_eq!(adapter.borrow().writes, [4, 4]);
        assert_eq!(adapter.borrow().destroys, [3, 4]);

        // The keyboard and pointer continue to work while the old gamepad is
        // owned for cleanup. Releasing it must not replay the uncertain frame.
        backend.inject(&Event::Relative { dx: 2, dy: 3 }).unwrap();
        backend.advance(std::time::Instant::now()).unwrap();
        assert_eq!(adapter.borrow().writes, [4, 4, 2]);
        assert_eq!(adapter.borrow().destroys, [3, 4, 4]);

        backend.inject(&gamepad_event(1, 1 << 13)).unwrap();
        assert_eq!(adapter.borrow().writes, [4, 4, 2, 5]);
        backend.stop().unwrap();
        assert_eq!(adapter.borrow().destroys, [3, 4, 4, 5, 1, 2]);
    }

    #[test]
    fn failed_gamepad_probe_destroy_recovers_without_allocating_another_probe() {
        let adapter = FakeAdapterGuard::install();
        let _guard = FakeAdapterGuard;
        let mut backend = LinuxBackend::new("/missing/uinput".into());
        backend.set_gamepad_profile(GamepadProfile::Xone);
        adapter.borrow_mut().fail_destroy = Some(3);

        backend.start().unwrap();
        assert!(!backend.controller_input_supported());
        assert_eq!(backend.effective_gamepad_profile(), None);
        assert_eq!(adapter.borrow().next_id, 3);
        assert_eq!(adapter.borrow().destroys, [3]);
        assert_eq!(
            backend.inject(&gamepad_event(1, 1 << 12)).unwrap_err().code,
            "controller_unavailable"
        );
        backend.inject(&Event::Relative { dx: 1, dy: 2 }).unwrap();

        adapter.borrow_mut().fail_destroy = Some(3);
        backend.advance(std::time::Instant::now()).unwrap();
        assert!(!backend.controller_input_supported());
        assert_eq!(adapter.borrow().next_id, 3);
        assert_eq!(adapter.borrow().destroys, [3, 3]);

        backend.advance(std::time::Instant::now()).unwrap();
        assert!(backend.controller_input_supported());
        assert_eq!(backend.effective_gamepad_profile(), Some("xone"));
        assert!(backend.gamepad_unavailable_reason().is_none());
        assert_eq!(adapter.borrow().next_id, 3);
        assert_eq!(adapter.borrow().destroys, [3, 3, 3]);
        backend.inject(&gamepad_event(1, 1 << 12)).unwrap();
        assert_eq!(adapter.borrow().next_id, 4);
        backend.stop().unwrap();
        assert_eq!(adapter.borrow().destroys, [3, 3, 3, 4, 1, 2]);
    }

    #[test]
    fn controller_cleanup_releases_all_keys_after_uncertain_keyboard_destroy_retry() {
        let adapter = FakeAdapterGuard::install();
        let _guard = FakeAdapterGuard;
        let mut controller = Controller::new(LinuxBackend::new("/missing/uinput".into()), true);
        let lease = acquire_fake_lease(&mut controller);
        assert_eq!(
            fake_batch(
                &mut controller,
                &lease,
                vec![
                    moonlight_key(0x41, KeyState::Down),
                    moonlight_key(0x42, KeyState::Down),
                    pointer_button(Button::Left, true),
                ],
            )["ok"],
            true
        );
        {
            let mut fake = adapter.borrow_mut();
            fake.fail_write = Some(1);
            fake.fail_destroy = Some(1);
            fake.fail_create = true;
        }
        let failure = fake_batch(
            &mut controller,
            &lease,
            vec![moonlight_key(0x43, KeyState::Down)],
        );
        assert_eq!(failure["error"]["code"], "injection_failed");
        assert_eq!(failure["cleanup_required"], true);
        assert_eq!(adapter.borrow().next_id, 2);
        assert!(adapter.borrow().fail_create);
        let released = controller.handle(
            "owner",
            Operation::Release { lease },
            std::time::Instant::now(),
            &mut || Ok(()),
        );
        assert_eq!(released["ok"], true);
        assert_eq!(released["released"], true);
        assert!(!controller.cleanup_required());
        assert_eq!(adapter.borrow().destroys, [1, 1, 2]);
        assert_eq!(adapter.borrow().next_id, 2);
        assert!(adapter.borrow().fail_create);
        adapter.borrow_mut().fail_create = false;
        let next_lease = acquire_fake_lease(&mut controller);
        assert_eq!(adapter.borrow().next_id, 4);
        assert_eq!(
            controller.handle(
                "owner",
                Operation::Release { lease: next_lease },
                std::time::Instant::now(),
                &mut || Ok(()),
            )["ok"],
            true
        );
    }

    #[test]
    fn uncertain_keyboard_cleanup_retires_without_reopening_and_keeps_pointer_live() {
        let adapter = FakeAdapterGuard::install();
        let _guard = FakeAdapterGuard;
        let mut backend = LinuxBackend::new("/missing/uinput".into());
        backend.set_gamepad_profile(GamepadProfile::Ds4);
        backend.start().unwrap();
        backend
            .inject(&moonlight_key(0x41, KeyState::Down))
            .unwrap();
        backend
            .inject(&moonlight_key(0x42, KeyState::Down))
            .unwrap();
        backend.inject(&pointer_button(Button::Left, true)).unwrap();

        adapter.borrow_mut().fail_write = Some(1);
        assert_eq!(
            backend
                .inject(&moonlight_key(0x43, KeyState::Down))
                .unwrap_err()
                .code,
            "injection_failed"
        );
        adapter.borrow_mut().fail_destroy = Some(1);
        assert_eq!(
            backend
                .inject(&moonlight_key(0x43, KeyState::Up))
                .unwrap_err()
                .code,
            "release_failed"
        );
        assert!(backend.keyboard_recovery_required);
        assert_eq!(adapter.borrow().next_id, 2);

        adapter.borrow_mut().fail_create = true;
        backend.inject(&moonlight_key(0x43, KeyState::Up)).unwrap();
        backend.inject(&moonlight_key(0x42, KeyState::Up)).unwrap();
        backend.inject(&moonlight_key(0x41, KeyState::Up)).unwrap();
        assert!(backend.keyboard_retired);
        assert!(backend.keyboard.is_none());
        assert!(backend.held_virtual_keycodes.is_empty());
        assert_eq!(
            backend
                .inject(&moonlight_key(0x44, KeyState::Down))
                .unwrap_err()
                .code,
            "backend_inactive"
        );
        assert_eq!(adapter.borrow().next_id, 2);
        assert!(adapter.borrow().fail_create);

        backend
            .inject(&pointer_button(Button::Left, false))
            .unwrap();
        assert_eq!(adapter.borrow().writes, [1, 1, 2, 1, 2]);
        assert_eq!(adapter.borrow().destroys, [1, 1]);
        backend.stop().unwrap();
        assert_eq!(adapter.borrow().destroys, [1, 1, 2]);
        assert_eq!(backend.start().unwrap_err().code, "acquire_failed");
        backend.start().unwrap();
        assert_eq!(adapter.borrow().next_id, 4);
        backend.stop().unwrap();
    }

    #[test]
    fn uncertain_pointer_cleanup_retires_without_reopening_and_keeps_keyboard_live() {
        let adapter = FakeAdapterGuard::install();
        let _guard = FakeAdapterGuard;
        let mut backend = LinuxBackend::new("/missing/uinput".into());
        backend.set_gamepad_profile(GamepadProfile::Ds4);
        backend.start().unwrap();
        backend
            .inject(&moonlight_key(0x41, KeyState::Down))
            .unwrap();
        backend.inject(&pointer_button(Button::Left, true)).unwrap();
        backend
            .inject(&pointer_button(Button::Right, true))
            .unwrap();

        adapter.borrow_mut().fail_write = Some(2);
        assert_eq!(
            backend
                .inject(&Event::Relative { dx: 1, dy: 2 })
                .unwrap_err()
                .code,
            "injection_failed"
        );
        adapter.borrow_mut().fail_destroy = Some(2);
        assert_eq!(
            backend
                .inject(&pointer_button(Button::Right, false))
                .unwrap_err()
                .code,
            "release_failed"
        );
        adapter.borrow_mut().fail_create = true;
        backend
            .inject(&pointer_button(Button::Right, false))
            .unwrap();
        backend
            .inject(&pointer_button(Button::Left, false))
            .unwrap();
        assert!(backend.pointer_retired);
        assert!(backend.pointer.is_none());
        assert_eq!(
            backend
                .inject(&Event::Relative { dx: 1, dy: 2 })
                .unwrap_err()
                .code,
            "backend_inactive"
        );
        assert!(adapter.borrow().fail_create);
        backend.inject(&moonlight_key(0x41, KeyState::Up)).unwrap();
        assert_eq!(adapter.borrow().writes, [1, 2, 2, 2, 1]);
        assert_eq!(adapter.borrow().destroys, [2, 2]);
        backend.stop().unwrap();
        assert_eq!(adapter.borrow().destroys, [2, 2, 1]);
    }

    #[test]
    fn terminal_key_release_completes_after_a_failed_keyboard_recreation() {
        let adapter = FakeAdapterGuard::install();
        let _guard = FakeAdapterGuard;
        let mut backend = LinuxBackend::new("/missing/uinput".into());
        backend.set_gamepad_profile(GamepadProfile::Ds4);
        backend.start().unwrap();
        adapter.borrow_mut().fail_write = Some(1);
        assert!(
            backend
                .inject(&moonlight_key(0x41, KeyState::Down))
                .is_err()
        );
        adapter.borrow_mut().fail_create = true;
        assert_eq!(
            backend
                .inject(&moonlight_key(0x42, KeyState::Down))
                .unwrap_err()
                .code,
            "acquire_failed"
        );
        assert!(backend.keyboard.is_none());
        assert!(backend.keyboard_recovery_required);
        backend.inject(&moonlight_key(0x41, KeyState::Up)).unwrap();
        assert!(backend.keyboard_retired);
        backend.stop().unwrap();
        assert_eq!(adapter.borrow().destroys, [1, 2]);
    }

    #[test]
    fn failed_keyboard_frame_and_destroy_block_reacquisition_until_cleanup() {
        let adapter = FakeAdapterGuard::install();
        let _guard = FakeAdapterGuard;
        let mut backend = LinuxBackend::new("/missing/uinput".into());
        backend.set_gamepad_profile(GamepadProfile::Ds4);
        backend.start().unwrap();
        assert_eq!(
            backend.keyboard.as_ref().unwrap().fake.as_ref().unwrap().0,
            1
        );
        assert_eq!(
            backend.pointer.as_ref().unwrap().fake.as_ref().unwrap().0,
            2
        );

        adapter.borrow_mut().fail_write = Some(1);
        assert_eq!(
            backend
                .inject(&Event::Key {
                    key: Key::MoonlightVk {
                        code: 0x41,
                        normalized: true,
                        repeat_modifiers: 0,
                        temporary_modifier: false,
                    },
                    state: KeyState::Down,
                })
                .unwrap_err()
                .code,
            "injection_failed"
        );
        assert!(backend.keyboard_recovery_required);
        assert!(backend.held_virtual_keycodes.contains_key(&0x41));
        backend.inject(&Event::Relative { dx: 4, dy: -2 }).unwrap();
        assert_eq!(adapter.borrow().writes, [1, 2]);

        adapter.borrow_mut().fail_destroy = Some(1);
        assert_eq!(backend.stop().unwrap_err().code, "release_failed");
        assert!(backend.keyboard_recovery_required);
        assert!(backend.held_virtual_keycodes.contains_key(&0x41));
        assert!(backend.keyboard.is_some());
        assert!(backend.pointer.is_none());
        assert_eq!(backend.start().unwrap_err().code, "backend_busy");
        assert_eq!(adapter.borrow().next_id, 2);

        backend.stop().unwrap();
        assert!(!backend.keyboard_recovery_required);
        assert!(backend.held_virtual_keycodes.is_empty());
        assert!(backend.keyboard.is_none());
        backend.start().unwrap();
        assert!(backend.held_virtual_keycodes.is_empty());
        assert_eq!(
            backend.keyboard.as_ref().unwrap().fake.as_ref().unwrap().0,
            3
        );
        assert_eq!(
            backend.pointer.as_ref().unwrap().fake.as_ref().unwrap().0,
            4
        );
        backend.inject(&Event::Relative { dx: 1, dy: 1 }).unwrap();
        assert_eq!(adapter.borrow().writes, [1, 2, 4]);
        backend.stop().unwrap();
        backend.stop().unwrap();
        assert_eq!(adapter.borrow().destroys, [1, 2, 1, 3, 4]);
    }

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
            "controller_unavailable"
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
        let bindings = KeyBindings::default();
        assert_eq!(
            code_for_virtual_key(0xa5, true, true, &bindings),
            Some(left_meta)
        );
        assert_eq!(
            code_for_virtual_key(0xa5, false, true, &bindings),
            Some(left_meta)
        );
        assert_eq!(
            code_for_virtual_key(0xa5, true, false, &bindings),
            Some(right_alt)
        );

        let mut backend = LinuxBackend::new(String::new());
        backend.set_key_rightalt_to_key_win(true);
        let down = backend
            .event_virtual_keycode(0xa5, true, KeyState::Down)
            .unwrap();
        assert_eq!(down, left_meta);
        backend.held_virtual_keycodes.insert(
            0xa5,
            OwnedKeyPlan {
                target: down,
                native: down,
                modifier: modifier_for_evdev(down),
            },
        );

        backend.set_key_rightalt_to_key_win(false);
        assert_eq!(
            backend.event_virtual_keycode(0xa5, false, KeyState::Up),
            Some(left_meta),
            "a setting change cannot change the native key selected at key-down"
        );
    }

    #[test]
    fn high_resolution_scroll_maps_raw_units_and_compatible_detents_per_axis() {
        let vertical = 0;
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

fn write_frame(
    fd: RawFd,
    frame: &[libc::input_event],
    events_written: &mut usize,
) -> InputResult<()> {
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
        *events_written = written / std::mem::size_of::<libc::input_event>();
    }
    Ok(())
}
fn set_bit(fd: RawFd, request: libc::Ioctl, bit: u32) -> InputResult<()> {
    // SAFETY: capability ioctls take a scalar bit number, not a pointer; fd is live.
    check(unsafe { libc::ioctl(fd, request, bit as libc::c_int) })
}
fn set_absolute_axis(fd: RawFd, code: u16, minimum: i32, maximum: i32) -> InputResult<()> {
    set_absolute_axis_with_calibration(fd, code, minimum, maximum, 0, 0)
}

fn set_absolute_axis_with_calibration(
    fd: RawFd,
    code: u16,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
) -> InputResult<()> {
    set_bit(fd, UI_SET_ABSBIT, u32::from(code))?;
    // SAFETY: uinput_abs_setup and input_absinfo are integer-only Linux UAPI
    // records, and all fields are initialized before the kernel reads them.
    let setup = libc::uinput_abs_setup {
        code,
        absinfo: libc::input_absinfo {
            value: 0,
            minimum,
            maximum,
            fuzz,
            flat,
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
