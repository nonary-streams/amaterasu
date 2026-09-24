use crate::gamepad::{
    BACK_BUTTON_MASK, ControllerSnapshot, GUIDE_BUTTON_MASK, GamepadDevice, GamepadSlots,
};
use crate::gamepad_profile::GamepadProfile;
use crate::model::{Event, InputError, InputResult};
use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::c_void;
use std::mem::{size_of, transmute_copy};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::Instant;
use windows_sys::Win32::Foundation::{FreeLibrary, GetLastError, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
    LoadLibraryExW,
};

const VIGEM_CLIENT_DLL: &str = "ViGEmClient.dll";
const VIGEM_ERROR_NONE: u32 = 0x2000_0000;

type VigemClient = *mut c_void;
type VigemTarget = *mut c_void;
type VigemError = u32;

type VigemAlloc = unsafe extern "C" fn() -> VigemClient;
type VigemFree = unsafe extern "C" fn(VigemClient);
type VigemConnect = unsafe extern "C" fn(VigemClient) -> VigemError;
type VigemDisconnect = unsafe extern "C" fn(VigemClient);
type VigemTargetX360Alloc = unsafe extern "C" fn() -> VigemTarget;
type VigemTargetFree = unsafe extern "C" fn(VigemTarget);
type VigemTargetAdd = unsafe extern "C" fn(VigemClient, VigemTarget) -> VigemError;
type VigemTargetRemove = unsafe extern "C" fn(VigemClient, VigemTarget) -> VigemError;
type VigemTargetX360Update =
    unsafe extern "C" fn(VigemClient, VigemTarget, XusbReport) -> VigemError;
