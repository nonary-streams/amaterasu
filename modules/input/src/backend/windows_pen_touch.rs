use crate::model::{Event, InputError, InputResult, PenTouchDevice, PenTouchPhase};
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows_sys::Win32::UI::Controls::{
    HSYNTHETICPOINTERDEVICE, POINTER_FEEDBACK_DEFAULT, POINTER_TYPE_INFO, POINTER_TYPE_INFO_0,
};
use windows_sys::Win32::UI::Input::Pointer::{
    POINTER_FLAG_CANCELED, POINTER_FLAG_DOWN, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE,
    POINTER_FLAG_UP, POINTER_FLAG_UPDATE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, PEN_FLAG_NONE, PEN_MASK_NONE, PEN_MASK_PRESSURE, PEN_MASK_TILT_X,
    PEN_MASK_TILT_Y, PT_PEN, PT_TOUCH, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, TOUCH_MASK_NONE,
    TOUCH_MASK_PRESSURE,
};

const MAX_PEN_CONTACTS: usize = 1;
const MAX_TOUCH_CONTACTS: usize = 10;
const PEN_MAX_CONTACTS: u32 = MAX_PEN_CONTACTS as u32;
const TOUCH_MAX_CONTACTS: u32 = MAX_TOUCH_CONTACTS as u32;
const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(50);
const PRESSURE_SCALE: f32 = 1024.0;

type CreateSyntheticPointerDevice =
    unsafe extern "system" fn(i32, u32, i32) -> HSYNTHETICPOINTERDEVICE;
type InjectSyntheticPointerInput =
    unsafe extern "system" fn(HSYNTHETICPOINTERDEVICE, *const POINTER_TYPE_INFO, u32) -> i32;
type DestroySyntheticPointerDevice = unsafe extern "system" fn(HSYNTHETICPOINTERDEVICE);

type SyntheticPointerHandle = HSYNTHETICPOINTERDEVICE;

/// Owns synthetic Windows pointer devices and the client pointer IDs active on
/// them. The OS auto-cancels synthetic contacts after a period without updates,
/// so `advance` refreshes each active contact on the reference's 50 ms cadence.
pub(super) struct WindowsPenTouch {
    api: Box<dyn SyntheticPointerApi>,
    pen: Option<PointerDeviceState>,
    touch: Option<PointerDeviceState>,
    next_keepalive: Option<Instant>,
}

impl WindowsPenTouch {
    pub(super) fn new() -> Self {
        Self::with_api(Box::new(NativeSyntheticPointerApi::load()))
    }

    fn with_api(api: Box<dyn SyntheticPointerApi>) -> Self {
        Self {
            api,
            pen: None,
            touch: None,
            next_keepalive: None,
        }
    }

    pub(super) fn supported(&self) -> bool {
        self.api.supported()
    }

    pub(super) fn max_contacts(&self, device: PenTouchDevice) -> usize {
        match device {
            PenTouchDevice::Pen => MAX_PEN_CONTACTS,
            PenTouchDevice::Touch => MAX_TOUCH_CONTACTS,
        }
    }

    pub(super) fn validate(&self, event: &Event) -> InputResult<()> {
        event.validate()?;
        if !matches!(event, Event::PenTouch { .. }) {
            return Ok(());
        }

        if self.api.supported() {
            Ok(())
        } else {
            Err(InputError::new(
                "unsupported_platform",
                "native pen/touch injection requires Windows 10 version 1809 or later",
            ))
        }
    }

    fn validate_lifecycle(&self, event: &Event) -> InputResult<()> {
        let Event::PenTouch {
            pointer_id,
            device,
            phase,
            ..
        } = event
        else {
            return Ok(());
        };

        let state = self.device_state(*device);
        let pointer_is_active = state.is_some_and(|state| state.contacts.contains_key(pointer_id));
        match phase {
            PenTouchPhase::Down if pointer_is_active => Err(InputError::new(
                "duplicate_pointer_down",
                "pointer is already active",
            )),
            PenTouchPhase::Down
                if *device == PenTouchDevice::Pen
                    && state.is_some_and(|state| !state.contacts.is_empty()) =>
            {
                Err(InputError::new(
                    "pen_pointer_busy",
                    "Windows supports one active synthetic pen pointer",
                ))
            }
            PenTouchPhase::Down
                if *device == PenTouchDevice::Touch
                    && state.is_some_and(|state| state.contacts.len() >= MAX_TOUCH_CONTACTS) =>
            {
                Err(InputError::new(
                    "touch_pointer_limit",
                    format!("Windows supports at most {MAX_TOUCH_CONTACTS} active touch pointers"),
                ))
            }
            PenTouchPhase::Move | PenTouchPhase::Up if !pointer_is_active => Err(InputError::new(
                "unknown_pointer",
                "pointer transition requires a matching active pointer ID",
            )),
            _ => Ok(()),
        }
    }

    pub(super) fn start(&mut self) -> InputResult<()> {
        self.stop()
    }

