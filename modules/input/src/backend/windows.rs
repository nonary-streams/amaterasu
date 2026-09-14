use super::Backend;
use crate::model::{Axis, Button, Event, InputError, InputResult, Key, KeyState};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

pub struct WindowsBackend;
impl WindowsBackend {
    pub fn new(_: String) -> Self {
        Self
    }
}
impl Backend for WindowsBackend {
    fn name(&self) -> &'static str {
        "windows_send_input"
    }
    fn supported(&self) -> bool {
        true
    }
    fn absolute_pointer(&self) -> bool {
        true
    }
    fn key_encoding(&self) -> &'static str {
        "windows_scan"
    }
    fn validate(&self, event: &Event) -> InputResult<()> {
        event.validate()?;
        if matches!(
            event,
            Event::Key {
                key: Key::LinuxEvdev { .. },
                ..
            }
        ) {
            return Err(InputError::new(
                "unsupported_event",
                "Windows requires windows_scan keyboard codes",
            ));
        }
        Ok(())
    }
    fn start(&mut self) -> InputResult<()> {
        Ok(())
    }
    fn inject(&mut self, event: &Event) -> InputResult<()> {
        let input = native_input(event)?;
        // SAFETY: input is a fully initialized INPUT with the matching union tag.
        // SendInput reads this one owned record synchronously and retains no pointer.
        let accepted = unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
        if accepted != 1 {
            return Err(InputError::new(
                "injection_failed",
                format!(
                    "SendInput accepted {accepted}/1 events (desktop/UIPI restrictions may apply): {}",
                    std::io::Error::last_os_error()
                ),
            ));
        }
        Ok(())
    }
    fn stop(&mut self) {}
}
fn native_input(event: &Event) -> InputResult<INPUT> {
    if let Event::Key { key, state } = event {
        let (code, extended) = match key {
            Key::WindowsScan { code, extended } => (*code, *extended),
            Key::MoonlightVk { code } => {
                let mapping = amaterasu_gamestream::control::virtual_key_mapping(*code)
                    .ok_or_else(|| {
                        InputError::new("unsupported_event", "unmapped Moonlight key")
                    })?;
                (mapping.windows_scan_code, mapping.windows_extended)
            }
            Key::LinuxEvdev { .. } => {
                return Err(InputError::new(
                    "unsupported_event",
                    "Windows requires scan codes or normalized Moonlight keys",
                ));
            }
        };
        let flags = KEYEVENTF_SCANCODE
            | if extended { KEYEVENTF_EXTENDEDKEY } else { 0 }
            | if *state == KeyState::Up {
                KEYEVENTF_KEYUP
            } else {
                0
            };
        return Ok(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: 0,
                    wScan: code,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }
    let mut mouse = MOUSEINPUT::default();
    match event {
        Event::Button { button, pressed } => {
            mouse.dwFlags = match (button, pressed) {
                (Button::Left, true) => MOUSEEVENTF_LEFTDOWN,
                (Button::Left, false) => MOUSEEVENTF_LEFTUP,
                (Button::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                (Button::Right, false) => MOUSEEVENTF_RIGHTUP,
                (Button::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                (Button::Middle, false) => MOUSEEVENTF_MIDDLEUP,
                (_, true) => MOUSEEVENTF_XDOWN,
                (_, false) => MOUSEEVENTF_XUP,
            };
            mouse.mouseData = match button {
                Button::Back => 1,
                Button::Forward => 2,
                _ => 0,
            };
        }
        Event::Relative { dx, dy } => {
            mouse.dx = *dx;
            mouse.dy = *dy;
            mouse.dwFlags = MOUSEEVENTF_MOVE | MOUSEEVENTF_MOVE_NOCOALESCE;
        }
        Event::Absolute { x, y } => {
            mouse.dx = i32::from(*x);
            mouse.dy = i32::from(*y);
            mouse.dwFlags = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
        }
        Event::Scroll { axis, steps } => {
            mouse.dwFlags = match axis {
                Axis::Horizontal => MOUSEEVENTF_HWHEEL,
                Axis::Vertical => MOUSEEVENTF_WHEEL,
            };
            mouse.mouseData = (*steps * 120) as u32;
        }
        Event::Key { .. } => unreachable!("keyboard handled above"),
    }
    Ok(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: mouse },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encoding_preserves_extended_keys_virtual_desktop_and_signed_scroll() {
        let key = native_input(&Event::Key {
            key: Key::WindowsScan {
                code: 0x1d,
                extended: true,
            },
            state: KeyState::Up,
        })
        .unwrap();
        assert_eq!(key.r#type, INPUT_KEYBOARD);
        // SAFETY: native_input selected INPUT_KEYBOARD, so ki is the active view.
        let key = unsafe { key.Anonymous.ki };
        assert_eq!(key.wVk, 0);
        assert_eq!(key.wScan, 0x1d);
        assert_eq!(
            key.dwFlags,
            KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP
        );
        let absolute = native_input(&Event::Absolute { x: 65535, y: 0 }).unwrap();
        // SAFETY: absolute events encode INPUT_MOUSE and initialize mi.
        let absolute = unsafe { absolute.Anonymous.mi };
        assert_eq!(absolute.dx, 65535);
        assert_eq!(
            absolute.dwFlags,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK
        );
        let scroll = native_input(&Event::Scroll {
            axis: Axis::Vertical,
            steps: -2,
        })
        .unwrap();
        // SAFETY: scroll events encode INPUT_MOUSE and initialize mi.
        let scroll = unsafe { scroll.Anonymous.mi };
        assert_eq!(scroll.mouseData as i32, -240);
    }
}
