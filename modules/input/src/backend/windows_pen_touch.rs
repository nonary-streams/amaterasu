use crate::model::{
    Event, InputError, InputResult, PEN_POINTER_ID, PenTool, PenTouchDevice, PenTouchPhase,
    pen_tilt_axes,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CStr;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{GetLastError, RECT};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows_sys::Win32::UI::Controls::{
    HSYNTHETICPOINTERDEVICE, POINTER_FEEDBACK_DEFAULT, POINTER_TYPE_INFO, POINTER_TYPE_INFO_0,
};
use windows_sys::Win32::UI::Input::Pointer::{
    POINTER_FLAG_CANCELED, POINTER_FLAG_DOWN, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE,
    POINTER_FLAG_UP, POINTER_FLAG_UPDATE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, PEN_FLAG_BARREL, PEN_FLAG_ERASER, PEN_FLAG_NONE, PEN_MASK_NONE,
    PEN_MASK_PRESSURE, PEN_MASK_ROTATION, PEN_MASK_TILT_X, PEN_MASK_TILT_Y, PT_PEN, PT_TOUCH,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    TOUCH_MASK_CONTACTAREA, TOUCH_MASK_NONE, TOUCH_MASK_ORIENTATION, TOUCH_MASK_PRESSURE,
};

const MAX_PEN_CONTACTS: usize = 1;
const MAX_TOUCH_CONTACTS: usize = 10;
const PEN_MAX_CONTACTS: u32 = MAX_PEN_CONTACTS as u32;
const TOUCH_MAX_CONTACTS: u32 = MAX_TOUCH_CONTACTS as u32;
const NATIVE_PEN_POINTER_ID: u32 = 0;
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
            contact_area_major,
            contact_area_minor,
            rotation_degrees,
            pen_tool,
            pen_buttons,
            tilt,
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
                recovery_required: false,
                uncertain_contacts: BTreeSet::new(),
            });
        }
        let terminal_pointer =
            matches!(phase, PenTouchPhase::Up | PenTouchPhase::Cancel).then_some(*pointer_id);
        let recovered = self.recover_device(*device, virtual_desktop, terminal_pointer)?;
        if recovered && terminal_pointer.is_some() {
            // Destroying the old generation released this contact. A terminal
            // event must not recreate it merely to send Up or Cancel.
            if let Some(state) = self.device_state_mut(*device).as_mut() {
                state.contacts.remove(pointer_id);
                state.uncertain_contacts.remove(pointer_id);
            }
            self.update_keepalive_deadline(Instant::now());
            return Ok(());
        }
        if terminal_pointer.is_some()
            && self
                .device_state(*device)
                .is_some_and(|state| state.uncertain_contacts.contains(pointer_id))
        {
            if let Some(state) = self.device_state_mut(*device).as_mut() {
                state.contacts.remove(pointer_id);
                state.uncertain_contacts.remove(pointer_id);
            }
            self.update_keepalive_deadline(Instant::now());
            return Ok(());
        }
        self.prepare_active_contact(*device, *pointer_id, virtual_desktop)?;
        let contact = PointerContact {
            x: *x,
            y: *y,
            pressure: *pressure,
            effective_pressure: None,
            contact_area_major: *contact_area_major,
            contact_area_minor: *contact_area_minor,
            effective_contact_area: None,
            rotation_degrees: *rotation_degrees,
            effective_rotation_degrees: None,
            pen_tool: *pen_tool,
            pen_buttons: *pen_buttons,
            effective_pen_tool: None,
            effective_pen_buttons: None,
            tilt: *tilt,
            effective_tilt_axes: None,
        };
        let frame = {
            let state_slot = self.device_state_slot_mut(*device);
            let state = state_slot
                .as_mut()
                .expect("pointer device state was created above");
            let previous = state.contacts.get(pointer_id).copied();
            let mut contact = contact;
            contact.effective_pen_tool = match contact.pen_tool {
                Some(PenTool::Pen | PenTool::Eraser) => contact.pen_tool,
                Some(PenTool::Unknown) | None => {
                    previous.and_then(|previous| previous.effective_pen_tool)
                }
            };
            contact.effective_pen_buttons = contact
                .pen_buttons
                .or_else(|| previous.and_then(|previous| previous.effective_pen_buttons));
            contact.effective_pressure = contact
                .pressure
                .or_else(|| previous.and_then(|previous| previous.effective_pressure));
            contact.effective_contact_area = contact
                .contact_area_major
                .zip(contact.contact_area_minor)
                .map(|(major, minor)| (major, minor, contact.rotation_degrees))
                .or_else(|| previous.and_then(|previous| previous.effective_contact_area));
            contact.effective_rotation_degrees = contact
                .rotation_degrees
                .or_else(|| previous.and_then(|previous| previous.effective_rotation_degrees));
            contact.effective_tilt_axes = pen_tilt_axes(contact.tilt, contact.rotation_degrees)
                .map(|(x, y)| (x.round() as i32, y.round() as i32))
                .or_else(|| previous.and_then(|previous| previous.effective_tilt_axes));
            state.contacts.insert(*pointer_id, contact);
            let frame = build_pointer_frame(
                *device,
                &state.native_contacts(),
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
            if let Some(state) = self.device_state_mut(*device).as_mut() {
                state.recovery_required = true;
                if matches!(
                    phase,
                    PenTouchPhase::Down | PenTouchPhase::Up | PenTouchPhase::Cancel
                ) {
                    state.uncertain_contacts.insert(*pointer_id);
                }
            }
            self.quarantine_device(*device);
            self.update_keepalive_deadline(Instant::now());
            return Err(error);
        }

        if matches!(phase, PenTouchPhase::Up | PenTouchPhase::Cancel)
            && let Some(state) = self.device_state_mut(*device).as_mut()
        {
            state.contacts.remove(pointer_id);
            state.uncertain_contacts.remove(pointer_id);
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
        for device in [PenTouchDevice::Pen, PenTouchDevice::Touch] {
            if self
                .device_state(device)
                .is_some_and(|state| state.recovery_required)
            {
                // The generation is uncertain; destroying its owned handle
                // releases every contact without sending another poisoned frame.
                continue;
            }
            let pointer_ids = self
                .device_state(device)
                .map(|state| state.contacts.keys().copied().collect::<Vec<_>>())
                .unwrap_or_default();
            for pointer_id in pointer_ids {
                // An injection failure can mean the synthetic handle was lost.
                // Destroying the owned handle below is the final release, so a
                // failed best-effort cancel must not strand the lease in cleanup.
                let _ = self.cancel_pointer(device, pointer_id);
            }
        }

        for device in [PenTouchDevice::Pen, PenTouchDevice::Touch] {
            if let Some(state) = self.device_state_slot_mut(device).take()
                && !state.handle.is_null()
            {
                self.api.destroy_device(state.handle);
            }
        }
        self.next_keepalive = None;

        Ok(())
    }

    fn cancel_pointer(&mut self, device: PenTouchDevice, pointer_id: u32) -> InputResult<()> {
        if self
            .device_state(device)
            .is_some_and(|state| state.uncertain_contacts.contains(&pointer_id))
        {
            if let Some(state) = self.device_state_mut(device).as_mut() {
                state.contacts.remove(&pointer_id);
                state.uncertain_contacts.remove(&pointer_id);
            }
            return Ok(());
        }
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
            let mut contacts = state.native_contacts();
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
        let mut first_error = None;
        for device in [PenTouchDevice::Pen, PenTouchDevice::Touch] {
            if let Err(error) = self.recover_device(device, virtual_desktop, None) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
                continue;
            }
            let Some(state) = self.device_state(device) else {
                continue;
            };
            let contacts = state.native_contacts();
            if contacts.is_empty() {
                continue;
            }
            let frame = build_pointer_frame(device, &contacts, None, virtual_desktop)
                .map(|records| (state.handle, records));
            let result = match frame {
                Ok((handle, records)) => self.api.inject(handle, &records),
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                if let Some(state) = self.device_state_mut(device).as_mut() {
                    state.recovery_required = true;
                }
                self.quarantine_device(device);
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn device_state(&self, device: PenTouchDevice) -> Option<&PointerDeviceState> {
        match device {
            PenTouchDevice::Pen => self.pen.as_ref(),
            PenTouchDevice::Touch => self.touch.as_ref(),
        }
    }

    fn recover_device(
        &mut self,
        device: PenTouchDevice,
        virtual_desktop: VirtualDesktop,
        terminal_pointer: Option<u32>,
    ) -> InputResult<bool> {
        let Some(mut state) = self.device_state_slot_mut(device).take() else {
            return Ok(false);
        };
        if !state.recovery_required {
            *self.device_state_slot_mut(device) = Some(state);
            return Ok(false);
        }

        if !state.handle.is_null() {
            self.api.destroy_device(state.handle);
            state.handle = std::ptr::null_mut();
        }
        let result = (|| {
            let mut restored = BTreeMap::new();
            for (&pointer_id, &contact) in &state.contacts {
                if state.uncertain_contacts.contains(&pointer_id)
                    || terminal_pointer == Some(pointer_id)
                {
                    continue;
                }
                if state.handle.is_null() {
                    state.handle = self.api.create_device(device)?;
                }
                restored.insert(pointer_id, contact);
                let records = build_pointer_frame(
                    device,
                    &restored,
                    Some((pointer_id, PenTouchPhase::Down)),
                    virtual_desktop,
                )?;
                self.api.inject(state.handle, &records)?;
            }
            state.recovery_required = false;
            Ok(true)
        })();
        if result.is_err() && !state.handle.is_null() {
            self.api.destroy_device(state.handle);
            state.handle = std::ptr::null_mut();
        }
        *self.device_state_slot_mut(device) = Some(state);
        result
    }

    fn quarantine_device(&mut self, device: PenTouchDevice) {
        if let Some(mut state) = self.device_state_slot_mut(device).take() {
            if !state.handle.is_null() {
                self.api.destroy_device(state.handle);
                state.handle = std::ptr::null_mut();
            }
            *self.device_state_slot_mut(device) = Some(state);
        }
    }

    fn prepare_active_contact(
        &mut self,
        device: PenTouchDevice,
        pointer_id: u32,
        virtual_desktop: VirtualDesktop,
    ) -> InputResult<()> {
        let Some(mut state) = self.device_state_slot_mut(device).take() else {
            return Ok(());
        };
        let result = (|| {
            if state.handle.is_null() {
                state.handle = self.api.create_device(device)?;
            }
            if state.uncertain_contacts.contains(&pointer_id) {
                let mut contacts = state.native_contacts();
                if let Some(contact) = state.contacts.get(&pointer_id) {
                    contacts.insert(pointer_id, *contact);
                    let records = build_pointer_frame(
                        device,
                        &contacts,
                        Some((pointer_id, PenTouchPhase::Down)),
                        virtual_desktop,
                    )?;
                    if let Err(error) = self.api.inject(state.handle, &records) {
                        state.recovery_required = true;
                        self.api.destroy_device(state.handle);
                        state.handle = std::ptr::null_mut();
                        return Err(error);
                    }
                    state.uncertain_contacts.remove(&pointer_id);
                }
            }
            Ok(())
        })();
        *self.device_state_slot_mut(device) = Some(state);
        result
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
            .any(|state| {
                state
                    .contacts
                    .keys()
                    .any(|pointer_id| !state.uncertain_contacts.contains(pointer_id))
            })
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
    recovery_required: bool,
    uncertain_contacts: BTreeSet<u32>,
}

impl PointerDeviceState {
    fn native_contacts(&self) -> BTreeMap<u32, PointerContact> {
        self.contacts
            .iter()
            .filter(|(pointer_id, _)| !self.uncertain_contacts.contains(pointer_id))
            .map(|(&pointer_id, &contact)| (pointer_id, contact))
            .collect()
    }
}

#[derive(Clone, Copy)]
struct PointerContact {
    x: u16,
    y: u16,
    pressure: Option<f32>,
    // A sparse move leaves the native pressure mask unset, but recreating a
    // device must restore the last pressure explicitly reported for this ID.
    effective_pressure: Option<f32>,
    contact_area_major: Option<f32>,
    contact_area_minor: Option<f32>,
    // Pair area with the rotation used when its native bounds were emitted.
    // A later orientation-only update must not rotate those existing bounds.
    effective_contact_area: Option<(f32, f32, Option<u16>)>,
    rotation_degrees: Option<u16>,
    effective_rotation_degrees: Option<u16>,
    // Raw fields preserve exactly what the latest event reported. None and
    // Unknown omit native update masks; effective fields restore a new Down.
    pen_tool: Option<PenTool>,
    pen_buttons: Option<u8>,
    // Windows frames are rebuilt from scratch, so carry forward effective
    // state separately when the latest event omits a tool or button value.
    effective_pen_tool: Option<PenTool>,
    effective_pen_buttons: Option<u8>,
    tilt: Option<u8>,
    // Keep the last emitted axis pair; tilt and rotation samples are sparse.
    effective_tilt_axes: Option<(i32, i32)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VirtualDesktop {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

impl VirtualDesktop {
    fn new(left: i32, top: i32, width: i32, height: i32) -> InputResult<Self> {
        if width <= 0 || height <= 0 {
            return Err(InputError::new(
                "desktop_unavailable",
                "Windows reported an invalid virtual desktop size",
            ));
        }
        left.checked_add(width).ok_or_else(|| {
            InputError::new(
                "desktop_unavailable",
                "Windows virtual desktop horizontal bounds exceed pointer API limits",
            )
        })?;
        top.checked_add(height).ok_or_else(|| {
            InputError::new(
                "desktop_unavailable",
                "Windows virtual desktop vertical bounds exceed pointer API limits",
            )
        })?;
        Ok(Self {
            left,
            top,
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

    fn screen_position(&self, x: i32, y: i32) -> InputResult<(i32, i32)> {
        Ok((
            self.left
                .checked_add(x)
                .ok_or_else(coordinate_overflow_error)?,
            self.top
                .checked_add(y)
                .ok_or_else(coordinate_overflow_error)?,
        ))
    }
}

fn coordinate_overflow_error() -> InputError {
    InputError::new(
        "desktop_unavailable",
        "Windows virtual desktop coordinate exceeds pointer API limits",
    )
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
    let (screen_x, screen_y) = virtual_desktop.screen_position(x, y)?;
    let pointer_flags = match phase {
        PenTouchPhase::Down => POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_DOWN,
        PenTouchPhase::Move => POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_UPDATE,
        PenTouchPhase::Up => POINTER_FLAG_UP,
        PenTouchPhase::Cancel => POINTER_FLAG_UP | POINTER_FLAG_CANCELED,
    };
    let rotation_degrees = if phase == PenTouchPhase::Down {
        contact
            .rotation_degrees
            .or(contact.effective_rotation_degrees)
    } else {
        contact.rotation_degrees
    };

    let mut pointer = POINTER_TYPE_INFO::default();
    match device {
        PenTouchDevice::Pen => {
            let mut pen_info = windows_sys::Win32::UI::Input::Pointer::POINTER_PEN_INFO::default();
            pen_info.pointerInfo.pointerType = PT_PEN;
            // The event contract uses PEN_POINTER_ID internally because pen
            // packets have no transport ID; the Windows synthetic pen has its
            // own stable native identity.
            debug_assert_eq!(pointer_id, PEN_POINTER_ID);
            pen_info.pointerInfo.pointerId = NATIVE_PEN_POINTER_ID;
            pen_info.pointerInfo.pointerFlags = pointer_flags;
            pen_info.pointerInfo.ptPixelLocation.x = x;
            pen_info.pointerInfo.ptPixelLocation.y = y;
            pen_info.penFlags = PEN_FLAG_NONE;
            pen_info.penMask = PEN_MASK_NONE;

            if contact.effective_pen_tool == Some(PenTool::Eraser) {
                pen_info.penFlags |= PEN_FLAG_ERASER;
            }
            if contact
                .effective_pen_buttons
                .is_some_and(|buttons| buttons != 0)
            {
                // Windows synthetic pens expose one barrel button. Preserve
                // the client mask in state and map any pressed button to it.
                pen_info.penFlags |= PEN_FLAG_BARREL;
            }

            let pressure = if phase == PenTouchPhase::Down {
                contact.pressure.or(contact.effective_pressure)
            } else {
                contact.pressure
            };
            if matches!(phase, PenTouchPhase::Down | PenTouchPhase::Move)
                && let Some(pressure) = pressure
            {
                pen_info.penMask |= PEN_MASK_PRESSURE;
                pen_info.pressure = pressure_to_native(pressure);
            }
            if let Some(rotation) = rotation_degrees {
                pen_info.penMask |= PEN_MASK_ROTATION;
                pen_info.rotation = u32::from(rotation);
            }
            let tilt_axes = pen_tilt_axes(contact.tilt, contact.rotation_degrees)
                .map(|(x, y)| (x.round() as i32, y.round() as i32));
            let tilt_axes = if phase == PenTouchPhase::Down {
                tilt_axes.or(contact.effective_tilt_axes)
            } else {
                tilt_axes
            };
            if let Some((tilt_x, tilt_y)) = tilt_axes {
                pen_info.penMask |= PEN_MASK_TILT_X | PEN_MASK_TILT_Y;
                pen_info.tiltX = tilt_x;
                pen_info.tiltY = tilt_y;
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
            touch_info.rcContact = RECT {
                left: screen_x,
                top: screen_y,
                right: screen_x,
                bottom: screen_y,
            };
            touch_info.rcContactRaw = touch_info.rcContact;
            touch_info.pressure = 512;
            let pressure = if phase == PenTouchPhase::Down {
                contact.pressure.or(contact.effective_pressure)
            } else {
                contact.pressure
            };
            if matches!(phase, PenTouchPhase::Down | PenTouchPhase::Move)
                && let Some(pressure) = pressure
            {
                touch_info.touchMask |= TOUCH_MASK_PRESSURE;
                touch_info.pressure = pressure_to_native(pressure);
            }
            let contact_area = contact
                .contact_area_major
                .zip(contact.contact_area_minor)
                .map(|(major, minor)| (major, minor, contact.rotation_degrees));
            let contact_area = if phase == PenTouchPhase::Down {
                contact_area.or(contact.effective_contact_area)
            } else {
                contact_area
            };
            if let Some((major, minor, area_rotation)) = contact_area {
                let bounds = contact_rectangle(
                    screen_x,
                    screen_y,
                    major,
                    minor,
                    area_rotation,
                    virtual_desktop,
                );
                touch_info.rcContact = bounds;
                touch_info.rcContactRaw = bounds;
                touch_info.touchMask |= TOUCH_MASK_CONTACTAREA;
            }
            if let Some(rotation) = rotation_degrees {
                touch_info.orientation = u32::from(rotation);
                touch_info.touchMask |= TOUCH_MASK_ORIENTATION;
            }

            pointer.r#type = PT_TOUCH;
            pointer.Anonymous = POINTER_TYPE_INFO_0 {
                touchInfo: touch_info,
            };
        }
    }
    Ok(pointer)
}

fn contact_rectangle(
    center_x: i32,
    center_y: i32,
    major: f32,
    minor: f32,
    rotation_degrees: Option<u16>,
    virtual_desktop: VirtualDesktop,
) -> RECT {
    // Contact area lengths use the same 16-bit normalized touch-port space as
    // position. The transport has already transformed the axes to the host
    // touch port, so recover those values instead of scaling by the desktop a
    // second time. The capture touch-port bounds are not carried in this event;
    // this adapter clips against the full virtual desktop as its available bound.
    let major_angle = f64::from(rotation_degrees.unwrap_or(45)).to_radians();
    let minor_angle = major_angle + std::f64::consts::FRAC_PI_2;
    let minor = if minor == 0.0 { major } else { minor };
    let major_pixels = (f64::from(major) * f64::from(u16::MAX)).round();
    let minor_pixels = (f64::from(minor) * f64::from(u16::MAX)).round();
    let width = absolute_trig_component(major_angle.cos()) * major_pixels
        + absolute_trig_component(minor_angle.cos()) * minor_pixels;
    let height = absolute_trig_component(major_angle.sin()) * major_pixels
        + absolute_trig_component(minor_angle.sin()) * minor_pixels;

    let desktop_left = f64::from(virtual_desktop.left);
    let desktop_top = f64::from(virtual_desktop.top);
    let desktop_right = desktop_left + f64::from(virtual_desktop.width);
    let desktop_bottom = desktop_top + f64::from(virtual_desktop.height);
    RECT {
        left: ((f64::from(center_x) - width / 2.0)
            .floor()
            .clamp(desktop_left, desktop_right)) as i32,
        top: ((f64::from(center_y) - height / 2.0)
            .floor()
            .clamp(desktop_top, desktop_bottom)) as i32,
        right: ((f64::from(center_x) + width / 2.0)
            .ceil()
            .clamp(desktop_left, desktop_right)) as i32,
        bottom: ((f64::from(center_y) + height / 2.0)
            .ceil()
            .clamp(desktop_top, desktop_bottom)) as i32,
    }
}

fn absolute_trig_component(value: f64) -> f64 {
    let value = value.abs();
    if value < 1e-12 {
        0.0
    } else if (1.0 - value).abs() < 1e-12 {
        1.0
    } else {
        value
    }
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
        // Pointer positions are virtual-screen-relative. Touch contact
        // rectangles use screen coordinates and need the possibly negative origin.
        let left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        VirtualDesktop::new(left, top, width, height)
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
    use std::collections::BTreeSet;
    use std::rc::Rc;

    #[derive(Default)]
    struct FakeState {
        supported: bool,
        desktop: Option<VirtualDesktop>,
        created: Vec<(PenTouchDevice, usize, u32)>,
        frames: Vec<(PenTouchDevice, Vec<POINTER_TYPE_INFO>)>,
        destroyed: Vec<usize>,
        fail_next_injection: bool,
        lose_handle_on_next_failure: bool,
        lost_handles: BTreeSet<usize>,
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
            let handle_id = handle as usize;
            if state.lost_handles.contains(&handle_id) {
                return Err(InputError::new(
                    "injection_failed",
                    "synthetic handle is lost",
                ));
            }
            if state.fail_next_injection {
                state.fail_next_injection = false;
                if std::mem::take(&mut state.lose_handle_on_next_failure) {
                    state.lost_handles.insert(handle_id);
                }
                return Err(InputError::new("injection_failed", "simulated device loss"));
            }
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
            desktop: Some(VirtualDesktop::new(0, 0, 1920, 1080).unwrap()),
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
            contact_area_major: None,
            contact_area_minor: None,
            rotation_degrees: None,
            pen_tool: None,
            pen_buttons: None,
            tilt: None,
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
    fn pen_events_keep_internal_identity_and_map_native_metadata() {
        let (mut backend, state) = backend();
        let mut down = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Down,
            u16::MAX,
            0,
        );
        if let Event::PenTouch {
            pressure,
            rotation_degrees,
            pen_tool,
            pen_buttons,
            tilt,
            ..
        } = &mut down
        {
            *pressure = Some(0.5);
            *rotation_degrees = Some(90);
            *pen_tool = Some(PenTool::Eraser);
            *pen_buttons = Some(1);
            *tilt = Some(45);
        }
        backend.inject(&down).unwrap();

        let mut move_event = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Move,
            0,
            u16::MAX,
        );
        if let Event::PenTouch {
            pressure,
            rotation_degrees,
            pen_tool,
            pen_buttons,
            tilt,
            ..
        } = &mut move_event
        {
            *pressure = Some(1.0);
            *rotation_degrees = Some(0);
            *pen_tool = Some(PenTool::Pen);
            *pen_buttons = Some(0);
            *tilt = Some(45);
        }
        backend.inject(&move_event).unwrap();
        backend
            .inject(&event(
                PEN_POINTER_ID,
                PenTouchDevice::Pen,
                PenTouchPhase::Up,
                100,
                200,
            ))
            .unwrap();

        let state = state.borrow();
        assert_eq!(state.created, vec![(PenTouchDevice::Pen, 1, 1)]);
        assert_eq!(state.frames.len(), 3);
        let down = pen_info(&state.frames[0].1[0]);
        assert_eq!(down.pointerInfo.pointerId, NATIVE_PEN_POINTER_ID);
        assert_eq!(down.pointerInfo.ptPixelLocation.x, 1919);
        assert_eq!(down.pointerInfo.ptPixelLocation.y, 0);
        assert_eq!(down.pressure, 512);
        assert_eq!(down.rotation, 90);
        assert_eq!(down.tiltX, -45);
        assert_eq!(down.tiltY, 0);
        assert_eq!(down.penFlags, PEN_FLAG_ERASER | PEN_FLAG_BARREL);
        assert_eq!(
            down.penMask,
            PEN_MASK_PRESSURE | PEN_MASK_ROTATION | PEN_MASK_TILT_X | PEN_MASK_TILT_Y
        );
        assert_eq!(
            down.pointerInfo.pointerFlags & POINTER_FLAG_DOWN,
            POINTER_FLAG_DOWN
        );

        let moved = pen_info(&state.frames[1].1[0]);
        assert_eq!(moved.pointerInfo.pointerId, NATIVE_PEN_POINTER_ID);
        assert_eq!(moved.pointerInfo.ptPixelLocation.x, 0);
        assert_eq!(moved.pointerInfo.ptPixelLocation.y, 1079);
        assert_eq!(moved.pressure, 1024);
        assert_eq!(moved.rotation, 0);
        assert_eq!(moved.tiltX, 0);
        assert_eq!(moved.tiltY, 45);
        assert_eq!(moved.penFlags, PEN_FLAG_NONE);
        assert_eq!(moved.penMask & PEN_MASK_ROTATION, PEN_MASK_ROTATION);
        assert_eq!(
            moved.pointerInfo.pointerFlags & POINTER_FLAG_UPDATE,
            POINTER_FLAG_UPDATE
        );

        let up = pen_info(&state.frames[2].1[0]);
        assert_eq!(up.pointerInfo.pointerId, NATIVE_PEN_POINTER_ID);
        assert_eq!(up.pointerInfo.pointerFlags, POINTER_FLAG_UP);
        assert!(backend.pen.as_ref().unwrap().contacts.is_empty());
    }

    #[test]
    fn unknown_pen_metadata_is_preserved_without_native_guesses() {
        let (mut backend, state) = backend();
        let mut down = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Down,
            100,
            200,
        );
        if let Event::PenTouch { pen_tool, tilt, .. } = &mut down {
            *pen_tool = Some(PenTool::Unknown);
            *tilt = Some(40);
        }
        backend.inject(&down).unwrap();

        let contact = backend
            .pen
            .as_ref()
            .unwrap()
            .contacts
            .get(&PEN_POINTER_ID)
            .unwrap();
        assert_eq!(contact.pen_tool, Some(PenTool::Unknown));
        assert_eq!(contact.rotation_degrees, None);
        assert_eq!(contact.pen_buttons, None);
        assert_eq!(contact.effective_pen_tool, None);
        assert_eq!(contact.effective_pen_buttons, None);
        assert_eq!(contact.tilt, Some(40));

        let pen = pen_info(&state.borrow().frames[0].1[0]);
        assert_eq!(pen.penFlags, PEN_FLAG_NONE);
        assert_eq!(
            pen.penMask & (PEN_MASK_ROTATION | PEN_MASK_TILT_X | PEN_MASK_TILT_Y),
            0
        );
    }

    #[test]
    fn missing_and_unknown_pen_metadata_preserve_native_tool_and_button_state() {
        let (mut backend, state) = backend();
        let mut down = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Down,
            100,
            200,
        );
        if let Event::PenTouch {
            pen_tool,
            pen_buttons,
            ..
        } = &mut down
        {
            *pen_tool = Some(PenTool::Eraser);
            *pen_buttons = Some(1);
        }
        backend.inject(&down).unwrap();

        let mut unknown_metadata = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Move,
            110,
            210,
        );
        if let Event::PenTouch {
            pen_tool,
            pen_buttons,
            ..
        } = &mut unknown_metadata
        {
            *pen_tool = Some(PenTool::Unknown);
            *pen_buttons = None;
        }
        backend.inject(&unknown_metadata).unwrap();

        let mut absent_metadata = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Move,
            120,
            220,
        );
        if let Event::PenTouch {
            pen_tool,
            pen_buttons,
            ..
        } = &mut absent_metadata
        {
            *pen_tool = None;
            *pen_buttons = None;
        }
        backend.inject(&absent_metadata).unwrap();

        let mut explicit_release = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Move,
            130,
            230,
        );
        if let Event::PenTouch {
            pen_tool,
            pen_buttons,
            ..
        } = &mut explicit_release
        {
            *pen_tool = Some(PenTool::Pen);
            *pen_buttons = Some(0);
        }
        backend.inject(&explicit_release).unwrap();

        let frames = &state.borrow().frames;
        assert_eq!(frames.len(), 4);
        assert_eq!(
            pen_info(&frames[0].1[0]).penFlags,
            PEN_FLAG_ERASER | PEN_FLAG_BARREL
        );
        assert_eq!(
            pen_info(&frames[1].1[0]).penFlags,
            PEN_FLAG_ERASER | PEN_FLAG_BARREL
        );
        assert_eq!(
            pen_info(&frames[2].1[0]).penFlags,
            PEN_FLAG_ERASER | PEN_FLAG_BARREL
        );
        assert_eq!(pen_info(&frames[3].1[0]).penFlags, PEN_FLAG_NONE);

        let contact = backend
            .pen
            .as_ref()
            .unwrap()
            .contacts
            .get(&PEN_POINTER_ID)
            .unwrap();
        assert_eq!(contact.pen_tool, Some(PenTool::Pen));
        assert_eq!(contact.pen_buttons, Some(0));
        assert_eq!(contact.effective_pen_tool, Some(PenTool::Pen));
        assert_eq!(contact.effective_pen_buttons, Some(0));
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
    fn touch_area_and_known_orientation_map_to_contact_rectangle() {
        let (mut backend, state) = backend();
        let mut down = event(42, PenTouchDevice::Touch, PenTouchPhase::Down, 32768, 32768);
        if let Event::PenTouch {
            pressure,
            contact_area_major,
            contact_area_minor,
            rotation_degrees,
            ..
        } = &mut down
        {
            *pressure = Some(0.5);
            *contact_area_major = Some(20.0 / f32::from(u16::MAX));
            *contact_area_minor = Some(10.0 / f32::from(u16::MAX));
            *rotation_degrees = Some(0);
        }
        backend.inject(&down).unwrap();

        let frame = state.borrow();
        let touch = touch_info(&frame.frames[0].1[0]);
        assert_eq!(touch.pointerInfo.pointerId, 42);
        assert_eq!(touch.pointerInfo.ptPixelLocation.x, 960);
        assert_eq!(touch.pointerInfo.ptPixelLocation.y, 540);
        assert_eq!(touch.orientation, 0);
        assert_eq!(touch.pressure, 512);
        assert_eq!(
            touch.touchMask,
            TOUCH_MASK_CONTACTAREA | TOUCH_MASK_ORIENTATION | TOUCH_MASK_PRESSURE
        );
        assert_eq!(
            (
                touch.rcContact.left,
                touch.rcContact.top,
                touch.rcContact.right,
                touch.rcContact.bottom,
            ),
            (950, 535, 970, 545)
        );
        assert_eq!(
            (
                touch.rcContactRaw.left,
                touch.rcContactRaw.top,
                touch.rcContactRaw.right,
                touch.rcContactRaw.bottom,
            ),
            (
                touch.rcContact.left,
                touch.rcContact.top,
                touch.rcContact.right,
                touch.rcContact.bottom,
            )
        );
    }

    #[test]
    fn unknown_touch_rotation_uses_bounds_fallback_without_claiming_orientation() {
        let (mut backend, state) = backend();
        let mut down = event(42, PenTouchDevice::Touch, PenTouchPhase::Down, 0, 0);
        if let Event::PenTouch {
            contact_area_major,
            contact_area_minor,
            ..
        } = &mut down
        {
            *contact_area_major = Some(20.0 / f32::from(u16::MAX));
            *contact_area_minor = Some(10.0 / f32::from(u16::MAX));
        }
        backend.inject(&down).unwrap();

        let frame = state.borrow();
        let touch = touch_info(&frame.frames[0].1[0]);
        assert_eq!(touch.orientation, 0);
        assert_eq!(touch.touchMask, TOUCH_MASK_CONTACTAREA);
        assert_eq!(
            (
                touch.rcContact.left,
                touch.rcContact.top,
                touch.rcContact.right,
                touch.rcContact.bottom,
            ),
            (0, 0, 11, 11)
        );
        let contact = backend.touch.as_ref().unwrap().contacts.get(&42).unwrap();
        assert_eq!(contact.rotation_degrees, None);
    }

    #[test]
    fn zero_minor_touch_area_uses_major_for_bounds_but_preserves_original_sample() {
        let (mut backend, state) = backend();
        let mut down = event(42, PenTouchDevice::Touch, PenTouchPhase::Down, 32768, 32768);
        if let Event::PenTouch {
            contact_area_major,
            contact_area_minor,
            rotation_degrees,
            ..
        } = &mut down
        {
            *contact_area_major = Some(20.0 / f32::from(u16::MAX));
            *contact_area_minor = Some(0.0);
            *rotation_degrees = Some(0);
        }
        backend.inject(&down).unwrap();

        let frame = state.borrow();
        let touch = touch_info(&frame.frames[0].1[0]);
        assert_eq!(
            (
                touch.rcContact.left,
                touch.rcContact.top,
                touch.rcContact.right,
                touch.rcContact.bottom,
            ),
            (950, 530, 970, 550)
        );
        let contact = backend.touch.as_ref().unwrap().contacts.get(&42).unwrap();
        assert_eq!(contact.contact_area_minor, Some(0.0));
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
        if let Event::PenTouch { tilt, .. } = &mut invalid_tilt {
            *tilt = Some(91);
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
        assert_eq!(state.frames.len(), 2);
        assert_eq!(state.destroyed, vec![1, 2]);
        assert_eq!(state.created.len(), 2);
        assert_eq!(
            touch_info(&state.frames[1].1[0]).pointerInfo.pointerFlags,
            POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_DOWN
        );
        assert!(state.frames.iter().all(|(_, frame)| {
            frame
                .iter()
                .all(|pointer| touch_info(pointer).pointerInfo.pointerId != 10)
        }));
        assert!(backend.touch.as_ref().unwrap().contacts.is_empty());
    }

    #[test]
    fn lost_touch_generation_does_not_replay_uncertain_down_before_other_cancel() {
        let (mut backend, state) = backend();
        backend
            .inject(&event(
                20,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                30,
                40,
            ))
            .unwrap();
        backend
            .inject(&event(
                PEN_POINTER_ID,
                PenTouchDevice::Pen,
                PenTouchPhase::Down,
                50,
                60,
            ))
            .unwrap();
        {
            let mut fake = state.borrow_mut();
            fake.fail_next_injection = true;
            fake.lose_handle_on_next_failure = true;
        }
        assert_eq!(
            backend
                .inject(&event(
                    10,
                    PenTouchDevice::Touch,
                    PenTouchPhase::Down,
                    10,
                    20
                ))
                .unwrap_err()
                .code,
            "injection_failed"
        );
        assert!(backend.touch.as_ref().unwrap().recovery_required);
        assert!(
            backend
                .touch
                .as_ref()
                .unwrap()
                .uncertain_contacts
                .contains(&10)
        );
        backend
            .inject(&event(
                20,
                PenTouchDevice::Touch,
                PenTouchPhase::Cancel,
                30,
                40,
            ))
            .unwrap();
        assert!(backend.touch.as_ref().unwrap().contacts.contains_key(&10));
        assert!(!backend.touch.as_ref().unwrap().contacts.contains_key(&20));
        assert!(
            backend
                .pen
                .as_ref()
                .unwrap()
                .contacts
                .contains_key(&PEN_POINTER_ID)
        );
        assert_eq!(state.borrow().destroyed, vec![1]);
        {
            let fake = state.borrow();
            assert_eq!(fake.frames.len(), 2);
            assert!(
                fake.frames
                    .iter()
                    .filter(|(device, _)| *device == PenTouchDevice::Touch)
                    .all(|(_, frame)| {
                        frame
                            .iter()
                            .all(|pointer| touch_info(pointer).pointerInfo.pointerId != 10)
                    })
            );
        }
        backend
            .inject(&event(
                10,
                PenTouchDevice::Touch,
                PenTouchPhase::Cancel,
                10,
                20,
            ))
            .unwrap();
        backend.stop().unwrap();
        assert_eq!(state.borrow().destroyed, vec![1, 2]);
        backend.start().unwrap();
        backend
            .inject(&event(
                30,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                70,
                80,
            ))
            .unwrap();
        backend.stop().unwrap();
    }

    #[test]
    fn healthy_touch_move_replays_without_uncertain_pointer_on_new_handle() {
        let (mut backend, state) = backend();
        backend
            .inject(&event(
                20,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                30,
                40,
            ))
            .unwrap();
        {
            let mut fake = state.borrow_mut();
            fake.fail_next_injection = true;
            fake.lose_handle_on_next_failure = true;
        }
        assert!(
            backend
                .inject(&event(
                    10,
                    PenTouchDevice::Touch,
                    PenTouchPhase::Down,
                    10,
                    20
                ))
                .is_err()
        );
        backend
            .inject(&event(
                20,
                PenTouchDevice::Touch,
                PenTouchPhase::Move,
                31,
                41,
            ))
            .unwrap();
        assert!(backend.touch.as_ref().unwrap().contacts.contains_key(&10));
        assert_eq!(state.borrow().destroyed, vec![1]);
        let fake = state.borrow();
        assert_eq!(fake.frames.len(), 3);
        for (_, frame) in &fake.frames[1..] {
            assert_eq!(frame.len(), 1);
            assert_eq!(touch_info(&frame[0]).pointerInfo.pointerId, 20);
        }
        drop(fake);
        backend.stop().unwrap();
    }

    #[test]
    fn lost_touch_handle_recovers_move_up_and_cancel_retries() {
        for phase in [
            PenTouchPhase::Move,
            PenTouchPhase::Up,
            PenTouchPhase::Cancel,
        ] {
            let (mut backend, state) = backend();
            let down = event(7, PenTouchDevice::Touch, PenTouchPhase::Down, 10, 20);
            backend.inject(&down).unwrap();
            let transition = event(7, PenTouchDevice::Touch, phase, 11, 21);
            {
                let mut fake = state.borrow_mut();
                fake.fail_next_injection = true;
                fake.lose_handle_on_next_failure = true;
            }
            assert_eq!(
                backend.inject(&transition).unwrap_err().code,
                "injection_failed"
            );
            assert_eq!(backend.touch.as_ref().unwrap().contacts[&7].x, 10);
            backend.inject(&transition).unwrap();
            assert_eq!(state.borrow().destroyed, vec![1]);
            assert!(!backend.touch.as_ref().unwrap().recovery_required);
            match phase {
                PenTouchPhase::Move => {
                    assert_eq!(backend.touch.as_ref().unwrap().contacts[&7].x, 11);
                    backend
                        .inject(&event(
                            7,
                            PenTouchDevice::Touch,
                            PenTouchPhase::Cancel,
                            11,
                            21,
                        ))
                        .unwrap();
                }
                PenTouchPhase::Up | PenTouchPhase::Cancel => {
                    assert!(backend.touch.as_ref().unwrap().contacts.is_empty());
                }
                PenTouchPhase::Down => unreachable!(),
            }
            backend.stop().unwrap();
        }
    }

    #[test]
    fn recreated_pointer_down_restores_explicit_metadata_after_sparse_move() {
        for (device, pointer_id) in [
            (PenTouchDevice::Pen, PEN_POINTER_ID),
            (PenTouchDevice::Touch, 7),
        ] {
            let (mut backend, state) = backend();
            let mut down = event(pointer_id, device, PenTouchPhase::Down, 32768, 32768);
            if let Event::PenTouch {
                pressure,
                contact_area_major,
                contact_area_minor,
                rotation_degrees,
                tilt,
                ..
            } = &mut down
            {
                *pressure = Some(0.75);
                *rotation_degrees = Some(90);
                if device == PenTouchDevice::Pen {
                    *tilt = Some(45);
                } else {
                    *contact_area_major = Some(0.02);
                    *contact_area_minor = Some(0.01);
                }
            }
            backend.inject(&down).unwrap();
            let mut changed = event(pointer_id, device, PenTouchPhase::Move, 32768, 32768);
            if let Event::PenTouch {
                pressure,
                contact_area_major,
                contact_area_minor,
                rotation_degrees,
                tilt,
                ..
            } = &mut changed
            {
                *pressure = Some(0.5);
                *rotation_degrees = Some(180);
                if device == PenTouchDevice::Pen {
                    *tilt = Some(30);
                } else {
                    *contact_area_major = Some(0.01);
                    *contact_area_minor = Some(0.005);
                }
            }
            backend.inject(&changed).unwrap();
            let sparse_move = event(pointer_id, device, PenTouchPhase::Move, 33000, 33000);
            backend.inject(&sparse_move).unwrap();
            {
                let fake = state.borrow();
                let sparse = &fake.frames[2].1[0];
                if device == PenTouchDevice::Pen {
                    let sparse = pen_info(sparse);
                    assert_eq!(sparse.penMask, PEN_MASK_NONE);
                } else {
                    let sparse = touch_info(sparse);
                    assert_eq!(sparse.touchMask, TOUCH_MASK_NONE);
                }
            }

            {
                let mut fake = state.borrow_mut();
                fake.fail_next_injection = true;
                fake.lose_handle_on_next_failure = true;
            }
            assert_eq!(
                backend.inject(&sparse_move).unwrap_err().code,
                "injection_failed"
            );
            assert!(backend.device_state(device).unwrap().recovery_required);
            backend.inject(&sparse_move).unwrap();
            {
                let fake = state.borrow();
                assert_eq!(fake.destroyed, [1]);
                assert_eq!(fake.created.len(), 2);
                assert_eq!(fake.frames.len(), 5);
                let replay = &fake.frames[3].1[0];
                if device == PenTouchDevice::Pen {
                    let replay = pen_info(replay);
                    let changed = pen_info(&fake.frames[1].1[0]);
                    assert_ne!(replay.pointerInfo.pointerFlags & POINTER_FLAG_DOWN, 0);
                    assert_eq!(replay.pressure, 512);
                    assert_eq!(replay.rotation, 180);
                    assert_eq!((replay.tiltX, replay.tiltY), (changed.tiltX, changed.tiltY));
                    assert_eq!(
                        replay.penMask,
                        PEN_MASK_PRESSURE | PEN_MASK_ROTATION | PEN_MASK_TILT_X | PEN_MASK_TILT_Y
                    );
                } else {
                    let replay = touch_info(replay);
                    let changed = touch_info(&fake.frames[1].1[0]);
                    assert_ne!(replay.pointerInfo.pointerFlags & POINTER_FLAG_DOWN, 0);
                    assert_eq!(replay.pressure, 512);
                    assert_eq!(replay.orientation, 180);
                    assert_eq!(
                        replay.touchMask,
                        TOUCH_MASK_PRESSURE | TOUCH_MASK_CONTACTAREA | TOUCH_MASK_ORIENTATION
                    );
                    assert!(replay.rcContact.right > replay.rcContact.left);
                    assert!(replay.rcContact.bottom > replay.rcContact.top);
                    assert_eq!(
                        replay.rcContact.right - replay.rcContact.left,
                        changed.rcContact.right - changed.rcContact.left
                    );
                    assert_eq!(
                        replay.rcContact.bottom - replay.rcContact.top,
                        changed.rcContact.bottom - changed.rcContact.top
                    );
                }
            }
            backend
                .inject(&event(pointer_id, device, PenTouchPhase::Up, 33100, 33100))
                .unwrap();
            backend.stop().unwrap();
            assert_eq!(state.borrow().destroyed, [1, 2]);
            backend.start().unwrap();
            backend
                .inject(&event(
                    pointer_id,
                    device,
                    PenTouchPhase::Down,
                    33200,
                    33200,
                ))
                .unwrap();
            let fake = state.borrow();
            let fresh = &fake.frames.last().unwrap().1[0];
            if device == PenTouchDevice::Pen {
                assert_eq!(pen_info(fresh).penMask, PEN_MASK_NONE);
            } else {
                assert_eq!(touch_info(fresh).touchMask, TOUCH_MASK_NONE);
            }
            drop(fake);
            backend.stop().unwrap();
            assert_eq!(state.borrow().destroyed, [1, 2, 3]);
        }
    }

    #[test]
    fn pen_recovery_replays_last_emitted_tilt_axes_after_single_field_moves() {
        let (mut backend, state) = backend();
        let mut down = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Down,
            32768,
            32768,
        );
        if let Event::PenTouch {
            rotation_degrees,
            tilt,
            ..
        } = &mut down
        {
            *rotation_degrees = Some(0);
            *tilt = Some(30);
        }
        backend.inject(&down).unwrap();
        let initial_axes = {
            let fake = state.borrow();
            let info = pen_info(&fake.frames[0].1[0]);
            (info.tiltX, info.tiltY)
        };
        let mut tilt_only = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Move,
            32768,
            32768,
        );
        if let Event::PenTouch { tilt, .. } = &mut tilt_only {
            *tilt = Some(45);
        }
        backend.inject(&tilt_only).unwrap();
        let mut rotation_only = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Move,
            32768,
            32768,
        );
        if let Event::PenTouch {
            rotation_degrees, ..
        } = &mut rotation_only
        {
            *rotation_degrees = Some(90);
        }
        backend.inject(&rotation_only).unwrap();
        {
            let fake = state.borrow();
            assert_eq!(
                pen_info(&fake.frames[1].1[0]).penMask & (PEN_MASK_TILT_X | PEN_MASK_TILT_Y),
                0
            );
            assert_eq!(pen_info(&fake.frames[2].1[0]).penMask, PEN_MASK_ROTATION);
        }
        {
            let mut fake = state.borrow_mut();
            fake.fail_next_injection = true;
            fake.lose_handle_on_next_failure = true;
        }
        let sparse = event(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Move,
            32768,
            32768,
        );
        assert_eq!(
            backend.inject(&sparse).unwrap_err().code,
            "injection_failed"
        );
        backend.inject(&sparse).unwrap();
        let fake = state.borrow();
        let replay = pen_info(&fake.frames[3].1[0]);
        assert_eq!(replay.rotation, 90);
        assert_eq!((replay.tiltX, replay.tiltY), initial_axes);
        assert_eq!(
            replay.penMask & (PEN_MASK_ROTATION | PEN_MASK_TILT_X | PEN_MASK_TILT_Y),
            PEN_MASK_ROTATION | PEN_MASK_TILT_X | PEN_MASK_TILT_Y
        );
        drop(fake);
        backend.stop().unwrap();
        assert_eq!(state.borrow().destroyed, [1, 2]);
    }

    #[test]
    fn touch_recovery_keeps_area_geometry_after_rotation_only_move() {
        let (mut backend, state) = backend();
        let mut down = event(7, PenTouchDevice::Touch, PenTouchPhase::Down, 32768, 32768);
        if let Event::PenTouch {
            contact_area_major,
            contact_area_minor,
            rotation_degrees,
            ..
        } = &mut down
        {
            *contact_area_major = Some(0.04);
            *contact_area_minor = Some(0.01);
            *rotation_degrees = Some(0);
        }
        backend.inject(&down).unwrap();
        let original_bounds = touch_info(&state.borrow().frames[0].1[0]).rcContact;
        let mut rotation_only = event(7, PenTouchDevice::Touch, PenTouchPhase::Move, 32768, 32768);
        if let Event::PenTouch {
            rotation_degrees, ..
        } = &mut rotation_only
        {
            *rotation_degrees = Some(90);
        }
        backend.inject(&rotation_only).unwrap();
        assert_eq!(
            touch_info(&state.borrow().frames[1].1[0]).touchMask,
            TOUCH_MASK_ORIENTATION
        );
        {
            let mut fake = state.borrow_mut();
            fake.fail_next_injection = true;
            fake.lose_handle_on_next_failure = true;
        }
        let sparse = event(7, PenTouchDevice::Touch, PenTouchPhase::Move, 32768, 32768);
        assert_eq!(
            backend.inject(&sparse).unwrap_err().code,
            "injection_failed"
        );
        backend.inject(&sparse).unwrap();
        let fake = state.borrow();
        let replay = touch_info(&fake.frames[2].1[0]);
        assert_eq!(replay.orientation, 90);
        assert_eq!(replay.rcContact.left, original_bounds.left);
        assert_eq!(replay.rcContact.top, original_bounds.top);
        assert_eq!(replay.rcContact.right, original_bounds.right);
        assert_eq!(replay.rcContact.bottom, original_bounds.bottom);
        assert_eq!(
            replay.touchMask & (TOUCH_MASK_CONTACTAREA | TOUCH_MASK_ORIENTATION),
            TOUCH_MASK_CONTACTAREA | TOUCH_MASK_ORIENTATION
        );
        drop(fake);
        backend.stop().unwrap();
        assert_eq!(state.borrow().destroyed, [1, 2]);
    }

    #[test]
    fn failed_replacement_keeps_uncertain_down_for_later_recovery_or_stop() {
        let (mut backend, state) = backend();
        {
            let mut fake = state.borrow_mut();
            fake.fail_next_injection = true;
            fake.lose_handle_on_next_failure = true;
        }
        let down = event(7, PenTouchDevice::Touch, PenTouchPhase::Down, 10, 20);
        assert_eq!(backend.inject(&down).unwrap_err().code, "injection_failed");
        state.borrow_mut().fail_next_device_creation = true;
        let moved = event(7, PenTouchDevice::Touch, PenTouchPhase::Move, 11, 21);
        assert_eq!(
            backend.inject(&moved).unwrap_err().code,
            "device_creation_failed"
        );
        assert!(backend.touch.as_ref().unwrap().handle.is_null());
        assert!(
            backend
                .touch
                .as_ref()
                .unwrap()
                .uncertain_contacts
                .contains(&7)
        );
        assert!(backend.touch.as_ref().unwrap().contacts.contains_key(&7));
        backend.inject(&moved).unwrap();
        assert_eq!(backend.touch.as_ref().unwrap().contacts[&7].x, 11);
        backend.stop().unwrap();
        assert_eq!(state.borrow().destroyed, vec![1, 2]);

        let (mut backend, state) = self::backend();
        state.borrow_mut().fail_next_injection = true;
        assert!(backend.inject(&down).is_err());
        state.borrow_mut().fail_next_device_creation = true;
        assert!(backend.inject(&moved).is_err());
        backend.stop().unwrap();
        assert!(backend.touch.is_none());
        assert_eq!(state.borrow().destroyed, vec![1]);
    }

    #[test]
    fn active_contacts_refresh_at_fifty_milliseconds_using_saved_coordinates() {
        let (mut backend, state) = backend();
        backend
            .inject(&event(
                PEN_POINTER_ID,
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
        assert_eq!(refresh.pointerInfo.pointerId, NATIVE_PEN_POINTER_ID);
        assert_eq!(refresh.pointerInfo.ptPixelLocation.x, 960);
        assert_eq!(refresh.pointerInfo.ptPixelLocation.y, 1079);
        assert_eq!(
            refresh.pointerInfo.pointerFlags,
            POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_UPDATE
        );
    }

    #[test]
    fn failed_pen_keepalive_does_not_skip_healthy_touch_device() {
        let (mut backend, state) = backend();
        backend
            .inject(&event(
                PEN_POINTER_ID,
                PenTouchDevice::Pen,
                PenTouchPhase::Down,
                100,
                200,
            ))
            .unwrap();
        backend
            .inject(&event(
                42,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                300,
                400,
            ))
            .unwrap();

        let deadline = backend.next_keepalive.unwrap();
        {
            let mut fake = state.borrow_mut();
            fake.fail_next_injection = true;
            fake.lose_handle_on_next_failure = true;
        }
        assert_eq!(
            backend.advance(deadline).unwrap_err().code,
            "injection_failed"
        );
        {
            let state = state.borrow();
            assert_eq!(state.frames.len(), 3);
            assert_eq!(state.frames[2].0, PenTouchDevice::Touch);
            let refresh = touch_info(&state.frames[2].1[0]);
            assert_eq!(refresh.pointerInfo.pointerId, 42);
            assert_eq!(
                refresh.pointerInfo.pointerFlags,
                POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_UPDATE
            );
        }

        backend.advance(backend.next_keepalive.unwrap()).unwrap();
        let state = state.borrow();
        assert_eq!(state.frames.len(), 6);
        assert_eq!(state.frames[3].0, PenTouchDevice::Pen);
        assert_eq!(state.frames[4].0, PenTouchDevice::Pen);
        assert_eq!(state.frames[5].0, PenTouchDevice::Touch);
        assert_eq!(state.destroyed, vec![1]);
    }

    #[test]
    fn failed_touch_keepalive_rebuilds_confirmed_contacts_without_blocking_pen() {
        let (mut backend, state) = backend();
        backend
            .inject(&event(
                7,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                10,
                20,
            ))
            .unwrap();
        backend
            .inject(&event(
                8,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                30,
                40,
            ))
            .unwrap();
        let deadline = backend.next_keepalive.unwrap();
        {
            let mut fake = state.borrow_mut();
            fake.fail_next_injection = true;
            fake.lose_handle_on_next_failure = true;
        }
        assert_eq!(
            backend.advance(deadline).unwrap_err().code,
            "injection_failed"
        );
        assert!(backend.touch.as_ref().unwrap().recovery_required);
        backend
            .inject(&event(
                PEN_POINTER_ID,
                PenTouchDevice::Pen,
                PenTouchPhase::Down,
                50,
                60,
            ))
            .unwrap();
        backend.advance(backend.next_keepalive.unwrap()).unwrap();
        assert_eq!(state.borrow().destroyed, vec![1]);
        assert_eq!(backend.touch.as_ref().unwrap().contacts.len(), 2);
        assert!(!backend.touch.as_ref().unwrap().recovery_required);
        assert!(
            backend
                .pen
                .as_ref()
                .unwrap()
                .contacts
                .contains_key(&PEN_POINTER_ID)
        );
        backend.stop().unwrap();
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
            .inject(&event(
                PEN_POINTER_ID,
                PenTouchDevice::Pen,
                PenTouchPhase::Down,
                50,
                60,
            ))
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
        assert_eq!(pen_cancel.pointerInfo.pointerId, NATIVE_PEN_POINTER_ID);
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
    fn failed_shutdown_cancel_releases_device_and_allows_reacquisition() {
        let (mut backend, state) = backend();
        backend
            .inject(&event(
                7,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                10,
                20,
            ))
            .unwrap();
        backend
            .inject(&event(
                7,
                PenTouchDevice::Touch,
                PenTouchPhase::Move,
                11,
                21,
            ))
            .unwrap();

        state.borrow_mut().fail_next_injection = true;
        backend.stop().unwrap();
        assert!(backend.touch.is_none());
        assert_eq!(state.borrow().destroyed, vec![1]);

        backend.start().unwrap();
        backend
            .inject(&event(
                7,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                30,
                40,
            ))
            .unwrap();
        backend
            .inject(&event(
                7,
                PenTouchDevice::Touch,
                PenTouchPhase::Move,
                31,
                41,
            ))
            .unwrap();
        backend.stop().unwrap();

        let state = state.borrow();
        assert_eq!(state.created.len(), 2);
        assert_eq!(state.destroyed, vec![1, 2]);
        assert_eq!(state.frames.len(), 5);
    }

    #[test]
    fn normalized_coordinates_are_relative_to_virtual_desktop_top_left() {
        let virtual_desktop = VirtualDesktop::new(-1920, -1080, 3840, 2160).unwrap();
        assert_eq!(virtual_desktop.map(0, 0).unwrap(), (0, 0));
        assert_eq!(
            virtual_desktop.map(u16::MAX, u16::MAX).unwrap(),
            (3839, 2159)
        );
        assert_eq!(virtual_desktop.map(32768, 32768).unwrap(), (1920, 1080));
        assert_eq!(
            virtual_desktop.screen_position(0, 0).unwrap(),
            (-1920, -1080)
        );
        assert!(VirtualDesktop::new(0, 0, 0, 1080).is_err());
        assert!(VirtualDesktop::new(i32::MAX, 0, 1, 1).is_err());
        assert!(map_normalized_coordinate(0, 0).is_err());
    }

    #[test]
    fn touch_contact_bounds_clip_to_negative_virtual_desktop_origin() {
        let (mut backend, state) = backend();
        let virtual_desktop = VirtualDesktop::new(-1920, -1080, 3840, 2160).unwrap();
        state.borrow_mut().desktop = Some(virtual_desktop);

        let mut down = event(5, PenTouchDevice::Touch, PenTouchPhase::Down, 0, 0);
        if let Event::PenTouch {
            contact_area_major,
            contact_area_minor,
            ..
        } = &mut down
        {
            *contact_area_major = Some(0.001);
            *contact_area_minor = Some(0.0005);
        }
        backend.inject(&down).unwrap();

        let frames = &state.borrow().frames;
        let touch = touch_info(&frames[0].1[0]);
        assert_eq!(touch.pointerInfo.ptPixelLocation.x, 0);
        assert_eq!(touch.pointerInfo.ptPixelLocation.y, 0);
        assert_eq!(touch.rcContact.left, -1920);
        assert_eq!(touch.rcContact.top, -1080);
        assert!(touch.rcContact.right > touch.rcContact.left);
        assert!(touch.rcContact.bottom > touch.rcContact.top);
        assert!(touch.rcContact.right <= 1920);
        assert!(touch.rcContact.bottom <= 1080);
    }
}