    pub(super) fn inject(&mut self, event: &Event) -> InputResult<()> {
        self.validate(event)?;
        self.validate_lifecycle(event)?;

        let Event::PenTouch {
            pointer_id,
            device,
            phase,
            x,
            y,
            pressure,
            tilt_x,
            tilt_y,
        } = event
        else {
            return Ok(());
        };

        if *phase == PenTouchPhase::Cancel
            && !self
                .device_state(*device)
                .is_some_and(|state| state.contacts.contains_key(pointer_id))
        {
            // Cleanup can cancel a down whose native setup failed before the
            // backend owned a device/contact. Ignore only this exact ID; do not
            // create a device or emit a cancel that could affect another ID.
            return Ok(());
        }

        let virtual_desktop = self.api.virtual_desktop()?;
        if self.device_state(*device).is_none() {
            let handle = self.api.create_device(*device)?;
            *self.device_state_slot_mut(*device) = Some(PointerDeviceState {
                handle,
                contacts: BTreeMap::new(),
            });
        }
        let contact = PointerContact {
            x: *x,
            y: *y,
            pressure: *pressure,
            tilt_x: *tilt_x,
            tilt_y: *tilt_y,
        };
        let frame = {
            let state_slot = self.device_state_slot_mut(*device);
            let state = state_slot
                .as_mut()
                .expect("pointer device state was created above");
            let previous = state.contacts.get(pointer_id).copied();
            state.contacts.insert(*pointer_id, contact);
            let frame = build_pointer_frame(
                *device,
                &state.contacts,
                Some((*pointer_id, *phase)),
                virtual_desktop,
            )?;
            (state.handle, previous, frame)
        };

        let (handle, previous_contact, records) = frame;
        if let Err(error) = self.api.inject(handle, &records) {
            // A failed down may still have reached Windows, so retain it for an
            // explicit cancel/retry. Failed move/up/cancel calls restore the
            // last accepted contact data and never release another pointer.
            if *phase != PenTouchPhase::Down
                && let Some(state) = self.device_state_mut(*device).as_mut()
                && let Some(previous_contact) = previous_contact
            {
                state.contacts.insert(*pointer_id, previous_contact);
            }
            self.update_keepalive_deadline(Instant::now());
            return Err(error);
        }

        if matches!(phase, PenTouchPhase::Up | PenTouchPhase::Cancel)
            && let Some(state) = self.device_state_mut(*device).as_mut()
        {
            state.contacts.remove(pointer_id);
        }
        self.update_keepalive_deadline(Instant::now());
        Ok(())
    }

    pub(super) fn advance(&mut self, now: Instant) -> InputResult<()> {
        if !self.has_active_contacts()
            || !self.next_keepalive.is_some_and(|deadline| now >= deadline)
        {
            return Ok(());
        }

        let result = self.refresh_active_contacts();
        self.update_keepalive_deadline(now);
        result
    }

    pub(super) fn stop(&mut self) -> InputResult<()> {
        let mut first_error = None;
        for device in [PenTouchDevice::Pen, PenTouchDevice::Touch] {
            let pointer_ids = self
                .device_state(device)
                .map(|state| state.contacts.keys().copied().collect::<Vec<_>>())
                .unwrap_or_default();
            for pointer_id in pointer_ids {
                if let Err(error) = self.cancel_pointer(device, pointer_id) {
                    first_error.get_or_insert(error);
                }
            }
        }

        for device in [PenTouchDevice::Pen, PenTouchDevice::Touch] {
            if let Some(state) = self.device_state_slot_mut(device).take() {
                self.api.destroy_device(state.handle);
            }
        }
        self.next_keepalive = None;

        first_error.map_or(Ok(()), Err)
    }

    fn cancel_pointer(&mut self, device: PenTouchDevice, pointer_id: u32) -> InputResult<()> {
        let contact = self
            .device_state(device)
            .and_then(|state| state.contacts.get(&pointer_id))
            .copied()
            .ok_or_else(|| InputError::new("unknown_pointer", "pointer is no longer active"))?;
        let virtual_desktop = self.api.virtual_desktop()?;
        let (handle, records) = {
            let state = self
                .device_state(device)
                .expect("active pointer has an owned device");
            let mut contacts = state.contacts.clone();
            contacts.insert(pointer_id, contact);
            (
                state.handle,
                build_pointer_frame(
                    device,
                    &contacts,
                    Some((pointer_id, PenTouchPhase::Cancel)),
                    virtual_desktop,
                )?,
            )
        };
        self.api.inject(handle, &records)?;
        if let Some(state) = self.device_state_mut(device).as_mut() {
            state.contacts.remove(&pointer_id);
        }
        Ok(())
    }

    fn refresh_active_contacts(&mut self) -> InputResult<()> {
        let virtual_desktop = self.api.virtual_desktop()?;
        for device in [PenTouchDevice::Pen, PenTouchDevice::Touch] {
            let Some(state) = self.device_state(device) else {
                continue;
            };
            if state.contacts.is_empty() {
                continue;
            }
            let records = build_pointer_frame(device, &state.contacts, None, virtual_desktop)?;
            self.api.inject(state.handle, &records)?;
        }
        Ok(())
    }

    fn device_state(&self, device: PenTouchDevice) -> Option<&PointerDeviceState> {
        match device {
            PenTouchDevice::Pen => self.pen.as_ref(),
            PenTouchDevice::Touch => self.touch.as_ref(),
        }
    }

    fn device_state_slot_mut(&mut self, device: PenTouchDevice) -> &mut Option<PointerDeviceState> {
        match device {
            PenTouchDevice::Pen => &mut self.pen,
            PenTouchDevice::Touch => &mut self.touch,
        }
    }

