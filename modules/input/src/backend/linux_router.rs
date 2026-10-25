//! Select a backend from requested capabilities before acquiring devices.
//! A failed companion request never changes this selection or replays input.
use super::{Backend, linux::LinuxBackend, linux_companion::LinuxCompanionBackend};
use crate::{
    feedback::FeedbackQueue,
    gamepad_profile::GamepadProfile,
    model::{Event, InputResult, PenTouchDevice},
};
use amaterasu_input_policy::KeyBindings;
use std::time::Instant;

pub struct LinuxBackendRouter {
    companion: LinuxCompanionBackend,
    native: LinuxBackend,
    native_profile: bool,
    native_keybindings: bool,
    active: bool,
    native_pen_route: bool,
}
impl LinuxBackendRouter {
    pub fn new(socket: String, uinput: String) -> Self {
        Self {
            companion: LinuxCompanionBackend::new(socket),
            native: LinuxBackend::new(uinput),
            native_profile: false,
            native_keybindings: false,
            active: false,
            native_pen_route: false,
        }
    }
    fn requires_native(&self) -> bool {
        self.native_profile || self.native_keybindings
    }
    fn selected(&self) -> &dyn Backend {
        if self.requires_native() {
            &self.native
        } else {
            &self.companion
        }
    }
    fn selected_mut(&mut self) -> &mut dyn Backend {
        if self.requires_native() {
            &mut self.native
        } else {
            &mut self.companion
        }
    }
}
impl Backend for LinuxBackendRouter {
    fn name(&self) -> &'static str {
        self.selected().name()
    }
    fn supported(&self) -> bool {
        self.selected().supported()
    }
    fn absolute_pointer(&self) -> bool {
        self.selected().absolute_pointer()
    }
    fn pen_touch_supported(&self) -> bool {
        if self.active {
            self.native_pen_route
        } else {
            self.native.pen_touch_supported()
        }
    }
    fn supports_pen_touch_event(&self, device: PenTouchDevice) -> bool {
        self.pen_touch_supported() || self.selected().supports_pen_touch_event(device)
    }
    fn pen_touch_max_contacts(&self, device: PenTouchDevice) -> Option<usize> {
        if self.pen_touch_supported() {
            self.native.pen_touch_max_contacts(device)
        } else {
            self.selected().pen_touch_max_contacts(device)
        }
    }
    fn touch_as_mouse_max_contacts(&self) -> Option<usize> {
        if self.pen_touch_supported() {
            None
        } else {
            self.selected().touch_as_mouse_max_contacts()
        }
    }
    fn controller_input_supported(&self) -> bool {
        self.selected().controller_input_supported()
    }
    fn effective_gamepad_profile(&self) -> Option<&'static str> {
        self.selected().effective_gamepad_profile()
    }
    fn gamepad_unavailable_reason(&self) -> Option<&str> {
        self.selected().gamepad_unavailable_reason()
    }
    fn key_encoding(&self) -> &'static str {
        self.selected().key_encoding()
    }
    fn validate(&self, event: &Event) -> InputResult<()> {
        if matches!(event, Event::PenTouch { .. }) && self.pen_touch_supported() {
            self.native.validate(event)
        } else {
            self.selected().validate(event)
        }
    }
    fn inject(&mut self, event: &Event) -> InputResult<()> {
        if matches!(event, Event::PenTouch { .. })
            && self.native_pen_route
            && !self.requires_native()
        {
            // This event family is selected before the lease starts. Native
            // injection failures never fall back to companion mouse emulation.
            self.native.inject(event)
        } else {
            self.selected_mut().inject(event)
        }
    }
    fn advance(&mut self, now: Instant) -> InputResult<()> {
        self.selected_mut().advance(now)
    }
    fn advance_pen_touch(&mut self, now: Instant) -> InputResult<()> {
        self.selected_mut().advance_pen_touch(now)
    }
    fn disable_controller_input(&mut self) -> InputResult<()> {
        self.selected_mut().disable_controller_input()
    }
    fn poll_gamepad_feedback(
        &mut self,
    ) -> InputResult<Option<amaterasu_input_companion::GamepadFeedback>> {
        self.selected_mut().poll_gamepad_feedback()
    }
    fn set_always_send_scancodes(&mut self, value: bool) {
        self.companion.set_always_send_scancodes(value);
        self.native.set_always_send_scancodes(value);
    }
    fn set_key_rightalt_to_key_win(&mut self, value: bool) {
        self.companion.set_key_rightalt_to_key_win(value);
        self.native.set_key_rightalt_to_key_win(value);
    }
    fn set_back_button_timeout(&mut self, value: i32) {
        self.companion.set_back_button_timeout(value);
        self.native.set_back_button_timeout(value);
    }
    fn set_ds4_back_as_touchpad_click(&mut self, value: bool) {
        self.companion.set_ds4_back_as_touchpad_click(value);
        self.native.set_ds4_back_as_touchpad_click(value);
    }
    fn set_motion_as_ds4(&mut self, value: bool) {
        self.companion.set_motion_as_ds4(value);
        self.native.set_motion_as_ds4(value);
    }
    fn set_touchpad_as_ds4(&mut self, value: bool) {
        self.companion.set_touchpad_as_ds4(value);
        self.native.set_touchpad_as_ds4(value);
    }
    fn set_ds5_inputtino_randomize_mac(&mut self, value: bool) {
        self.companion.set_ds5_inputtino_randomize_mac(value);
        self.native.set_ds5_inputtino_randomize_mac(value);
    }
    fn set_feedback_generation(&mut self, value: u64) {
        self.companion.set_feedback_generation(value);
        self.native.set_feedback_generation(value);
    }
    fn set_feedback_queue(&mut self, queue: FeedbackQueue) {
        self.companion.set_feedback_queue(queue.clone());
        self.native.set_feedback_queue(queue);
    }
    fn supported_gamepad_profiles(&self) -> &'static [&'static str] {
        GamepadProfile::implemented_profiles()
    }
    fn set_gamepad_profile(&mut self, profile: GamepadProfile) {
        if !self.active {
            self.native_profile = matches!(
                profile,
                GamepadProfile::Ds4 | GamepadProfile::Ds5 | GamepadProfile::Switch
            );
        }
        self.companion.set_gamepad_profile(profile);
        self.native.set_gamepad_profile(profile);
    }
    fn set_keybindings(&mut self, bindings: &KeyBindings) {
        if !self.active {
            self.native_keybindings = (u8::MIN..=u8::MAX).any(|code| {
                bindings.resolve(code, false) != KeyBindings::empty().resolve(code, false)
            });
        }
        self.companion.set_keybindings(bindings);
        self.native.set_keybindings(bindings);
    }
    fn start(&mut self) -> InputResult<()> {
        // Pin even a partially failed acquisition until stop confirms teardown.
        self.native_pen_route = self.native.pen_touch_supported();
        self.active = true;
        self.selected_mut().start()
    }
    fn stop(&mut self) -> InputResult<()> {
        let primary = self.selected_mut().stop();
        // Pen/touch devices are independently owned even for companion keyboard
        // sessions. Always attempt their cleanup, including partial startup.
        let native_pen = if self.requires_native() {
            Ok(())
        } else {
            self.native.stop()
        };
        primary?;
        native_pen?;
        self.active = false;
        self.native_pen_route = false;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn router() -> LinuxBackendRouter {
        LinuxBackendRouter::new(
            "/nonexistent/amaterasu-companion.sock".into(),
            "/nonexistent/uinput".into(),
        )
    }
    #[test]
    fn standard_input_uses_companion_and_native_profiles_select_before_acquisition() {
        let mut backend = router();
        backend.set_keybindings(&KeyBindings::default());
        assert!(!backend.requires_native());
        backend.set_keybindings(&KeyBindings::empty());
        assert!(!backend.requires_native());
        for profile in [
            GamepadProfile::Ds4,
            GamepadProfile::Ds5,
            GamepadProfile::Switch,
        ] {
            backend.set_gamepad_profile(profile);
            assert!(backend.requires_native());
        }
        backend.set_gamepad_profile(GamepadProfile::Xone);
        assert!(!backend.requires_native());
    }
    #[test]
    fn controller_initialization_keeps_default_keyboard_policy_on_companion() {
        let mut controller = crate::controller::Controller::new(router(), true);
        let response = controller.handle(
            "test",
            crate::model::Operation::Status {},
            Instant::now(),
            &mut || Ok(()),
        );
        assert_eq!(response["backend"], "linux_uinput_companion_touch_as_mouse");
    }
    #[test]
    fn ordinary_profile_native_pen_route_stays_separate_when_device_permission_is_lost() {
        let mut backend = router();
        backend.set_gamepad_profile(GamepadProfile::Auto);
        backend.set_keybindings(&KeyBindings::default());
        // Model a successful pre-acquisition native capability probe followed
        // by loss of access before the first native report. No device is opened.
        backend.active = true;
        backend.native_pen_route = true;
        assert!(!backend.requires_native());
        assert_eq!(backend.name(), "linux_uinput_companion_touch_as_mouse");
        assert!(backend.pen_touch_supported());
        assert!(backend.supports_pen_touch_event(PenTouchDevice::Pen));
        assert_eq!(backend.touch_as_mouse_max_contacts(), None);
        let down: Event = serde_json::from_value(serde_json::json!({
            "kind":"pen_touch","pointer_id":1,"device":"pen","phase":"down","x":10,"y":20
        }))
        .unwrap();
        let error = backend.inject(&down).unwrap_err();
        assert_eq!(
            error.code, "injection_failed",
            "native error must not become companion replay"
        );
        assert!(backend.native_pen_route);
        assert_eq!(backend.name(), "linux_uinput_companion_touch_as_mouse");
        backend.stop().unwrap();
        assert!(!backend.active);
        assert!(!backend.native_pen_route);
    }
    #[test]
    fn companion_acquisition_failure_never_switches_or_replays_through_native() {
        let mut backend = router();
        assert!(backend.start().is_err());
        assert!(!backend.requires_native());
        backend.set_gamepad_profile(GamepadProfile::Ds5);
        assert!(
            !backend.requires_native(),
            "failed acquisition remains pinned until cleanup"
        );
        backend.stop().unwrap();
        backend.set_gamepad_profile(GamepadProfile::Ds5);
        assert!(backend.requires_native());
    }
}