type VigemTargetDs4Alloc = unsafe extern "C" fn() -> VigemTarget;
type VigemTargetDs4Update = unsafe extern "C" fn(VigemClient, VigemTarget, Ds4Report) -> VigemError;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct XusbReport {
    buttons: u16,
    left_trigger: u8,
    right_trigger: u8,
    left_x: i16,
    left_y: i16,
    right_x: i16,
    right_y: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Ds4Report {
    left_x: u8,
    left_y: u8,
    right_x: u8,
    right_y: u8,
    buttons: u16,
    special_buttons: u8,
    left_trigger: u8,
    right_trigger: u8,
    padding: u8,
}

const DS4_BUTTON_SHARE: u16 = 0x1000;
const DS4_BUTTON_OPTIONS: u16 = 0x2000;
const DS4_BUTTON_L3: u16 = 0x4000;
const DS4_BUTTON_R3: u16 = 0x8000;
const DS4_BUTTON_L2: u16 = 0x0400;
const DS4_BUTTON_R2: u16 = 0x0800;
const DS4_SPECIAL_PS: u8 = 0x01;
const DS4_SPECIAL_TOUCHPAD: u8 = 0x02;

#[derive(Clone, Copy)]
struct VigemFunctions {
    alloc: VigemAlloc,
    free: VigemFree,
    connect: VigemConnect,
    disconnect: VigemDisconnect,
    target_x360_alloc: VigemTargetX360Alloc,
    target_free: VigemTargetFree,
    target_add: VigemTargetAdd,
    target_remove: VigemTargetRemove,
    target_x360_update: VigemTargetX360Update,
    target_ds4_alloc: Option<VigemTargetDs4Alloc>,
    target_ds4_update: Option<VigemTargetDs4Update>,
}

struct VigemConnection {
    module: HMODULE,
    client: NonNull<c_void>,
    functions: VigemFunctions,
    allocated_targets: RefCell<HashSet<NonNull<c_void>>>,
}

impl VigemConnection {
    fn connect_default() -> InputResult<Self> {
        Self::connect(VIGEM_CLIENT_DLL)
    }

    fn connect(dll_name: &str) -> InputResult<Self> {
        let wide_name: Vec<u16> = Path::new(dll_name)
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // Search only the application directory and System32, avoiding the
        // current directory and PATH for this native driver client library.
        let module = unsafe {
            LoadLibraryExW(
                wide_name.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            return Err(InputError::new(
                "controller_unavailable",
                format!("could not load {dll_name} (Windows error {})", unsafe {
                    GetLastError()
                }),
            ));
        }

        let functions = match unsafe { VigemFunctions::load(module) } {
            Ok(functions) => functions,
            Err(error) => {
                // SAFETY: `module` is the valid reference returned by the
                // matching LoadLibraryExW call above.
                unsafe { FreeLibrary(module) };
                return Err(error);
            }
        };

        // SAFETY: function pointers were resolved from the documented
        // ViGEmClient exports and the client pointer is owned by this connection.
        let client = NonNull::new(unsafe { (functions.alloc)() }).ok_or_else(|| {
            // SAFETY: module is still owned here; no client was allocated.
            unsafe { FreeLibrary(module) };
            InputError::new(
                "controller_unavailable",
                "ViGEmClient could not allocate a client",
            )
        })?;
        // SAFETY: the client was allocated above and has not been connected before.
        let status = unsafe { (functions.connect)(client.as_ptr()) };
        if !vigem_succeeded(status) {
            // SAFETY: the allocated client and library are still valid. A failed
            // connect is never passed to disconnect, matching ViGEmClient's API.
            unsafe {
                (functions.free)(client.as_ptr());
                FreeLibrary(module);
            }
            return Err(vigem_error("connect", status));
        }

        Ok(Self {
            module,
            client,
            functions,
            allocated_targets: RefCell::new(HashSet::new()),
        })
    }

    fn create_x360_target(&self) -> InputResult<NonNull<c_void>> {
        // SAFETY: the loaded SDK function allocates a target object owned by the
        // caller. The returned pointer stays paired with this connection.
        let target =
            NonNull::new(unsafe { (self.functions.target_x360_alloc)() }).ok_or_else(|| {
                InputError::new(
                    "controller_unavailable",
                    "ViGEm could not allocate an Xbox 360 target",
                )
            })?;
        // SAFETY: both handles are live objects from this ViGEmClient instance.
        let status = unsafe { (self.functions.target_add)(self.client.as_ptr(), target.as_ptr()) };
        if !vigem_succeeded(status) {
            // SAFETY: target_add failed, so this target was not attached to the
            // driver and can be freed with the matching SDK allocator.
            unsafe { (self.functions.target_free)(target.as_ptr()) };
            return Err(vigem_error("target_add", status));
        }
        self.allocated_targets.borrow_mut().insert(target);
        Ok(target)
    }

    fn supports_ds4(&self) -> bool {
        self.functions.target_ds4_alloc.is_some() && self.functions.target_ds4_update.is_some()
    }

    fn create_ds4_target(&self) -> InputResult<NonNull<c_void>> {
        let allocate = self.functions.target_ds4_alloc.ok_or_else(|| {
            InputError::new(
                "controller_unavailable",
                "ViGEmClient does not provide DS4 target support",
            )
        })?;
        // SAFETY: the ViGEm SDK allocator returns a target object owned by the
        // caller; it remains paired with this live connection through release.
        let target = NonNull::new(unsafe { allocate() }).ok_or_else(|| {
            InputError::new(
                "controller_unavailable",
                "ViGEm could not allocate a DualShock 4 target",
            )
        })?;
        // SAFETY: the client and target are live objects from this ViGEm SDK.
        let status = unsafe { (self.functions.target_add)(self.client.as_ptr(), target.as_ptr()) };
        if !vigem_succeeded(status) {
            // SAFETY: a failed target_add leaves this target detached and owned
            // by the caller, so the matching SDK deallocator can free it.
            unsafe { (self.functions.target_free)(target.as_ptr()) };
            return Err(vigem_error("target_add", status));
        }
        self.allocated_targets.borrow_mut().insert(target);
        Ok(target)
    }

    fn update_x360(&self, target: NonNull<c_void>, state: ControllerSnapshot) -> InputResult<()> {
        let report = xusb_report(state);
        // SAFETY: the client and target are live, target was attached through
        // this client, and XusbReport matches the SDK's 12-byte XUSB_REPORT ABI.
        let status = unsafe {
            (self.functions.target_x360_update)(self.client.as_ptr(), target.as_ptr(), report)
        };
        if !vigem_succeeded(status) {
            return Err(vigem_error("x360_update", status));
        }
        Ok(())
    }

    fn update_ds4(
        &self,
        target: NonNull<c_void>,
        state: ControllerSnapshot,
        back_as_touchpad_click: bool,
    ) -> InputResult<()> {
        let update = self.functions.target_ds4_update.ok_or_else(|| {
            InputError::new(
                "controller_unavailable",
                "ViGEmClient does not provide DS4 report support",
            )
        })?;
        let report = ds4_report(state, back_as_touchpad_click);
        // SAFETY: the client and target are live objects attached through this
        // connection, and Ds4Report matches the SDK's 10-byte DS4_REPORT ABI.
        let status = unsafe { update(self.client.as_ptr(), target.as_ptr(), report) };
        if !vigem_succeeded(status) {
            return Err(vigem_error("ds4_update", status));
        }
        Ok(())
    }

    fn remove_target(&self, target: NonNull<c_void>) -> InputResult<()> {
        // SAFETY: target is still allocated and belongs to this live connection.
        let status =
            unsafe { (self.functions.target_remove)(self.client.as_ptr(), target.as_ptr()) };
        if !vigem_succeeded(status) {
            return Err(vigem_error("target_remove", status));
        }
        // ViGEmClient documents that successful removal detaches the target and
        // that the target object can then be reused or freed.
        self.allocated_targets.borrow_mut().remove(&target);
        unsafe { (self.functions.target_free)(target.as_ptr()) };
        Ok(())
    }
}

impl VigemFunctions {
    unsafe fn load(module: HMODULE) -> InputResult<Self> {
        Ok(Self {
            alloc: unsafe { load_symbol(module, b"vigem_alloc\0")? },
            free: unsafe { load_symbol(module, b"vigem_free\0")? },
            connect: unsafe { load_symbol(module, b"vigem_connect\0")? },
            disconnect: unsafe { load_symbol(module, b"vigem_disconnect\0")? },
            target_x360_alloc: unsafe { load_symbol(module, b"vigem_target_x360_alloc\0")? },
            target_free: unsafe { load_symbol(module, b"vigem_target_free\0")? },
            target_add: unsafe { load_symbol(module, b"vigem_target_add\0")? },
            target_remove: unsafe { load_symbol(module, b"vigem_target_remove\0")? },
            target_x360_update: unsafe { load_symbol(module, b"vigem_target_x360_update\0")? },
            target_ds4_alloc: unsafe { load_optional_symbol(module, b"vigem_target_ds4_alloc\0") },
            target_ds4_update: unsafe {
                load_optional_symbol(module, b"vigem_target_ds4_update\0")
            },
        })
    }
}

unsafe fn load_optional_symbol<T: Copy>(module: HMODULE, name: &'static [u8]) -> Option<T> {
    debug_assert_eq!(name.last(), Some(&0));
    // SAFETY: `name` is a static NUL-terminated ViGEmClient export name and
    // `module` remains loaded for the lifetime of the resolved pointer.
    let address = unsafe { GetProcAddress(module, name.as_ptr()) }?;
    if size_of::<T>() != size_of::<Option<unsafe extern "system" fn() -> isize>>() {
        return None;
    }
    // SAFETY: the caller supplies the function-pointer alias declared for this
    // optional SDK export; absence is handled before this conversion.
    Some(unsafe { transmute_copy(&address) })
}

unsafe fn load_symbol<T: Copy>(module: HMODULE, name: &'static [u8]) -> InputResult<T> {
    debug_assert_eq!(name.last(), Some(&0));
    // SAFETY: `name` is a static NUL-terminated export name and `module` remains
    // loaded for the lifetime of all function pointers resolved from it.
    let address = unsafe { GetProcAddress(module, name.as_ptr()) }.ok_or_else(|| {
        InputError::new(
            "controller_unavailable",
            format!(
                "ViGEmClient is missing required export {}",
                String::from_utf8_lossy(&name[..name.len() - 1])
            ),
        )
    })?;
    if size_of::<T>() != size_of::<Option<unsafe extern "system" fn() -> isize>>() {
        return Err(InputError::new(
            "controller_unavailable",
            "ViGEmClient export has an unexpected function pointer size",
        ));
    }
    // SAFETY: caller selects a function-pointer alias whose signature matches
    // the corresponding public ViGEmClient declaration for this named export.
    Ok(unsafe { transmute_copy(&address) })
}

impl Drop for VigemConnection {
    fn drop(&mut self) {
        // SAFETY: this connection owns a successfully connected client. Any
        // target whose normal remove failed is still registered here; disconnect
        // removes driver-side targets before their remaining SDK objects are freed.
        unsafe {
            (self.functions.disconnect)(self.client.as_ptr());
            for target in self.allocated_targets.get_mut().drain() {
                (self.functions.target_free)(target.as_ptr());
            }
            (self.functions.free)(self.client.as_ptr());
            FreeLibrary(self.module);
        }
    }
}

struct VigemGamepad {
    connection: Rc<VigemConnection>,
    target: Option<NonNull<c_void>>,
    profile: GamepadProfile,
    ds4_back_as_touchpad_click: bool,
}

impl VigemGamepad {
    fn create(
        connection: Rc<VigemConnection>,
        profile: GamepadProfile,
        ds4_back_as_touchpad_click: bool,
    ) -> InputResult<Self> {
        let target = match profile {
            GamepadProfile::Ds4 => connection.create_ds4_target()?,
            GamepadProfile::Auto | GamepadProfile::X360 => connection.create_x360_target()?,
            _ => {
                return Err(InputError::new(
                    "controller_unavailable",
                    "requested gamepad profile is not supported by the ViGEm backend",
                ));
            }
        };
        Ok(Self {
            connection,
            target: Some(target),
            profile,
            ds4_back_as_touchpad_click,
        })
    }

    fn target(&self) -> InputResult<NonNull<c_void>> {
        self.target.ok_or_else(|| {
            InputError::new(
                "controller_unavailable",
                "ViGEm gamepad target was already released",
            )
        })
    }
}

impl GamepadDevice for VigemGamepad {
    fn update(&mut self, state: ControllerSnapshot) -> InputResult<()> {
        match self.profile {
            GamepadProfile::Ds4 => {
                self.connection
                    .update_ds4(self.target()?, state, self.ds4_back_as_touchpad_click)
            }
            GamepadProfile::Auto | GamepadProfile::X360 => {
                self.connection.update_x360(self.target()?, state)
            }
            _ => Err(InputError::new(
                "controller_unavailable",
                "requested gamepad profile is not supported by the ViGEm backend",
            )),
        }
    }

    fn set_ds4_back_as_touchpad_click(&mut self, enabled: bool) {
        self.ds4_back_as_touchpad_click = enabled;
    }

    fn release(&mut self) -> InputResult<()> {
        if let Some(target) = self.target {
            // Detach even if a neutral report is rejected. Successful removal
            // is the stronger cleanup guarantee because the virtual device is
            // no longer connected to applications.
            match self.profile {
                GamepadProfile::Ds4 => {
                    let _ = self.connection.update_ds4(
                        target,
                        ControllerSnapshot::default(),
                        self.ds4_back_as_touchpad_click,
                    );
                }
                GamepadProfile::Auto | GamepadProfile::X360 => {
                    let _ = self
                        .connection
                        .update_x360(target, ControllerSnapshot::default());
                }
                _ => {}
            }
            self.connection.remove_target(target)?;
            self.target = None;
        }
        Ok(())
    }
}

impl Drop for VigemGamepad {
    fn drop(&mut self) {
        // Final fallback for process teardown after normal lease retries. Keep
        // the client alive through removal/free; ViGEm disconnect also clears
        // attached devices if the bus rejects this best-effort removal.
        let _ = self.release();
    }
}

pub(super) struct WindowsGamepads {
    slots: GamepadSlots<VigemGamepad>,
    connection: Option<Rc<VigemConnection>>,
    unavailable_reason: Option<String>,
    profile: GamepadProfile,
    target_profile: GamepadProfile,
    effective_profile: Option<&'static str>,
    back_button_timeout_ms: i32,
    ds4_back_as_touchpad_click: bool,
}

impl WindowsGamepads {
    pub(super) fn new() -> Self {
        Self {
            slots: GamepadSlots::new(),
            connection: None,
            unavailable_reason: None,
            profile: GamepadProfile::Auto,
            target_profile: GamepadProfile::X360,
            effective_profile: None,
            back_button_timeout_ms: -1,
            ds4_back_as_touchpad_click: true,
        }
    }

    pub(super) fn start(&mut self) -> InputResult<()> {
        match self.profile {
            GamepadProfile::Auto | GamepadProfile::X360 => {
                self.start_with_profile(GamepadProfile::X360, VigemConnection::connect_default)
            }
            GamepadProfile::Ds4 => {
                self.start_with_profile(GamepadProfile::Ds4, VigemConnection::connect_default)
            }
            unsupported => {
                if !self.slots.is_empty() || self.connection.is_some() {
                    return Err(InputError::new(
                        "backend_busy",
                        "Windows ViGEm gamepad resources are still active",
                    ));
                }
                self.effective_profile = None;
                self.unavailable_reason = Some(format!(
                    "Windows gamepad profile '{}' is recognized but not implemented by this backend",
                    unsupported.as_str()
                ));
                Ok(())
            }
        }
    }

    fn start_with(
        &mut self,
        probe: impl FnOnce() -> InputResult<VigemConnection>,
    ) -> InputResult<()> {
        let target_profile = if self.profile == GamepadProfile::Ds4 {
            GamepadProfile::Ds4
        } else {
            GamepadProfile::X360
        };
        self.start_with_profile(target_profile, probe)
    }

    fn start_with_profile(
        &mut self,
        target_profile: GamepadProfile,
        probe: impl FnOnce() -> InputResult<VigemConnection>,
    ) -> InputResult<()> {
        if !self.slots.is_empty() || self.connection.is_some() {
            return Err(InputError::new(
                "backend_busy",
                "Windows ViGEm gamepad resources are still active",
            ));
        }
        self.unavailable_reason = None;
        self.effective_profile = None;
        match probe() {
            Ok(connection)
                if target_profile == GamepadProfile::Ds4 && !connection.supports_ds4() =>
            {
                self.unavailable_reason = Some(
                    "ViGEmClient is missing the DS4 target allocation or report export".into(),
                );
            }
            Ok(connection) => {
                self.target_profile = target_profile;
                self.connection = Some(Rc::new(connection));
                self.effective_profile = Some(target_profile.as_str());
            }
            Err(error) => self.unavailable_reason = Some(error.message),
        }
        Ok(())
    }

    pub(super) fn supported(&self) -> bool {
        self.connection.is_some()
    }

    pub(super) fn effective_profile(&self) -> Option<&'static str> {
        self.effective_profile
    }

    pub(super) fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable_reason.as_deref().or(match self.profile {
            GamepadProfile::Auto | GamepadProfile::X360 => None,
            GamepadProfile::Ds4 => {
                Some("Windows DS4 input requires ViGEmClient DS4 report support")
            }
            GamepadProfile::Vhf => Some("Windows gamepad profile 'vhf' is recognized but not implemented by this backend"),
            GamepadProfile::VhfXbox => Some("Windows gamepad profile 'vhf_xbox' is recognized but not implemented by this backend"),
            GamepadProfile::VhfXboxOne => Some("Windows gamepad profile 'vhf_xbox_one' is recognized but not implemented by this backend"),
            GamepadProfile::VhfDs4 => Some("Windows gamepad profile 'vhf_ds4' is recognized but not implemented by this backend"),
            GamepadProfile::VhfDs5 => Some("Windows gamepad profile 'vhf_ds5' is recognized but not implemented by this backend"),
            GamepadProfile::VhfSwitch => Some("Windows gamepad profile 'vhf_switch' is recognized but not implemented by this backend"),
        })
    }

    pub(super) fn set_profile(&mut self, profile: GamepadProfile) {
        self.profile = profile;
    }

    pub(super) fn disable(&mut self) -> InputResult<()> {
        self.slots.stop()
    }

    pub(super) fn validate(&self, event: &Event) -> InputResult<()> {
        if matches!(event, Event::Controller { .. }) && !self.supported() {
            return Err(InputError::new(
                "controller_unavailable",
                self.unavailable_reason.as_deref().unwrap_or(
                    "ViGEm X360 input is unavailable; install ViGEmBus and provide ViGEmClient.dll",
                ),
            ));
        }
        Ok(())
    }

    pub(super) fn inject(&mut self, event: &Event) -> InputResult<()> {
        self.validate(event)?;
        let Event::Controller {
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
        else {
            return Err(InputError::new(
                "unsupported_event",
                "Windows gamepad backend accepts only controller snapshots",
            ));
        };
        let state = ControllerSnapshot {
            buttons: *buttons,
            left_trigger: *left_trigger,
            right_trigger: *right_trigger,
            left_x: *left_x,
            left_y: *left_y,
            right_x: *right_x,
            right_y: *right_y,
        };
        let connection = self
            .connection
            .as_ref()
            .expect("validated ViGEm connection")
            .clone();
        let target_profile = self.target_profile;
        let ds4_back_as_touchpad_click = self.ds4_back_as_touchpad_click;
        self.slots
            .apply(
                *number,
                *active_mask,
                state,
                self.back_button_timeout_ms,
                Instant::now(),
                |_slot| {
                    VigemGamepad::create(
                        connection.clone(),
                        target_profile,
                        ds4_back_as_touchpad_click,
                    )
                },
            )
            .map_err(|error| InputError::new("controller_unavailable", error.message))
    }

    pub(super) fn advance(&mut self, now: Instant) {
        // A timer write failure remains dirty in GamepadSlots and is retried
        // on later module ticks without tearing down keyboard/mouse injection.
        let _ = self.slots.advance(now);
    }

    pub(super) fn set_back_button_timeout(&mut self, timeout_ms: i32) {
        self.back_button_timeout_ms = timeout_ms;
        if timeout_ms < 0 {
            self.slots.disable_pending_back_timeouts();
        }
    }

    pub(super) fn set_ds4_back_as_touchpad_click(&mut self, enabled: bool) {
        self.ds4_back_as_touchpad_click = enabled;
        self.slots.set_ds4_back_as_touchpad_click(enabled);
    }

    pub(super) fn stop(&mut self) -> InputResult<()> {
        self.slots.stop()?;
        self.connection = None;
        self.unavailable_reason = None;
        self.effective_profile = None;
        Ok(())
    }
}

fn xusb_report(state: ControllerSnapshot) -> XusbReport {
    let buttons = [
        (1 << 0, 0x0001), // D-pad up
        (1 << 1, 0x0002), // D-pad down
        (1 << 2, 0x0004), // D-pad left
        (1 << 3, 0x0008), // D-pad right
        (1 << 4, 0x0010), // Start
        (BACK_BUTTON_MASK, 0x0020),
        (1 << 6, 0x0040), // Left stick click
        (1 << 7, 0x0080), // Right stick click
        (1 << 8, 0x0100), // Left shoulder
        (1 << 9, 0x0200), // Right shoulder
        (GUIDE_BUTTON_MASK, 0x0400),
        (1 << 21, 0x0400), // Misc button shares XUSB Guide
        (1 << 12, 0x1000), // South / A
        (1 << 13, 0x2000), // East / B
        (1 << 14, 0x4000), // West / X
        (1 << 15, 0x8000), // North / Y
    ]
    .into_iter()
    .filter(|(mask, _)| state.buttons & mask != 0)
    .fold(0, |mapped, (_, target)| mapped | target);

    XusbReport {
        buttons,
        left_trigger: state.left_trigger,
        right_trigger: state.right_trigger,
        left_x: state.left_x,
        left_y: state.left_y,
        right_x: state.right_x,
        right_y: state.right_y,
    }
}

fn ds4_report(state: ControllerSnapshot, back_as_touchpad_click: bool) -> Ds4Report {
    let up = state.buttons & (1 << 0) != 0;
    let down = state.buttons & (1 << 1) != 0;
    let left = state.buttons & (1 << 2) != 0;
    let right = state.buttons & (1 << 3) != 0;
    let dpad = if up {
        if right {
            1
        } else if left {
            7
        } else {
            0
        }
    } else if down {
        if right {
            3
        } else if left {
            5
        } else {
            4
        }
    } else if right {
        2
    } else if left {
        6
    } else {
        0x08
    };
    let buttons =
        dpad | if state.buttons & (1 << 4) != 0 {
            DS4_BUTTON_OPTIONS
        } else {
            0
        } | if state.buttons & BACK_BUTTON_MASK != 0 {
            DS4_BUTTON_SHARE
        } else {
            0
        } | if state.buttons & (1 << 6) != 0 {
            DS4_BUTTON_L3
        } else {
            0
        } | if state.buttons & (1 << 7) != 0 {
            DS4_BUTTON_R3
        } else {
            0
        } | if state.buttons & (1 << 8) != 0 {
            0x0100
        } else {
            0
        } | if state.buttons & (1 << 9) != 0 {
            0x0200
        } else {
            0
        } | if state.left_trigger > 0 {
            DS4_BUTTON_L2
        } else {
            0
        } | if state.right_trigger > 0 {
            DS4_BUTTON_R2
        } else {
            0
        } | if state.buttons & (1 << 12) != 0 {
            0x0020
        } else {
            0
        } | if state.buttons & (1 << 13) != 0 {
            0x0040
        } else {
            0
        } | if state.buttons & (1 << 14) != 0 {
            0x0010
        } else {
            0
        } | if state.buttons & (1 << 15) != 0 {
            0x0080
        } else {
            0
        };
    let mut special_buttons = if state.buttons & GUIDE_BUTTON_MASK != 0 {
        DS4_SPECIAL_PS
    } else {
        0
    };
    if state.buttons & ((1 << 20) | (1 << 21)) != 0
        || (back_as_touchpad_click && state.buttons & BACK_BUTTON_MASK != 0)
    {
        special_buttons |= DS4_SPECIAL_TOUCHPAD;
    }

    Ds4Report {
        left_x: ds4_axis_x(state.left_x),
        left_y: ds4_axis_y(state.left_y),
        right_x: ds4_axis_x(state.right_x),
        right_y: ds4_axis_y(state.right_y),
        buttons,
        special_buttons,
        left_trigger: state.left_trigger,
        right_trigger: state.right_trigger,
        padding: 0,
    }
}

fn ds4_axis_x(value: i16) -> u8 {
    ((i32::from(value) + 32_768) / 257) as u8
}

fn ds4_axis_y(value: i16) -> u8 {
    let inverted = -((32_767 + i32::from(value) - 1) / 257);
    if inverted == 0 {
        u8::MAX
    } else {
        inverted as u8
    }
}

fn vigem_error(operation: &str, status: VigemError) -> InputError {
    InputError::new(
        "controller_unavailable",
        format!("ViGEm {operation} failed with status 0x{status:08X}"),
    )
}

const fn vigem_succeeded(status: VigemError) -> bool {
    status == VIGEM_ERROR_NONE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xusb_snapshot_maps_supported_buttons_axes_and_triggers() {
        let report = xusb_report(ControllerSnapshot {
            buttons: 0x0001_f7ff,
            left_trigger: 17,
            right_trigger: 251,
            left_x: i16::MIN,
            left_y: i16::MAX,
            right_x: -1234,
            right_y: 4321,
        });
        assert_eq!(report.buttons, 0xf7ff);
        assert_eq!(report.left_trigger, 17);
        assert_eq!(report.right_trigger, 251);
        assert_eq!(report.left_x, i16::MIN);
        assert_eq!(report.left_y, i16::MAX);
        assert_eq!(report.right_x, -1234);
        assert_eq!(report.right_y, 4321);
        assert_eq!(size_of::<XusbReport>(), 12);
    }

    #[test]
    fn ds4_report_maps_standard_buttons_special_controls_axes_and_triggers() {
        let report = ds4_report(
            ControllerSnapshot {
                buttons: (1 << 4)
                    | BACK_BUTTON_MASK
                    | (1 << 6)
                    | (1 << 7)
                    | (1 << 8)
                    | (1 << 9)
                    | GUIDE_BUTTON_MASK
                    | (1 << 12)
                    | (1 << 13)
                    | (1 << 14)
                    | (1 << 15),
                left_trigger: 17,
                right_trigger: 251,
                left_x: i16::MIN,
                left_y: i16::MAX,
                right_x: 0,
                right_y: 0,
            },
            true,
        );

        assert_eq!(report.buttons, 0xfff8);
        assert_eq!(
            report.special_buttons,
            DS4_SPECIAL_PS | DS4_SPECIAL_TOUCHPAD
        );
        assert_eq!(report.left_x, 0);
        assert_eq!(report.left_y, 2);
        assert_eq!(report.right_x, 127);
        assert_eq!(report.right_y, 129);
        assert_eq!(report.left_trigger, 17);
        assert_eq!(report.right_trigger, 251);
        assert_eq!(report.padding, 0);
        assert_eq!(size_of::<Ds4Report>(), 10);
    }

    #[test]
    fn ds4_dpad_encodes_eight_directions_with_vertical_then_right_precedence() {
        let directions = [
            (1 << 0, 0),
            ((1 << 0) | (1 << 3), 1),
            (1 << 3, 2),
            ((1 << 1) | (1 << 3), 3),
            (1 << 1, 4),
            ((1 << 1) | (1 << 2), 5),
            (1 << 2, 6),
            ((1 << 0) | (1 << 2), 7),
            (0, 8),
            ((1 << 0) | (1 << 1), 0),
            ((1 << 2) | (1 << 3), 2),
        ];
        for (buttons, expected) in directions {
            let report = ds4_report(
                ControllerSnapshot {
                    buttons,
                    ..ControllerSnapshot::default()
                },
                false,
            );
            assert_eq!(report.buttons & 0x0f, expected);
        }
    }

    #[test]
    fn ds4_back_mapping_and_client_touchpad_buttons_are_independent() {
        let back = ControllerSnapshot {
            buttons: BACK_BUTTON_MASK,
            ..ControllerSnapshot::default()
        };
        let without_mapping = ds4_report(back, false);
        let with_mapping = ds4_report(back, true);
        assert_eq!(without_mapping.buttons & DS4_BUTTON_SHARE, DS4_BUTTON_SHARE);
        assert_eq!(without_mapping.special_buttons, 0);
        assert_eq!(with_mapping.special_buttons, DS4_SPECIAL_TOUCHPAD);

        let touchpad = ds4_report(
            ControllerSnapshot {
                buttons: (1 << 20) | (1 << 21),
                ..ControllerSnapshot::default()
            },
            false,
        );
        assert_eq!(touchpad.special_buttons, DS4_SPECIAL_TOUCHPAD);
    }

    #[test]
    fn paddle_and_touchpad_extension_bits_are_not_misrepresented_as_xusb_buttons() {
        let report = xusb_report(ControllerSnapshot {
            buttons: (1 << 11) | (0x1f << 16),
            ..ControllerSnapshot::default()
        });
        assert_eq!(report.buttons, 0);
    }

    #[test]
    fn misc_button_maps_to_the_xusb_guide_control() {
        let report = xusb_report(ControllerSnapshot {
            buttons: 1 << 21,
            ..ControllerSnapshot::default()
        });
        assert_eq!(report.buttons, 0x0400);
    }

    #[test]
    fn missing_vigem_client_dll_reports_probe_failure() {
        let error = match VigemConnection::connect("AmaterasuMissingViGEmClient.dll") {
            Ok(_) => panic!("test DLL unexpectedly loaded"),
            Err(error) => error,
        };
        assert_eq!(error.code, "controller_unavailable");
        assert!(error.message.contains("could not load"));
    }

    #[test]
    fn failed_driver_probe_leaves_controller_unavailable_with_an_explicit_error() {
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with(|| {
                Err(InputError::new(
                    "controller_unavailable",
                    "simulated missing ViGEm driver",
                ))
            })
            .unwrap();

        assert!(!gamepads.supported());
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
        let error = gamepads.validate(&event).unwrap_err();
        assert_eq!(error.code, "controller_unavailable");
        assert!(error.message.contains("simulated"));
    }

    #[test]
    fn explicit_ds4_profile_without_ds4_exports_reports_unavailable() {
        let mut gamepads = WindowsGamepads::new();
        gamepads.set_profile(GamepadProfile::Ds4);
        gamepads
            .start_with_profile(GamepadProfile::Ds4, || {
                Err(InputError::new(
                    "controller_unavailable",
                    "simulated missing DS4 report exports",
                ))
            })
            .unwrap();

        assert!(!gamepads.supported());
        assert_eq!(gamepads.effective_profile(), None);
        assert!(
            gamepads
                .unavailable_reason()
                .unwrap()
                .contains("simulated missing DS4")
        );
        let event = Event::Controller {
            number: 0,
            active_mask: 1,
            buttons: 1,
            left_trigger: 0,
            right_trigger: 0,
            left_x: 0,
            left_y: 0,
            right_x: 0,
            right_y: 0,
        };
        assert_eq!(
            gamepads.validate(&event).unwrap_err().code,
            "controller_unavailable"
        );
        gamepads.stop().unwrap();
        assert!(
            gamepads
                .unavailable_reason()
                .unwrap()
                .contains("DS4 report support")
        );
    }

    #[test]
    fn auto_profile_invokes_the_real_backend_probe() {
        let mut gamepads = WindowsGamepads::new();
        let mut probed = false;
        gamepads
            .start_with(|| {
                probed = true;
                Err(InputError::new(
                    "controller_unavailable",
                    "test driver probe failure",
                ))
            })
            .unwrap();

        assert!(probed);
        assert!(!gamepads.supported());
        assert_eq!(gamepads.effective_profile(), None);
        assert_eq!(
            gamepads.unavailable_reason(),
            Some("test driver probe failure")
        );
    }

    #[test]
    fn vigem_status_uses_the_sdk_success_code_instead_of_zero() {
        assert!(vigem_succeeded(VIGEM_ERROR_NONE));
        assert!(!vigem_succeeded(0));
        assert!(!vigem_succeeded(VIGEM_ERROR_NONE + 1));
    }
}