    fn device_state_mut(&mut self, device: PenTouchDevice) -> &mut Option<PointerDeviceState> {
        self.device_state_slot_mut(device)
    }

    fn has_active_contacts(&self) -> bool {
        [self.pen.as_ref(), self.touch.as_ref()]
            .into_iter()
            .flatten()
            .any(|state| !state.contacts.is_empty())
    }

    fn update_keepalive_deadline(&mut self, now: Instant) {
        self.next_keepalive = self
            .has_active_contacts()
            .then_some(now + KEEPALIVE_INTERVAL);
    }
}

impl Drop for WindowsPenTouch {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct PointerDeviceState {
    handle: SyntheticPointerHandle,
    contacts: BTreeMap<u32, PointerContact>,
}

#[derive(Clone, Copy)]
struct PointerContact {
    x: u16,
    y: u16,
    pressure: Option<f32>,
    tilt_x: Option<i16>,
    tilt_y: Option<i16>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VirtualDesktop {
    width: u32,
    height: u32,
}

impl VirtualDesktop {
    fn new(width: i32, height: i32) -> InputResult<Self> {
        if width <= 0 || height <= 0 {
            return Err(InputError::new(
                "desktop_unavailable",
                "Windows reported an invalid virtual desktop size",
            ));
        }
        Ok(Self {
            width: width as u32,
            height: height as u32,
        })
    }

    fn map(&self, x: u16, y: u16) -> InputResult<(i32, i32)> {
        Ok((
            map_normalized_coordinate(x, self.width)?,
            map_normalized_coordinate(y, self.height)?,
        ))
    }
}

fn map_normalized_coordinate(value: u16, extent: u32) -> InputResult<i32> {
    if extent == 0 {
        return Err(InputError::new(
            "desktop_unavailable",
            "Windows reported an empty virtual desktop extent",
        ));
    }
    let maximum = u64::from(extent - 1);
    let scaled = u64::from(value)
        .checked_mul(maximum)
        .and_then(|coordinate| coordinate.checked_add(u64::from(u16::MAX) / 2))
        .ok_or_else(|| {
            InputError::new(
                "invalid_event",
                "normalized desktop coordinate overflowed during scaling",
            )
        })?
        / u64::from(u16::MAX);
    i32::try_from(scaled).map_err(|_| {
        InputError::new(
            "desktop_unavailable",
            "Windows virtual desktop coordinate exceeds pointer API limits",
        )
    })
}

fn build_pointer_frame(
    device: PenTouchDevice,
    contacts: &BTreeMap<u32, PointerContact>,
    transition: Option<(u32, PenTouchPhase)>,
    virtual_desktop: VirtualDesktop,
) -> InputResult<Vec<POINTER_TYPE_INFO>> {
    contacts
        .iter()
        .map(|(&pointer_id, contact)| {
            let phase = transition
                .filter(|(transition_id, _)| *transition_id == pointer_id)
                .map_or(PenTouchPhase::Move, |(_, phase)| phase);
            pointer_info_for_contact(device, pointer_id, phase, *contact, virtual_desktop)
        })
        .collect()
}

fn pointer_info_for_contact(
    device: PenTouchDevice,
    pointer_id: u32,
    phase: PenTouchPhase,
    contact: PointerContact,
    virtual_desktop: VirtualDesktop,
) -> InputResult<POINTER_TYPE_INFO> {
    let (x, y) = virtual_desktop.map(contact.x, contact.y)?;
    let pointer_flags = match phase {
        PenTouchPhase::Down => POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_DOWN,
        PenTouchPhase::Move => POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_UPDATE,
        PenTouchPhase::Up => POINTER_FLAG_UP,
        PenTouchPhase::Cancel => POINTER_FLAG_UP | POINTER_FLAG_CANCELED,
    };

    let mut pointer = POINTER_TYPE_INFO::default();
    match device {
        PenTouchDevice::Pen => {
            let mut pen_info = windows_sys::Win32::UI::Input::Pointer::POINTER_PEN_INFO::default();
            pen_info.pointerInfo.pointerType = PT_PEN;
            pen_info.pointerInfo.pointerId = pointer_id;
            pen_info.pointerInfo.pointerFlags = pointer_flags;
            pen_info.pointerInfo.ptPixelLocation.x = x;
            pen_info.pointerInfo.ptPixelLocation.y = y;
            pen_info.penFlags = PEN_FLAG_NONE;
            pen_info.penMask = PEN_MASK_NONE;

            if matches!(phase, PenTouchPhase::Down | PenTouchPhase::Move)
                && let Some(pressure) = contact.pressure
            {
                pen_info.penMask |= PEN_MASK_PRESSURE;
                pen_info.pressure = pressure_to_native(pressure);
            }
            if let (Some(tilt_x), Some(tilt_y)) = (contact.tilt_x, contact.tilt_y) {
                pen_info.penMask |= PEN_MASK_TILT_X | PEN_MASK_TILT_Y;
                pen_info.tiltX = i32::from(tilt_x);
                pen_info.tiltY = i32::from(tilt_y);
            }

            pointer.r#type = PT_PEN;
            pointer.Anonymous = POINTER_TYPE_INFO_0 { penInfo: pen_info };
        }
        PenTouchDevice::Touch => {
            let mut touch_info =
                windows_sys::Win32::UI::Input::Pointer::POINTER_TOUCH_INFO::default();
            touch_info.pointerInfo.pointerType = PT_TOUCH;
            touch_info.pointerInfo.pointerId = pointer_id;
            touch_info.pointerInfo.pointerFlags = pointer_flags;
            touch_info.pointerInfo.ptPixelLocation.x = x;
            touch_info.pointerInfo.ptPixelLocation.y = y;
            touch_info.touchMask = TOUCH_MASK_NONE;
            touch_info.pressure = 512;
            if matches!(phase, PenTouchPhase::Down | PenTouchPhase::Move)
                && let Some(pressure) = contact.pressure
            {
                touch_info.touchMask |= TOUCH_MASK_PRESSURE;
                touch_info.pressure = pressure_to_native(pressure);
            }

            pointer.r#type = PT_TOUCH;
            pointer.Anonymous = POINTER_TYPE_INFO_0 {
                touchInfo: touch_info,
            };
        }
    }
    Ok(pointer)
}

fn pressure_to_native(pressure: f32) -> u32 {
    (pressure * PRESSURE_SCALE).round() as u32
}

trait SyntheticPointerApi {
    fn supported(&self) -> bool;
    fn virtual_desktop(&self) -> InputResult<VirtualDesktop>;
    fn create_device(&mut self, device: PenTouchDevice) -> InputResult<SyntheticPointerHandle>;
    fn inject(
        &mut self,
        handle: SyntheticPointerHandle,
        pointers: &[POINTER_TYPE_INFO],
    ) -> InputResult<()>;
    fn destroy_device(&mut self, handle: SyntheticPointerHandle);
}

struct NativeSyntheticPointerApi {
    create: Option<CreateSyntheticPointerDevice>,
    inject: Option<InjectSyntheticPointerInput>,
    destroy: Option<DestroySyntheticPointerDevice>,
}

impl NativeSyntheticPointerApi {
    fn load() -> Self {
        // GetModuleHandleA borrows user32.dll without changing its reference
        // count. A desktop Windows process keeps user32 loaded for its lifetime.
        let module = unsafe { GetModuleHandleA(c"user32.dll".as_ptr().cast()) };
        if module.is_null() {
            return Self {
                create: None,
                inject: None,
                destroy: None,
            };
        }

        // SAFETY: each resolved export is a documented Win10 1809 user32
        // function and is transmuted only to its exact ABI signature below.
        unsafe {
            Self {
                create: load_export(module, c"CreateSyntheticPointerDevice").map(|address| {
                    // SAFETY: the export has this exact documented ABI.
                    std::mem::transmute::<*const (), CreateSyntheticPointerDevice>(address)
                }),
                inject: load_export(module, c"InjectSyntheticPointerInput").map(|address| {
                    // SAFETY: the export has this exact documented ABI.
                    std::mem::transmute::<*const (), InjectSyntheticPointerInput>(address)
                }),
                destroy: load_export(module, c"DestroySyntheticPointerDevice").map(|address| {
                    // SAFETY: the export has this exact documented ABI.
                    std::mem::transmute::<*const (), DestroySyntheticPointerDevice>(address)
                }),
            }
        }
    }
}

fn load_export(module: windows_sys::Win32::Foundation::HMODULE, name: &CStr) -> Option<*const ()> {
    // SAFETY: module is the live user32 module and CStr guarantees NUL termination.
    let export = unsafe { GetProcAddress(module, name.as_ptr().cast()) }?;
    Some(export as *const ())
}

impl SyntheticPointerApi for NativeSyntheticPointerApi {
    fn supported(&self) -> bool {
        self.create.is_some() && self.inject.is_some() && self.destroy.is_some()
    }

