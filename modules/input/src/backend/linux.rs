use super::Backend;
use crate::model::{Axis, Button, Event, InputError, InputResult, Key, KeyState};
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};

// Linux UAPI constants from linux/input-event-codes.h and linux/uinput.h.
// libc's _IO/_IOW use the target architecture's ioctl encoding and ABI types.
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const UI_SET_EVBIT: libc::Ioctl = libc::_IOW::<libc::c_int>(b'U' as u32, 100);
const UI_SET_KEYBIT: libc::Ioctl = libc::_IOW::<libc::c_int>(b'U' as u32, 101);
const UI_SET_RELBIT: libc::Ioctl = libc::_IOW::<libc::c_int>(b'U' as u32, 102);
const UI_DEV_SETUP: libc::Ioctl = libc::_IOW::<libc::uinput_setup>(b'U' as u32, 3);
const UI_DEV_CREATE: libc::Ioctl = libc::_IO(b'U' as u32, 1);
const UI_DEV_DESTROY: libc::Ioctl = libc::_IO(b'U' as u32, 2);

struct Device {
    file: File,
}
impl Device {
    fn create(path: &str, keyboard: bool) -> InputResult<Self> {
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
            for code in [0, 1, 6, 8] {
                set_bit(fd, UI_SET_RELBIT, code)?;
            }
        }
        // SAFETY: this Linux ABI packet has integer and byte-array fields; all-zero
        // is valid initialization. No pointer or driver-private payload is stored.
        let mut setup: libc::uinput_setup = unsafe { std::mem::zeroed() };
        setup.id.bustype = 0x06; // BUS_VIRTUAL, not a borrowed hardware USB identity.
        setup.id.version = 1;
        let name: &[u8] = if keyboard {
            b"Amaterasu keyboard"
        } else {
            b"Amaterasu pointer"
        };
        for (target, source) in setup.name.iter_mut().zip(name) {
            *target = *source as libc::c_char;
        }
        // SAFETY: fd is an owned uinput descriptor and setup remains alive with
        // the exact uinput_setup size encoded into UI_DEV_SETUP.
        check(unsafe { libc::ioctl(fd, UI_DEV_SETUP, &setup) })?;
        // SAFETY: this ioctl takes no pointer argument and creates the configured device.
        check(unsafe { libc::ioctl(fd, UI_DEV_CREATE) })?;
        Ok(Self { file })
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
}
impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the owned descriptor remains live throughout its device destroy
        // ioctl. Closing the file then lets the kernel tear down the virtual device.
        let _ = unsafe { libc::ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY) };
    }
}
pub struct LinuxBackend {
    path: String,
    keyboard: Option<Device>,
    pointer: Option<Device>,
}
impl LinuxBackend {
    pub fn new(path: String) -> Self {
        Self {
            path,
            keyboard: None,
            pointer: None,
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
    fn key_encoding(&self) -> &'static str {
        "linux_evdev"
    }
    fn validate(&self, event: &Event) -> InputResult<()> {
        event.validate()?;
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
        Ok(())
    }
    fn start(&mut self) -> InputResult<()> {
        if self.keyboard.is_some() || self.pointer.is_some() {
            return Err(InputError::new(
                "backend_busy",
                "uinput devices are already active",
            ));
        }
        let keyboard = Device::create(&self.path, true)?;
        let pointer = Device::create(&self.path, false)?;
        self.keyboard = Some(keyboard);
        self.pointer = Some(pointer);
        Ok(())
    }
    fn inject(&mut self, event: &Event) -> InputResult<()> {
        self.validate(event)?;
        if let Event::Key { key, state } = event {
            let code = match key {
                Key::LinuxEvdev { code } => *code,
                Key::MoonlightVk { code } => {
                    amaterasu_gamestream::control::virtual_key_mapping(*code)
                        .ok_or_else(|| {
                            InputError::new("unsupported_event", "unmapped Moonlight key")
                        })?
                        .linux_evdev_code
                }
                Key::WindowsScan { .. } => {
                    return Err(InputError::new(
                        "unsupported_event",
                        "Linux requires evdev codes or normalized Moonlight keys",
                    ));
                }
            };
            let keyboard = self.keyboard.as_ref().ok_or_else(not_started)?;
            return keyboard.emit(&[(
                EV_KEY,
                code,
                match state {
                    KeyState::Up => 0,
                    KeyState::Down => 1,
                    KeyState::Repeat => 2,
                },
            )]);
        }
        let pointer = self.pointer.as_ref().ok_or_else(not_started)?;
        match event {
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
            Event::Relative { dx, dy } => pointer.emit(&[(EV_REL, 0, *dx), (EV_REL, 1, *dy)]),
            Event::Scroll { axis, steps } => pointer.emit(&[(
                EV_REL,
                match axis {
                    Axis::Horizontal => 6,
                    Axis::Vertical => 8,
                },
                *steps,
            )]),
            _ => Err(InputError::new(
                "unsupported_event",
                "unsupported uinput event",
            )),
        }
    }
    fn stop(&mut self) {
        self.pointer = None;
        self.keyboard = None;
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
    assert!(std::mem::size_of::<libc::uinput_setup>() == 92);
    assert!(std::mem::size_of::<libc::input_event>() == 24);
    assert!(UI_DEV_SETUP == 0x405c_5503);
    assert!(UI_SET_EVBIT == 0x4004_5564);
    assert!(UI_SET_KEYBIT == 0x4004_5565);
    assert!(UI_SET_RELBIT == 0x4004_5566);
    assert!(UI_DEV_CREATE == 0x5501);
    assert!(UI_DEV_DESTROY == 0x5502);
};