    fn virtual_desktop(&self) -> InputResult<VirtualDesktop> {
        // InjectSyntheticPointerInput expects coordinates relative to the
        // virtual desktop's top-left, so only its full width and height apply.
        let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        VirtualDesktop::new(width, height)
    }

    fn create_device(&mut self, device: PenTouchDevice) -> InputResult<SyntheticPointerHandle> {
        if !self.supported() {
            return Err(unsupported_api_error());
        }
        let (pointer_type, max_count) = match device {
            PenTouchDevice::Pen => (PT_PEN, PEN_MAX_CONTACTS),
            PenTouchDevice::Touch => (PT_TOUCH, TOUCH_MAX_CONTACTS),
        };
        // SAFETY: function was resolved from user32 with its documented ABI.
        let handle = unsafe {
            self.create.expect("support checked above")(
                pointer_type,
                max_count,
                POINTER_FEEDBACK_DEFAULT,
            )
        };
        if handle.is_null() {
            return Err(InputError::new(
                "device_creation_failed",
                format!(
                    "CreateSyntheticPointerDevice failed: {}",
                    std::io::Error::from_raw_os_error(unsafe { GetLastError() as i32 })
                ),
            ));
        }
        Ok(handle)
    }

    fn inject(
        &mut self,
        handle: SyntheticPointerHandle,
        pointers: &[POINTER_TYPE_INFO],
    ) -> InputResult<()> {
        let inject = self.inject.ok_or_else(unsupported_api_error)?;
        let count = u32::try_from(pointers.len()).map_err(|_| {
            InputError::new("invalid_event", "pointer frame exceeds Windows API limits")
        })?;
        // SAFETY: pointers is a fully initialized owned slice whose union tags
        // match the device. The Win32 call reads it synchronously and retains no pointer.
        let accepted = unsafe { inject(handle, pointers.as_ptr(), count) };
        if accepted == 0 {
            return Err(InputError::new(
                "injection_failed",
                format!(
                    "InjectSyntheticPointerInput failed: {}",
                    std::io::Error::from_raw_os_error(unsafe { GetLastError() as i32 })
                ),
            ));
        }
        Ok(())
    }

    fn destroy_device(&mut self, handle: SyntheticPointerHandle) {
        if let Some(destroy) = self.destroy {
            // SAFETY: this handle was created by the matching user32 API and
            // is consumed once by WindowsPenTouch when its owned session ends.
            unsafe { destroy(handle) };
        }
    }
}

fn unsupported_api_error() -> InputError {
    InputError::new(
        "unsupported_platform",
        "native pen/touch injection requires Windows 10 version 1809 or later",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct FakeState {
        supported: bool,
        desktop: Option<VirtualDesktop>,
        created: Vec<(PenTouchDevice, usize, u32)>,
        frames: Vec<(PenTouchDevice, Vec<POINTER_TYPE_INFO>)>,
        destroyed: Vec<usize>,
        fail_next_injection: bool,
        fail_next_device_creation: bool,
    }

    struct FakePointerApi {
        state: Rc<RefCell<FakeState>>,
    }

    impl SyntheticPointerApi for FakePointerApi {
        fn supported(&self) -> bool {
            self.state.borrow().supported
        }

        fn virtual_desktop(&self) -> InputResult<VirtualDesktop> {
            self.state.borrow().desktop.ok_or_else(|| {
                InputError::new("desktop_unavailable", "fake desktop was not configured")
            })
        }

        fn create_device(&mut self, device: PenTouchDevice) -> InputResult<SyntheticPointerHandle> {
            let mut state = self.state.borrow_mut();
            if state.fail_next_device_creation {
                state.fail_next_device_creation = false;
                return Err(InputError::new(
                    "device_creation_failed",
                    "simulated synthetic-pointer device creation failure",
                ));
            }
            let handle_id = state.created.len() + 1;
            let max_count = match device {
                PenTouchDevice::Pen => PEN_MAX_CONTACTS,
                PenTouchDevice::Touch => TOUCH_MAX_CONTACTS,
            };
            state.created.push((device, handle_id, max_count));
            Ok(handle_id as SyntheticPointerHandle)
        }

        fn inject(
            &mut self,
            handle: SyntheticPointerHandle,
            pointers: &[POINTER_TYPE_INFO],
        ) -> InputResult<()> {
            let mut state = self.state.borrow_mut();
            if state.fail_next_injection {
                state.fail_next_injection = false;
                return Err(InputError::new("injection_failed", "simulated device loss"));
            }
            let handle_id = handle as usize;
            let device = state
                .created
                .iter()
                .find(|(_, created_handle, _)| *created_handle == handle_id)
                .map(|(device, _, _)| *device)
                .expect("fake inject uses a created device");
            state.frames.push((device, pointers.to_vec()));
            Ok(())
        }

        fn destroy_device(&mut self, handle: SyntheticPointerHandle) {
            self.state.borrow_mut().destroyed.push(handle as usize);
        }
    }

    fn backend_with_support(supported: bool) -> (WindowsPenTouch, Rc<RefCell<FakeState>>) {
        let state = Rc::new(RefCell::new(FakeState {
            supported,
            desktop: Some(VirtualDesktop {
                width: 1920,
                height: 1080,
            }),
            ..FakeState::default()
        }));
        let api = FakePointerApi {
            state: Rc::clone(&state),
        };
        (WindowsPenTouch::with_api(Box::new(api)), state)
    }

    fn backend() -> (WindowsPenTouch, Rc<RefCell<FakeState>>) {
        backend_with_support(true)
    }

    fn event(
        pointer_id: u32,
        device: PenTouchDevice,
        phase: PenTouchPhase,
        x: u16,
        y: u16,
    ) -> Event {
        Event::PenTouch {
            pointer_id,
            device,
            phase,
            x,
            y,
            pressure: None,
            tilt_x: None,
            tilt_y: None,
        }
    }

    fn pen_info(
        pointer: &POINTER_TYPE_INFO,
    ) -> windows_sys::Win32::UI::Input::Pointer::POINTER_PEN_INFO {
        assert_eq!(pointer.r#type, PT_PEN);
        // SAFETY: the helper checks the POINTER_TYPE_INFO tag before reading the pen union arm.
        unsafe { pointer.Anonymous.penInfo }
    }

    fn touch_info(
        pointer: &POINTER_TYPE_INFO,
    ) -> windows_sys::Win32::UI::Input::Pointer::POINTER_TOUCH_INFO {
        assert_eq!(pointer.r#type, PT_TOUCH);
        // SAFETY: the helper checks the POINTER_TYPE_INFO tag before reading the touch union arm.
        unsafe { pointer.Anonymous.touchInfo }
    }

    #[test]
    fn pen_events_keep_pointer_identity_position_pressure_and_tilt() {
        let (mut backend, state) = backend();
        let mut down = event(73, PenTouchDevice::Pen, PenTouchPhase::Down, u16::MAX, 0);
        if let Event::PenTouch {
            pressure,
            tilt_x,
            tilt_y,
            ..
        } = &mut down
        {
            *pressure = Some(0.5);
            *tilt_x = Some(25);
            *tilt_y = Some(-35);
        }
        backend.inject(&down).unwrap();

        let mut move_event = event(73, PenTouchDevice::Pen, PenTouchPhase::Move, 0, u16::MAX);
        if let Event::PenTouch { pressure, .. } = &mut move_event {
            *pressure = Some(1.0);
        }
        backend.inject(&move_event).unwrap();
        backend
            .inject(&event(73, PenTouchDevice::Pen, PenTouchPhase::Up, 100, 200))
            .unwrap();

        let state = state.borrow();
        assert_eq!(state.created, vec![(PenTouchDevice::Pen, 1, 1)]);
        assert_eq!(state.frames.len(), 3);
        let down = pen_info(&state.frames[0].1[0]);
        assert_eq!(down.pointerInfo.pointerId, 73);
        assert_eq!(down.pointerInfo.ptPixelLocation.x, 1919);
        assert_eq!(down.pointerInfo.ptPixelLocation.y, 0);
        assert_eq!(down.pressure, 512);
        assert_eq!(down.tiltX, 25);
        assert_eq!(down.tiltY, -35);
        assert_eq!(
            down.penMask,
            PEN_MASK_PRESSURE | PEN_MASK_TILT_X | PEN_MASK_TILT_Y
        );
        assert_eq!(
            down.pointerInfo.pointerFlags & POINTER_FLAG_DOWN,
            POINTER_FLAG_DOWN
        );

        let moved = pen_info(&state.frames[1].1[0]);
        assert_eq!(moved.pointerInfo.pointerId, 73);
        assert_eq!(moved.pointerInfo.ptPixelLocation.x, 0);
        assert_eq!(moved.pointerInfo.ptPixelLocation.y, 1079);
        assert_eq!(moved.pressure, 1024);
        assert_eq!(
            moved.pointerInfo.pointerFlags & POINTER_FLAG_UPDATE,
            POINTER_FLAG_UPDATE
        );

        let up = pen_info(&state.frames[2].1[0]);
        assert_eq!(up.pointerInfo.pointerId, 73);
        assert_eq!(up.pointerInfo.pointerFlags, POINTER_FLAG_UP);
        assert!(backend.pen.as_ref().unwrap().contacts.is_empty());
    }

    #[test]
    fn touch_contacts_are_batched_and_cancel_only_the_requested_pointer() {
        let (mut backend, state) = backend();
        backend
            .inject(&event(
                11,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                1000,
                2000,
            ))
            .unwrap();
        backend
            .inject(&event(
                22,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                3000,
                4000,
            ))
            .unwrap();
        backend
            .inject(&event(
                11,
                PenTouchDevice::Touch,
                PenTouchPhase::Cancel,
                1100,
                2100,
            ))
            .unwrap();

        let state = state.borrow();
        assert_eq!(state.created, vec![(PenTouchDevice::Touch, 1, 10)]);
        let cancel_frame = &state.frames[2].1;
        assert_eq!(cancel_frame.len(), 2);
        let canceled = touch_info(&cancel_frame[0]);
        let still_active = touch_info(&cancel_frame[1]);
        assert_eq!(canceled.pointerInfo.pointerId, 11);
        assert_eq!(
            canceled.pointerInfo.pointerFlags,
            POINTER_FLAG_UP | POINTER_FLAG_CANCELED
        );
        assert_eq!(still_active.pointerInfo.pointerId, 22);
        assert_eq!(
            still_active.pointerInfo.pointerFlags & (POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT),
            POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT
        );
        assert_eq!(still_active.touchMask, TOUCH_MASK_NONE);
        assert_eq!(still_active.pressure, 512);
        assert_eq!(backend.touch.as_ref().unwrap().contacts.len(), 1);
        assert!(backend.touch.as_ref().unwrap().contacts.contains_key(&22));
    }

    #[test]
    fn duplicate_and_unknown_lifecycle_events_do_not_reach_the_native_api() {
        let (mut backend, state) = backend();
        let down = event(5, PenTouchDevice::Touch, PenTouchPhase::Down, 1, 2);
        backend.inject(&down).unwrap();
        assert_eq!(
            backend.inject(&down).unwrap_err().code,
            "duplicate_pointer_down"
        );
        for phase in [PenTouchPhase::Move, PenTouchPhase::Up] {
            assert_eq!(
                backend
                    .inject(&event(6, PenTouchDevice::Touch, phase, 1, 2))
                    .unwrap_err()
                    .code,
                "unknown_pointer"
            );
        }
        let frame_count = state.borrow().frames.len();
        backend
            .inject(&event(
                6,
                PenTouchDevice::Touch,
                PenTouchPhase::Cancel,
                1,
                2,
            ))
            .unwrap();
        assert_eq!(state.borrow().frames.len(), frame_count);
        assert!(backend.touch.as_ref().unwrap().contacts.contains_key(&5));
        backend
            .inject(&event(5, PenTouchDevice::Touch, PenTouchPhase::Up, 1, 2))
            .unwrap();
        assert_eq!(
            backend
                .inject(&event(5, PenTouchDevice::Touch, PenTouchPhase::Up, 1, 2))
                .unwrap_err()
                .code,
            "unknown_pointer"
        );
        backend
            .inject(&event(
                5,
                PenTouchDevice::Touch,
                PenTouchPhase::Cancel,
                1,
                2,
            ))
            .unwrap();
        assert_eq!(state.borrow().frames.len(), 2);
    }

    #[test]
    fn cleanup_cancel_after_virtual_desktop_failure_is_a_noop() {
        let (mut backend, state) = backend();
        state.borrow_mut().desktop = None;
        let down = event(5, PenTouchDevice::Touch, PenTouchPhase::Down, 1, 2);
        assert_eq!(
            backend.inject(&down).unwrap_err().code,
            "desktop_unavailable"
        );

        backend
            .inject(&event(
                5,
                PenTouchDevice::Touch,
                PenTouchPhase::Cancel,
                1,
                2,
            ))
            .unwrap();

        let state = state.borrow();
        assert!(state.created.is_empty());
        assert!(state.frames.is_empty());
    }

    #[test]
    fn cleanup_cancel_after_device_creation_failure_is_a_noop() {
        let (mut backend, state) = backend();
        state.borrow_mut().fail_next_device_creation = true;
        let down = event(5, PenTouchDevice::Touch, PenTouchPhase::Down, 1, 2);
        assert_eq!(
            backend.inject(&down).unwrap_err().code,
            "device_creation_failed"
        );

        backend
            .inject(&event(
                5,
                PenTouchDevice::Touch,
                PenTouchPhase::Cancel,
                1,
                2,
            ))
            .unwrap();

        let state = state.borrow();
        assert!(state.created.is_empty());
        assert!(state.frames.is_empty());
    }

    #[test]
    fn batch_preflight_accepts_lifecycle_transitions_before_injection() {
        let (backend, state) = backend();
        for phase in [PenTouchPhase::Down, PenTouchPhase::Move, PenTouchPhase::Up] {
            backend
                .validate(&event(5, PenTouchDevice::Touch, phase, 1, 2))
                .unwrap();
        }
        assert!(state.borrow().frames.is_empty());
        assert!(state.borrow().created.is_empty());
    }

    #[test]
    fn malformed_pressure_and_touch_tilt_are_rejected_before_device_creation() {
        let (mut backend, state) = backend();
        let mut invalid_pressure = event(1, PenTouchDevice::Pen, PenTouchPhase::Down, 0, 0);
        if let Event::PenTouch { pressure, .. } = &mut invalid_pressure {
            *pressure = Some(f32::NAN);
        }
        assert_eq!(
            backend.inject(&invalid_pressure).unwrap_err().code,
            "invalid_event"
        );

        let mut invalid_tilt = event(2, PenTouchDevice::Touch, PenTouchPhase::Down, 0, 0);
        if let Event::PenTouch { tilt_x, .. } = &mut invalid_tilt {
            *tilt_x = Some(1);
        }
        assert_eq!(
            backend.inject(&invalid_tilt).unwrap_err().code,
            "invalid_event"
        );
        assert!(state.borrow().created.is_empty());
    }

    #[test]
    fn unsupported_windows_versions_reject_pen_touch_without_creating_devices() {
        let (mut backend, state) = backend_with_support(false);
        assert!(!backend.supported());
        assert_eq!(
            backend
                .inject(&event(1, PenTouchDevice::Pen, PenTouchPhase::Down, 0, 0))
                .unwrap_err()
                .code,
            "unsupported_platform"
        );
        assert!(state.borrow().created.is_empty());
    }

    #[test]
    fn native_contact_capacity_matches_synthetic_device_limits() {
        let (backend, _) = backend();
        assert_eq!(backend.max_contacts(PenTouchDevice::Pen), MAX_PEN_CONTACTS);
        assert_eq!(
            backend.max_contacts(PenTouchDevice::Touch),
            MAX_TOUCH_CONTACTS
        );
    }

    #[test]
    fn touch_capacity_rejects_an_extra_contact_before_native_injection() {
        let (mut backend, state) = backend();
        for pointer_id in 1..=MAX_TOUCH_CONTACTS as u32 {
            backend
                .inject(&event(
                    pointer_id,
                    PenTouchDevice::Touch,
                    PenTouchPhase::Down,
                    pointer_id as u16,
                    0,
                ))
                .unwrap();
        }

        assert_eq!(
            backend
                .inject(&event(
                    MAX_TOUCH_CONTACTS as u32 + 1,
                    PenTouchDevice::Touch,
                    PenTouchPhase::Down,
                    0,
                    0,
                ))
                .unwrap_err()
                .code,
            "touch_pointer_limit"
        );
        assert_eq!(state.borrow().frames.len(), MAX_TOUCH_CONTACTS);
        let contacts = &backend.touch.as_ref().unwrap().contacts;
        assert_eq!(contacts.len(), MAX_TOUCH_CONTACTS);
        assert!(!contacts.contains_key(&(MAX_TOUCH_CONTACTS as u32 + 1)));
    }

    #[test]
    fn device_loss_preserves_uncertain_downs_and_never_cancels_another_pointer() {
        let (mut backend, state) = backend();
        backend
            .inject(&event(
                20,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                300,
                400,
            ))
            .unwrap();
        state.borrow_mut().fail_next_injection = true;
        assert_eq!(
            backend
                .inject(&event(
                    10,
                    PenTouchDevice::Touch,
                    PenTouchPhase::Down,
                    100,
                    200
                ))
                .unwrap_err()
                .code,
            "injection_failed"
        );
        assert!(backend.touch.as_ref().unwrap().contacts.contains_key(&10));
        backend
            .inject(&event(
                10,
                PenTouchDevice::Touch,
                PenTouchPhase::Cancel,
                100,
                200,
            ))
            .unwrap();
        state.borrow_mut().fail_next_injection = true;
        assert_eq!(
            backend
                .inject(&event(
                    20,
                    PenTouchDevice::Touch,
                    PenTouchPhase::Move,
                    301,
                    401
                ))
                .unwrap_err()
                .code,
            "injection_failed"
        );
        assert_eq!(backend.touch.as_ref().unwrap().contacts[&20].x, 300);
        backend
            .inject(&event(
                20,
                PenTouchDevice::Touch,
                PenTouchPhase::Cancel,
                300,
                400,
            ))
            .unwrap();

        let state = state.borrow();
        assert_eq!(state.frames.len(), 3);
        let first_cancel = &state.frames[1].1;
        assert_eq!(first_cancel.len(), 2);
        assert_eq!(touch_info(&first_cancel[0]).pointerInfo.pointerId, 10);
        assert_eq!(
            touch_info(&first_cancel[0]).pointerInfo.pointerFlags,
            POINTER_FLAG_UP | POINTER_FLAG_CANCELED
        );
        assert_eq!(touch_info(&first_cancel[1]).pointerInfo.pointerId, 20);
        assert_eq!(
            touch_info(&first_cancel[1]).pointerInfo.pointerFlags & POINTER_FLAG_CANCELED,
            0
        );
        let final_cancel = &state.frames[2].1;
        assert_eq!(final_cancel.len(), 1);
        assert_eq!(touch_info(&final_cancel[0]).pointerInfo.pointerId, 20);
        assert_eq!(
            touch_info(&final_cancel[0]).pointerInfo.pointerFlags,
            POINTER_FLAG_UP | POINTER_FLAG_CANCELED
        );
        assert!(backend.touch.as_ref().unwrap().contacts.is_empty());
    }

    #[test]
    fn active_contacts_refresh_at_fifty_milliseconds_using_saved_coordinates() {
        let (mut backend, state) = backend();
        backend
            .inject(&event(
                8,
                PenTouchDevice::Pen,
                PenTouchPhase::Down,
                32768,
                65535,
            ))
            .unwrap();
        let deadline = backend.next_keepalive.unwrap();
        backend
            .advance(deadline - Duration::from_millis(1))
            .unwrap();
        assert_eq!(state.borrow().frames.len(), 1);

        backend.advance(deadline).unwrap();
        let state = state.borrow();
        assert_eq!(state.frames.len(), 2);
        let refresh = pen_info(&state.frames[1].1[0]);
        assert_eq!(refresh.pointerInfo.pointerId, 8);
        assert_eq!(refresh.pointerInfo.ptPixelLocation.x, 960);
        assert_eq!(refresh.pointerInfo.ptPixelLocation.y, 1079);
        assert_eq!(
            refresh.pointerInfo.pointerFlags,
            POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_UPDATE
        );
    }

    #[test]
    fn shutdown_cancels_owned_pointers_and_destroys_each_device_once() {
        let (mut backend, state) = backend();
        backend
            .inject(&event(
                2,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                10,
                20,
            ))
            .unwrap();
        backend
            .inject(&event(
                1,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                30,
                40,
            ))
            .unwrap();
        backend
            .inject(&event(99, PenTouchDevice::Pen, PenTouchPhase::Down, 50, 60))
            .unwrap();
        backend.stop().unwrap();
        let frame_count = state.borrow().frames.len();
        backend.stop().unwrap();

        let state = state.borrow();
        let touch_cancellations = state.frames[4..6]
            .iter()
            .map(|(_, frame)| {
                frame
                    .iter()
                    .filter_map(|pointer| {
                        let info = touch_info(pointer);
                        (info.pointerInfo.pointerFlags & POINTER_FLAG_CANCELED != 0)
                            .then_some(info.pointerInfo.pointerId)
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(touch_cancellations, vec![vec![1], vec![2]]);
        let pen_cancel = pen_info(&state.frames[3].1[0]);
        assert_eq!(pen_cancel.pointerInfo.pointerId, 99);
        assert_eq!(
            pen_cancel.pointerInfo.pointerFlags,
            POINTER_FLAG_UP | POINTER_FLAG_CANCELED
        );
        assert_eq!(state.destroyed, vec![2, 1]);
        assert_eq!(state.frames.len(), frame_count);
        assert!(backend.pen.is_none());
        assert!(backend.touch.is_none());
    }

    #[test]
    fn normalized_coordinates_use_full_virtual_desktop_extents_without_origin_offset() {
        let virtual_desktop = VirtualDesktop::new(3840, 2160).unwrap();
        assert_eq!(virtual_desktop.map(0, 0).unwrap(), (0, 0));
        assert_eq!(
            virtual_desktop.map(u16::MAX, u16::MAX).unwrap(),
            (3839, 2159)
        );
        assert_eq!(virtual_desktop.map(32768, 32768).unwrap(), (1920, 1080));
        assert!(VirtualDesktop::new(0, 1080).is_err());
        assert!(map_normalized_coordinate(0, 0).is_err());
    }
}
