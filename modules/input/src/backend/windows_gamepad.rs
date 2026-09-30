use crate::gamepad::{
    BACK_BUTTON_MASK, ControllerSnapshot, GUIDE_BUTTON_MASK, GamepadDevice, GamepadSlots,
};
use crate::gamepad_profile::GamepadProfile;
use crate::model::{
    ControllerBatteryState, ControllerMotionType, ControllerTouchPhase, Event, InputError,
    InputResult,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::{size_of, transmute_copy};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{FreeLibrary, GetLastError, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
    LoadLibraryExW,
};

const VIGEM_CLIENT_DLL: &str = "ViGEmClient.dll";
const VIGEM_ERROR_NONE: u32 = 0x2000_0000;
const VIGEM_ERROR_BUS_NOT_FOUND: u32 = 0xE000_0001;
const VIGEM_ERROR_BUS_INVALID_HANDLE: u32 = 0xE000_0013;
const VIGEM_ERROR_WINAPI: u32 = 0xE000_0017;

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
type VigemTargetDs4UpdateEx =
    unsafe extern "C" fn(VigemClient, VigemTarget, Ds4ReportEx) -> VigemError;

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

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Ds4Touch {
    packet_counter: u8,
    pointer_one: u8,
    pointer_one_data: [u8; 3],
    pointer_two: u8,
    pointer_two_data: [u8; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ds4ReportEx {
    left_x: u8,
    left_y: u8,
    right_x: u8,
    right_y: u8,
    buttons: [u8; 2],
    special_buttons: u8,
    left_trigger: u8,
    right_trigger: u8,
    timestamp: [u8; 2],
    battery_level: u8,
    gyro_x: [u8; 2],
    gyro_y: [u8; 2],
    gyro_z: [u8; 2],
    accel_x: [u8; 2],
    accel_y: [u8; 2],
    accel_z: [u8; 2],
    unknown_one: [u8; 5],
    battery_special: u8,
    unknown_two: [u8; 2],
    touch_packets: u8,
    current_touch: Ds4Touch,
    previous_touches: [Ds4Touch; 2],
    trailing_padding: [u8; 3],
}

impl Default for Ds4ReportEx {
    fn default() -> Self {
        Self::neutral()
    }
}

impl Ds4ReportEx {
    fn neutral() -> Self {
        let inactive_touch = Ds4Touch {
            pointer_one: 0x80,
            pointer_two: 0x80,
            ..Ds4Touch::default()
        };
        let mut report = Self {
            left_x: 0x80,
            left_y: 0x80,
            right_x: 0x80,
            right_y: 0x80,
            buttons: u16::from(DS4_DPAD_NONE).to_le_bytes(),
            special_buttons: 0,
            left_trigger: 0,
            right_trigger: 0,
            timestamp: [0; 2],
            battery_level: 0xff,
            gyro_x: [0; 2],
            gyro_y: [0; 2],
            gyro_z: [0; 2],
            accel_x: [0; 2],
            accel_y: [0; 2],
            accel_z: [0; 2],
            unknown_one: [0; 5],
            battery_special: 0x1a,
            unknown_two: [0; 2],
            touch_packets: 1,
            current_touch: inactive_touch,
            previous_touches: [inactive_touch; 2],
            trailing_padding: [0; 3],
        };
        // ViGEm's pinned DS4 initialization establishes normal gravity before
        // the first client sensor packet arrives.
        report.set_motion(ControllerMotionType::Accel, 0.0, DS4_EARTH_G, 0.0);
        report
    }

    fn set_controller_state(&mut self, state: ControllerSnapshot, back_as_touchpad_click: bool) {
        let report = ds4_report(state, back_as_touchpad_click);
        self.left_x = report.left_x;
        self.left_y = report.left_y;
        self.right_x = report.right_x;
        self.right_y = report.right_y;
        self.buttons = report.buttons.to_le_bytes();
        self.special_buttons = report.special_buttons;
        self.left_trigger = report.left_trigger;
        self.right_trigger = report.right_trigger;
    }

    fn set_timestamp(&mut self, timestamp: u16) {
        self.timestamp = timestamp.to_le_bytes();
    }

    fn timestamp(&self) -> u16 {
        u16::from_le_bytes(self.timestamp)
    }

    fn set_motion(&mut self, motion_type: ControllerMotionType, x: f32, y: f32, z: f32) {
        match motion_type {
            ControllerMotionType::Accel => {
                self.accel_x = ds4_accel(x, -297, 1.010796).to_le_bytes();
                self.accel_y = ds4_accel(y, -42, 1.014614).to_le_bytes();
                self.accel_z = ds4_accel(z, -512, 1.024768).to_le_bytes();
            }
            ControllerMotionType::Gyro => {
                self.gyro_x = ds4_gyro(x, 1, 0.977596).to_le_bytes();
                self.gyro_y = ds4_gyro(y, 0, 0.972370).to_le_bytes();
                self.gyro_z = ds4_gyro(z, 0, 0.971550).to_le_bytes();
            }
        }
    }

    fn set_battery(&mut self, state: ControllerBatteryState, percentage: Option<u8>) {
        match state {
            ControllerBatteryState::Charging | ControllerBatteryState::Discharging => {
                if state == ControllerBatteryState::Charging {
                    self.battery_special |= 0x10;
                } else {
                    self.battery_special &= !0x10;
                }
                if self.battery_special & 0x0f > 0x0a {
                    self.battery_special = (self.battery_special & !0x0f) | 0x05;
                }
            }
            ControllerBatteryState::Full => {
                self.battery_special = 0x1b;
                self.battery_level = 0xff;
            }
            ControllerBatteryState::NotPresent | ControllerBatteryState::NotCharging => {
                self.battery_special = 0x1f;
            }
            ControllerBatteryState::Unknown => {}
        }
        if let Some(percentage) = percentage {
            self.battery_level = (u16::from(percentage) * 255 / 100) as u8;
            if self.battery_special & 0x10 != 0 && self.battery_special & 0x0f <= 0x0a {
                self.battery_special = (self.battery_special & !0x0f) | ((percentage + 5) / 10);
            }
        }
    }

    fn set_touch_contact(&mut self, pointer_index: u8, x: f32, y: f32) {
        let x = (x * DS4_TOUCH_RESOLUTION_X) as u16;
        let y = (y * DS4_TOUCH_RESOLUTION_Y) as u16;
        let packed = [
            x as u8,
            (((x >> 8) & 0x0f) | ((y & 0x0f) << 4)) as u8,
            (y >> 4) as u8,
        ];
        if pointer_index == 0 {
            self.current_touch.pointer_one_data = packed;
        } else {
            self.current_touch.pointer_two_data = packed;
        }
    }

    fn set_touch_down(&mut self, pointer_index: u8) {
        let pointer = if pointer_index == 0 {
            &mut self.current_touch.pointer_one
        } else {
            &mut self.current_touch.pointer_two
        };
        *pointer &= 0x7f;
        *pointer = pointer.wrapping_add(1);
    }

    fn set_touch_up(&mut self, pointer_index: u8) {
        if pointer_index == 0 {
            self.current_touch.pointer_one |= 0x80;
        } else {
            self.current_touch.pointer_two |= 0x80;
        }
    }

    fn set_touch_all_up(&mut self) {
        self.set_touch_up(0);
        self.set_touch_up(1);
    }
}

fn ds4_accel(value: f32, bias: i32, scale: f32) -> i16 {
    let bounded = value.clamp(-DS4_MAX_ACCEL_MPS2, DS4_MAX_ACCEL_MPS2);
    let sensor_value = (bounded / DS4_EARTH_G * 8192.0) as i32;
    (((sensor_value as f32 + bias as f32) / scale) as i32)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

fn ds4_gyro(value: f32, bias: i32, scale: f32) -> i16 {
    let bounded = value.clamp(-DS4_MAX_GYRO_DPS, DS4_MAX_GYRO_DPS);
    let sensor_value = (bounded * 16.0) as i32;
    (((sensor_value as f32 + bias as f32) / scale) as i32)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

const DS4_DPAD_NONE: u16 = 0x0008;
const DS4_TOUCH_RESOLUTION_X: f32 = 1920.0;
const DS4_TOUCH_RESOLUTION_Y: f32 = 943.0;
const DS4_MAX_ACCEL_MPS2: f32 = 200.0;
const DS4_MAX_GYRO_DPS: f32 = 4000.0;
const DS4_EARTH_G: f32 = 9.80665;
const DS4_MOTION_STALE_TIMEOUT: Duration = Duration::from_millis(750);
const DS4_KEEPALIVE_INTERVAL: Duration = Duration::from_millis(100);

const LI_CTYPE_XBOX: u8 = 0x01;
const LI_CTYPE_PS: u8 = 0x02;
const LI_CCAP_TOUCHPAD: u32 = 0x08;
const LI_CCAP_ACCEL: u32 = 0x10;
const LI_CCAP_GYRO: u32 = 0x20;

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
    target_ds4_update_ex: Option<VigemTargetDs4UpdateEx>,
}

struct VigemConnection {
    module: HMODULE,
    client: NonNull<c_void>,
    functions: VigemFunctions,
    allocated_targets: RefCell<HashSet<NonNull<c_void>>>,
    reconnect_required: Cell<bool>,
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
            reconnect_required: Cell::new(false),
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
            if is_connection_failure(status) {
                self.reconnect_required.set(true);
            }
            // SAFETY: target_add failed, so this target was not attached to the
            // driver and can be freed with the matching SDK allocator.
            unsafe { (self.functions.target_free)(target.as_ptr()) };
            return Err(vigem_error("target_add", status));
        }
        self.allocated_targets.borrow_mut().insert(target);
        Ok(target)
    }

    fn supports_ds4(&self) -> bool {
        self.functions.target_ds4_alloc.is_some()
            && (self.functions.target_ds4_update.is_some()
                || self.functions.target_ds4_update_ex.is_some())
    }

    fn supports_ds4_extended(&self) -> bool {
        self.functions.target_ds4_update_ex.is_some()
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
            if is_connection_failure(status) {
                self.reconnect_required.set(true);
            }
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
            if is_connection_failure(status) {
                self.reconnect_required.set(true);
            }
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
        let report = ds4_report(state, back_as_touchpad_click);
        let update = self.functions.target_ds4_update.ok_or_else(|| {
            InputError::new(
                "controller_unavailable",
                "ViGEmClient does not provide DS4 report support",
            )
        })?;
        // SAFETY: the client and target are live objects attached through this
        // connection, and Ds4Report matches the SDK's 10-byte DS4_REPORT ABI.
        let status = unsafe { update(self.client.as_ptr(), target.as_ptr(), report) };
        if !vigem_succeeded(status) {
            if is_connection_failure(status) {
                self.reconnect_required.set(true);
            }
            return Err(vigem_error("ds4_update", status));
        }
        Ok(())
    }

    fn update_ds4_extended(&self, target: NonNull<c_void>, report: Ds4ReportEx) -> InputResult<()> {
        let update = self.functions.target_ds4_update_ex.ok_or_else(|| {
            InputError::new(
                "controller_unavailable",
                "ViGEmClient does not provide extended DS4 report support",
            )
        })?;
        // SAFETY: the client and target are live objects attached through this
        // connection, and Ds4ReportEx matches the SDK's packed 63-byte report ABI.
        let status = unsafe { update(self.client.as_ptr(), target.as_ptr(), report) };
        if !vigem_succeeded(status) {
            if is_connection_failure(status) {
                self.reconnect_required.set(true);
            }
            return Err(vigem_error("ds4_update_ex", status));
        }
        Ok(())
    }

    fn remove_target(&self, target: NonNull<c_void>) -> InputResult<()> {
        // SAFETY: target is still allocated and belongs to this live connection.
        let status =
            unsafe { (self.functions.target_remove)(self.client.as_ptr(), target.as_ptr()) };
        if !vigem_succeeded(status) {
            if is_connection_failure(status) {
                self.reconnect_required.set(true);
            }
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
            target_ds4_update_ex: unsafe {
                load_optional_symbol(module, b"vigem_target_ds4_update_ex\0")
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
        // target whose normal remove failed is still registered here. The SDK
        // guarantees disconnect destroys all connected targets; their allocated
        // objects remain caller-owned and are freed after disconnect.
        unsafe {
            (self.functions.disconnect)(self.client.as_ptr());
            for target in self.allocated_targets.get_mut().drain() {
                (self.functions.target_free)(target.as_ptr());
            }
            (self.functions.free)(self.client.as_ptr());
            if !self.module.is_null() {
                FreeLibrary(self.module);
            }
        }
    }
}

struct VigemGamepad {
    connection: Option<Rc<VigemConnection>>,
    target: Option<NonNull<c_void>>,
    profile: GamepadProfile,
    ds4_back_as_touchpad_click: bool,
    replacement_pending: bool,
    controller_state: ControllerSnapshot,
    extended_report: Ds4ReportEx,
    pointer_id_map: HashMap<u32, u8>,
    available_pointers: u8,
    last_report_at: Instant,
    last_gyro_motion_at: Instant,
    report_dirty: bool,
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
        let now = Instant::now();
        Ok(Self {
            connection: Some(connection),
            target: Some(target),
            profile,
            ds4_back_as_touchpad_click,
            replacement_pending: false,
            controller_state: ControllerSnapshot::default(),
            extended_report: Ds4ReportEx::neutral(),
            pointer_id_map: HashMap::new(),
            available_pointers: 0x03,
            last_report_at: now,
            last_gyro_motion_at: now,
            report_dirty: false,
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

    fn connection(&self) -> InputResult<&VigemConnection> {
        self.connection.as_deref().ok_or_else(|| {
            InputError::new("controller_unavailable", "ViGEm client is reconnecting")
        })
    }

    fn invalidate_connection(&mut self) {
        // The old client's disconnect destroys every attached target. Its
        // owned target objects are freed by VigemConnection::drop, so these
        // pointers must never be used or freed from the next client generation.
        self.target = None;
        self.connection = None;
        self.replacement_pending = true;
    }

    fn bind_connection(&mut self, connection: Rc<VigemConnection>) {
        self.connection = Some(connection);
    }

    fn allocate_target(&self) -> InputResult<NonNull<c_void>> {
        match self.profile {
            GamepadProfile::Ds4 => self.connection()?.create_ds4_target(),
            GamepadProfile::Auto | GamepadProfile::X360 => self.connection()?.create_x360_target(),
            _ => Err(InputError::new(
                "controller_unavailable",
                "requested gamepad profile is not supported by the ViGEm backend",
            )),
        }
    }

    fn retry_replacement(&mut self) -> InputResult<()> {
        if let Some(target) = self.target {
            // A failed report may already have reached the bus. Do not send
            // another report to that target before it is detached.
            self.connection()?.remove_target(target)?;
            self.target = None;
        }
        self.target = Some(self.allocate_target()?);
        self.replacement_pending = false;
        Ok(())
    }

    fn ensure_target(&mut self) -> InputResult<()> {
        if self.replacement_pending || self.target.is_none() {
            self.retry_replacement()?;
        }
        Ok(())
    }

    fn supports_auxiliary_reports(&self) -> bool {
        self.profile == GamepadProfile::Ds4
            && self
                .connection
                .as_deref()
                .is_some_and(VigemConnection::supports_ds4_extended)
    }

    fn send_current_ds4_report(&mut self, now: Instant) -> InputResult<()> {
        let target = self.target()?;
        if self.connection()?.supports_ds4_extended() {
            let elapsed = now.saturating_duration_since(self.last_report_at);
            let timestamp = self
                .extended_report
                .timestamp()
                .wrapping_add((elapsed.as_nanos() / 5_333) as u16);
            self.extended_report.set_timestamp(timestamp);
            self.connection()?
                .update_ds4_extended(target, self.extended_report)?;
        } else {
            self.connection()?.update_ds4(
                target,
                self.controller_state,
                self.ds4_back_as_touchpad_click,
            )?;
        }
        self.last_report_at = now;
        self.report_dirty = false;
        Ok(())
    }

    fn write_auxiliary_report(&mut self, now: Instant) -> InputResult<()> {
        if !self.supports_auxiliary_reports() {
            return Ok(());
        }
        self.ensure_target()?;
        match self.send_current_ds4_report(now) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.report_dirty = true;
                self.recover_after_update_failure(error.clone())?;
                match self.send_current_ds4_report(now) {
                    Ok(()) => Ok(()),
                    Err(replay_error) => {
                        self.report_dirty = true;
                        self.recover_after_update_failure(replay_error.clone())?;
                        self.report_dirty = true;
                        Err(replay_error)
                    }
                }
            }
        }
    }

    fn touch(
        &mut self,
        _touchpad_index: u8,
        pointer_id: u32,
        phase: ControllerTouchPhase,
        x: f32,
        y: f32,
        _pressure: f32,
    ) -> InputResult<()> {
        if !self.supports_auxiliary_reports() {
            return Ok(());
        }
        let pointer_index = match phase {
            ControllerTouchPhase::Hover
            | ControllerTouchPhase::ButtonOnly
            | ControllerTouchPhase::HoverLeave => return Ok(()),
            ControllerTouchPhase::CancelAll => {
                self.pointer_id_map.clear();
                self.available_pointers = 0x03;
                self.extended_report.set_touch_all_up();
                None
            }
            ControllerTouchPhase::Down => {
                if let Some(pointer_index) = self.pointer_id_map.get(&pointer_id).copied() {
                    Some(pointer_index)
                } else {
                    let Some(pointer_index) =
                        (0..2).find(|index| self.available_pointers & (1 << index) != 0)
                    else {
                        return Ok(());
                    };
                    self.pointer_id_map.insert(pointer_id, pointer_index);
                    self.available_pointers &= !(1 << pointer_index);
                    self.extended_report.set_touch_down(pointer_index);
                    Some(pointer_index)
                }
            }
            ControllerTouchPhase::Move
            | ControllerTouchPhase::Up
            | ControllerTouchPhase::Cancel => {
                let Some(pointer_index) = self.pointer_id_map.get(&pointer_id).copied() else {
                    return Ok(());
                };
                if matches!(
                    phase,
                    ControllerTouchPhase::Up | ControllerTouchPhase::Cancel
                ) {
                    self.pointer_id_map.remove(&pointer_id);
                    self.available_pointers |= 1 << pointer_index;
                    self.extended_report.set_touch_up(pointer_index);
                }
                Some(pointer_index)
            }
        };
        if let Some(pointer_index) = pointer_index {
            self.extended_report.set_touch_contact(pointer_index, x, y);
        }
        self.extended_report.current_touch.packet_counter = self
            .extended_report
            .current_touch
            .packet_counter
            .wrapping_add(1);
        self.write_auxiliary_report(Instant::now())
    }

    fn motion(
        &mut self,
        motion_type: ControllerMotionType,
        x: f32,
        y: f32,
        z: f32,
    ) -> InputResult<()> {
        if !self.supports_auxiliary_reports() {
            return Ok(());
        }
        self.extended_report.set_motion(motion_type, x, y, z);
        if motion_type == ControllerMotionType::Gyro {
            self.last_gyro_motion_at = Instant::now();
        }
        self.write_auxiliary_report(Instant::now())
    }

    fn battery(
        &mut self,
        state: ControllerBatteryState,
        percentage: Option<u8>,
    ) -> InputResult<()> {
        if !self.supports_auxiliary_reports() {
            return Ok(());
        }
        self.extended_report.set_battery(state, percentage);
        self.write_auxiliary_report(Instant::now())
    }

    fn advance_auxiliary(&mut self, now: Instant) -> InputResult<()> {
        if !self.supports_auxiliary_reports() {
            return Ok(());
        }
        let gyro_stale =
            now.saturating_duration_since(self.last_gyro_motion_at) > DS4_MOTION_STALE_TIMEOUT;
        if gyro_stale
            && (self.extended_report.gyro_x != [0; 2]
                || self.extended_report.gyro_y != [0; 2]
                || self.extended_report.gyro_z != [0; 2])
        {
            self.extended_report
                .set_motion(ControllerMotionType::Gyro, 0.0, 0.0, 0.0);
            self.report_dirty = true;
        }
        if self.report_dirty
            || now.saturating_duration_since(self.last_report_at) >= DS4_KEEPALIVE_INTERVAL
        {
            self.write_auxiliary_report(now)?;
        }
        Ok(())
    }
}

impl GamepadDevice for VigemGamepad {
    fn update(&mut self, state: ControllerSnapshot) -> InputResult<()> {
        self.ensure_target()?;
        self.controller_state = state;
        match self.profile {
            GamepadProfile::Ds4 => {
                self.extended_report
                    .set_controller_state(state, self.ds4_back_as_touchpad_click);
                self.send_current_ds4_report(Instant::now())
            }
            GamepadProfile::Auto | GamepadProfile::X360 => {
                self.connection()?.update_x360(self.target()?, state)
            }
            _ => Err(InputError::new(
                "controller_unavailable",
                "requested gamepad profile is not supported by the ViGEm backend",
            )),
        }
    }

    fn set_ds4_back_as_touchpad_click(&mut self, enabled: bool) -> bool {
        let changed =
            self.profile == GamepadProfile::Ds4 && self.ds4_back_as_touchpad_click != enabled;
        self.ds4_back_as_touchpad_click = enabled;
        changed
    }

    fn recover_after_update_failure(&mut self, error: InputError) -> InputResult<()> {
        // A bus or client failure invalidates every target on this connection.
        // Let the owner disconnect the whole generation, including other slots,
        // instead of creating another target on the same broken client.
        if self.connection()?.reconnect_required.get() {
            return Err(error);
        }
        if self.replacement_pending {
            // The attempted detach already failed during this write. Leave
            // the uncertain target untouched until the next tick or stop.
            return Err(error);
        }
        // Removal is the boundary after which a possibly applied report can no
        // longer reach applications. Retain the old target if detach fails so
        // lease cleanup can retry it; never allocate a replacement first.
        self.replacement_pending = true;
        self.retry_replacement()
    }

    fn release(&mut self) -> InputResult<()> {
        if let Some(target) = self.target {
            // Detach even if a neutral report is rejected. Successful removal
            // is the stronger cleanup guarantee because the virtual device is
            // no longer connected to applications.
            if !self.replacement_pending {
                match self.profile {
                    GamepadProfile::Ds4 => {
                        self.controller_state = ControllerSnapshot::default();
                        self.extended_report = Ds4ReportEx::neutral();
                        self.pointer_id_map.clear();
                        self.available_pointers = 0x03;
                        let _ = self.send_current_ds4_report(Instant::now());
                    }
                    GamepadProfile::Auto | GamepadProfile::X360 => {
                        let _ = self
                            .connection()?
                            .update_x360(target, ControllerSnapshot::default());
                    }
                    _ => {}
                }
            }
            self.connection()?.remove_target(target)?;
            self.target = None;
        }
        self.replacement_pending = false;
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

struct SharedVigemGamepad(Rc<RefCell<VigemGamepad>>);

impl GamepadDevice for SharedVigemGamepad {
    fn update(&mut self, state: ControllerSnapshot) -> InputResult<()> {
        self.0.borrow_mut().update(state)
    }

    fn recover_after_update_failure(&mut self, error: InputError) -> InputResult<()> {
        self.0.borrow_mut().recover_after_update_failure(error)
    }

    fn set_ds4_back_as_touchpad_click(&mut self, enabled: bool) -> bool {
        self.0.borrow_mut().set_ds4_back_as_touchpad_click(enabled)
    }

    fn release(&mut self) -> InputResult<()> {
        self.0.borrow_mut().release()
    }
}

#[derive(Clone, Copy)]
struct ControllerArrivalMetadata {
    controller_type: u8,
    capabilities: u32,
}

pub(super) struct WindowsGamepads {
    slots: GamepadSlots<SharedVigemGamepad>,
    devices: [Option<Weak<RefCell<VigemGamepad>>>; 16],
    retired_devices: [Option<Weak<RefCell<VigemGamepad>>>; 16],
    controller_metadata: [Option<ControllerArrivalMetadata>; 16],
    active_mask: u16,
    connection: Option<Rc<VigemConnection>>,
    unavailable_reason: Option<String>,
    profile: GamepadProfile,
    target_profile: GamepadProfile,
    effective_profile: Option<&'static str>,
    back_button_timeout_ms: i32,
    ds4_back_as_touchpad_click: bool,
    motion_as_ds4: bool,
    touchpad_as_ds4: bool,
    reconnect_at: Option<Instant>,
}

impl WindowsGamepads {
    pub(super) fn new() -> Self {
        Self {
            slots: GamepadSlots::new(),
            devices: std::array::from_fn(|_| None),
            retired_devices: std::array::from_fn(|_| None),
            controller_metadata: [None; 16],
            active_mask: 0,
            connection: None,
            unavailable_reason: None,
            profile: GamepadProfile::Auto,
            target_profile: GamepadProfile::X360,
            effective_profile: None,
            back_button_timeout_ms: -1,
            ds4_back_as_touchpad_click: true,
            motion_as_ds4: true,
            touchpad_as_ds4: true,
            reconnect_at: None,
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
                self.reconnect_at = None;
                self.unavailable_reason = Some(format!(
                    "Windows gamepad profile '{}' is recognized but not implemented by this backend",
                    unsupported.as_str()
                ));
                Ok(())
            }
        }
    }

    #[cfg(test)]
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
        self.reconnect_at = None;
        self.target_profile = target_profile;
        self.clear_controller_tracking();
        match probe() {
            Ok(connection)
                if target_profile == GamepadProfile::Ds4 && !connection.supports_ds4() =>
            {
                self.unavailable_reason = Some(
                    "ViGEmClient is missing the DS4 target allocation or report export".into(),
                );
                self.reconnect_at = Some(Instant::now() + Duration::from_secs(1));
            }
            Ok(connection) => {
                self.connection = Some(Rc::new(connection));
                self.effective_profile = Some(target_profile.as_str());
            }
            Err(error) => {
                self.unavailable_reason = Some(error.message);
                self.reconnect_at = Some(Instant::now() + Duration::from_secs(1));
            }
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
        let result = self.slots.stop();
        self.invalidate_if_required(Instant::now());
        self.clear_controller_tracking();
        result
    }

    pub(super) fn validate(&self, event: &Event) -> InputResult<()> {
        event.validate()?;
        if is_controller_event(event) && !self.supported() {
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
        let result = match event {
            Event::Controller {
                number,
                active_mask,
                buttons,
                left_trigger,
                right_trigger,
                left_x,
                left_y,
                right_x,
                right_y,
            } => self.apply_controller_state(
                *number,
                *active_mask,
                ControllerSnapshot {
                    buttons: *buttons,
                    left_trigger: *left_trigger,
                    right_trigger: *right_trigger,
                    left_x: *left_x,
                    left_y: *left_y,
                    right_x: *right_x,
                    right_y: *right_y,
                },
            ),
            Event::ControllerArrival {
                number,
                controller_type,
                capabilities,
                ..
            } => self.controller_arrival(
                *number,
                ControllerArrivalMetadata {
                    controller_type: *controller_type,
                    capabilities: *capabilities,
                },
            ),
            Event::ControllerTouch {
                number,
                touchpad_index,
                pointer_id,
                phase,
                x,
                y,
                pressure,
            } => self.with_gamepad(*number, |gamepad| {
                gamepad.touch(*touchpad_index, *pointer_id, *phase, *x, *y, *pressure)
            }),
            Event::ControllerMotion {
                number,
                motion_type,
                x,
                y,
                z,
            } => self.with_gamepad(*number, |gamepad| gamepad.motion(*motion_type, *x, *y, *z)),
            Event::ControllerBattery {
                number,
                state,
                percentage,
            } => self.with_gamepad(*number, |gamepad| gamepad.battery(*state, *percentage)),
            _ => {
                return Err(InputError::new(
                    "unsupported_event",
                    "Windows gamepad backend accepts only controller events",
                ));
            }
        };
        self.invalidate_if_required(Instant::now());
        result.map_err(|error| InputError::new("controller_unavailable", error.message))
    }

    fn controller_arrival(
        &mut self,
        number: u16,
        metadata: ControllerArrivalMetadata,
    ) -> InputResult<()> {
        let slot = usize::from(number);
        if self.active_mask & (1 << slot) != 0
            && self.devices[slot]
                .as_ref()
                .and_then(Weak::upgrade)
                .is_some()
        {
            // Like the pinned runtime, duplicate arrivals never rebuild a live target.
            return Ok(());
        }
        self.controller_metadata[slot] = Some(metadata);
        self.apply_controller_state(
            number,
            self.active_mask | (1 << slot),
            ControllerSnapshot::default(),
        )
    }

    fn apply_controller_state(
        &mut self,
        number: u16,
        active_mask: u16,
        state: ControllerSnapshot,
    ) -> InputResult<()> {
        let removed_slots = self.active_mask & !active_mask;
        for slot in 0..self.devices.len() {
            if removed_slots & (1 << slot) != 0 {
                self.retired_devices[slot] = self.devices[slot].take();
                self.devices[slot] = None;
                self.controller_metadata[slot] = None;
            }
        }
        self.active_mask = active_mask;

        let connection = self
            .connection
            .as_ref()
            .expect("validated ViGEm connection")
            .clone();
        let metadata_by_slot = self.controller_metadata;
        let configured_profile = self.profile;
        let fallback_profile = self.target_profile;
        let back_as_touchpad_click = self.ds4_back_as_touchpad_click;
        let motion_as_ds4 = self.motion_as_ds4;
        let touchpad_as_ds4 = self.touchpad_as_ds4;
        let ds4_supported = connection.supports_ds4();
        let mut created_device = None;
        let result = self.slots.apply(
            number,
            active_mask,
            state,
            self.back_button_timeout_ms,
            Instant::now(),
            |slot| {
                let target_profile = select_gamepad_profile(
                    configured_profile,
                    fallback_profile,
                    metadata_by_slot[usize::from(slot)],
                    ds4_supported,
                    motion_as_ds4,
                    touchpad_as_ds4,
                )?;
                let device = Rc::new(RefCell::new(VigemGamepad::create(
                    connection.clone(),
                    target_profile,
                    back_as_touchpad_click,
                )?));
                created_device = Some((slot, Rc::downgrade(&device), target_profile));
                Ok(SharedVigemGamepad(device))
            },
        );
        drop(connection);
        if let Some((slot, device, target_profile)) = created_device {
            self.devices[usize::from(slot)] = Some(device);
            self.effective_profile = Some(target_profile.as_str());
        }
        self.reap_retired_devices();
        result
    }

    fn with_gamepad(
        &self,
        number: u16,
        apply: impl FnOnce(&mut VigemGamepad) -> InputResult<()>,
    ) -> InputResult<()> {
        if self.active_mask & (1 << number) == 0 {
            return Ok(());
        }
        let Some(device) = self.devices[usize::from(number)]
            .as_ref()
            .and_then(Weak::upgrade)
        else {
            // Auxiliary records for a slot that has not arrived (or has already
            // departed) are ignored, matching the pinned native input path.
            return Ok(());
        };
        apply(&mut device.borrow_mut())
    }

    fn clear_controller_tracking(&mut self) {
        self.active_mask = 0;
        self.controller_metadata = [None; 16];
        for slot in 0..self.devices.len() {
            if self.devices[slot].is_some() {
                self.retired_devices[slot] = self.devices[slot].take();
            }
        }
        self.devices = std::array::from_fn(|_| None);
        self.reap_retired_devices();
    }

    fn reap_retired_devices(&mut self) {
        for retired in &mut self.retired_devices {
            if retired
                .as_ref()
                .is_some_and(|device| device.upgrade().is_none())
            {
                *retired = None;
            }
        }
    }

    fn needs_ds4_support(&self) -> bool {
        self.profile == GamepadProfile::Ds4
            || self
                .devices
                .iter()
                .flatten()
                .filter_map(Weak::upgrade)
                .chain(
                    self.retired_devices
                        .iter()
                        .flatten()
                        .filter_map(Weak::upgrade),
                )
                .any(|device| device.borrow().profile == GamepadProfile::Ds4)
    }

    pub(super) fn advance(&mut self, now: Instant) {
        if self.connection.is_none() && self.reconnect_at.is_some_and(|due| now >= due) {
            self.reconnect(now);
        }
        // A timer write failure remains dirty in GamepadSlots and is retried
        // on later module ticks without tearing down keyboard/mouse injection.
        if self.connection.is_some() {
            let _ = self.slots.advance(now);
            self.reap_retired_devices();
            self.invalidate_if_required(now);
            if self.connection.is_some() {
                for device in self.devices.iter().flatten().filter_map(Weak::upgrade) {
                    let _ = device.borrow_mut().advance_auxiliary(now);
                }
                self.invalidate_if_required(now);
            }
        }
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

    pub(super) fn set_motion_as_ds4(&mut self, enabled: bool) {
        self.motion_as_ds4 = enabled;
    }

    pub(super) fn set_touchpad_as_ds4(&mut self, enabled: bool) {
        self.touchpad_as_ds4 = enabled;
    }

    pub(super) fn stop(&mut self) -> InputResult<()> {
        let result = self.slots.stop();
        self.invalidate_if_required(Instant::now());
        self.clear_controller_tracking();
        result?;
        self.connection = None;
        self.unavailable_reason = None;
        self.effective_profile = None;
        self.reconnect_at = None;
        Ok(())
    }

    fn invalidate_if_required(&mut self, now: Instant) {
        if self
            .connection
            .as_ref()
            .is_some_and(|connection| connection.reconnect_required.get())
        {
            self.slots.invalidate_devices(|device| {
                device.0.borrow_mut().invalidate_connection();
            });
            // This drops the last old-generation Rc. The SDK disconnects all
            // its targets, then our connection owner frees their objects.
            self.connection = None;
            self.effective_profile = None;
            self.unavailable_reason = Some("ViGEm client connection is reconnecting".into());
            self.reconnect_at = Some(now + Duration::from_secs(1));
        }
    }

    fn reconnect(&mut self, now: Instant) {
        self.reconnect_with(now, VigemConnection::connect_default);
    }

    fn reconnect_with(
        &mut self,
        now: Instant,
        probe: impl FnOnce() -> InputResult<VigemConnection>,
    ) {
        self.reconnect_at = Some(now + Duration::from_secs(1));
        match probe() {
            Ok(connection) if self.needs_ds4_support() && !connection.supports_ds4() => {
                self.unavailable_reason = Some(
                    "ViGEmClient is missing the DS4 target allocation or report export".into(),
                );
            }
            Ok(connection) => {
                let connection = Rc::new(connection);
                self.slots.invalidate_devices(|device| {
                    device.0.borrow_mut().bind_connection(connection.clone());
                });
                self.connection = Some(connection);
                self.effective_profile = self
                    .devices
                    .iter()
                    .flatten()
                    .filter_map(Weak::upgrade)
                    .next()
                    .map(|device| device.borrow().profile.as_str())
                    .or(Some(self.target_profile.as_str()));
                self.unavailable_reason = None;
                self.reconnect_at = None;
            }
            Err(error) => self.unavailable_reason = Some(error.message),
        }
    }
}

fn is_controller_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Controller { .. }
            | Event::ControllerArrival { .. }
            | Event::ControllerTouch { .. }
            | Event::ControllerMotion { .. }
            | Event::ControllerBattery { .. }
    )
}

fn select_gamepad_profile(
    configured: GamepadProfile,
    fallback: GamepadProfile,
    metadata: Option<ControllerArrivalMetadata>,
    ds4_supported: bool,
    motion_as_ds4: bool,
    touchpad_as_ds4: bool,
) -> InputResult<GamepadProfile> {
    let requested = match configured {
        GamepadProfile::Ds4 => GamepadProfile::Ds4,
        GamepadProfile::X360 => GamepadProfile::X360,
        GamepadProfile::Auto => {
            let Some(metadata) = metadata else {
                return Ok(fallback);
            };
            match metadata.controller_type {
                LI_CTYPE_PS => GamepadProfile::Ds4,
                LI_CTYPE_XBOX => GamepadProfile::X360,
                // The pinned ViGEm path has no Switch Pro target; Nintendo
                // controllers follow its capability-based branch here.
                _ if (motion_as_ds4
                    && metadata.capabilities & (LI_CCAP_ACCEL | LI_CCAP_GYRO) != 0)
                    || (touchpad_as_ds4 && metadata.capabilities & LI_CCAP_TOUCHPAD != 0) =>
                {
                    GamepadProfile::Ds4
                }
                _ => fallback,
            }
        }
        unsupported => {
            return Err(InputError::new(
                "controller_unavailable",
                format!(
                    "Windows gamepad profile '{}' is recognized but not implemented by this backend",
                    unsupported.as_str()
                ),
            ));
        }
    };
    if requested == GamepadProfile::Ds4 && !ds4_supported {
        let reason = if configured == GamepadProfile::Ds4 {
            "explicit DS4 profile requires ViGEmClient DS4 target allocation and report support"
        } else {
            "selected controller requires ViGEmClient DS4 target allocation and report support"
        };
        Err(InputError::new("controller_unavailable", reason))
    } else {
        Ok(requested)
    }
}

fn is_connection_failure(status: VigemError) -> bool {
    matches!(
        status,
        VIGEM_ERROR_BUS_NOT_FOUND | VIGEM_ERROR_BUS_INVALID_HANDLE | VIGEM_ERROR_WINAPI
    )
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

    #[derive(Default)]
    struct FakeBusTrace {
        attached: usize,
        freed: usize,
        disconnects: usize,
        removals: usize,
        target_profiles: Vec<GamepadProfile>,
        fail_next_update: Option<VigemError>,
        fail_updates: Vec<VigemError>,
        fail_remove: bool,
        fail_remove_on_attempt: Option<usize>,
        stale_reports: usize,
        updates: Vec<(u8, u16)>,
        ds4_special_updates: Vec<u8>,
        extended_reports: Vec<Ds4ReportEx>,
    }

    struct FakeClient {
        generation: u8,
        trace: Rc<RefCell<FakeBusTrace>>,
    }

    struct FakeTarget {
        generation: u8,
        trace: Option<Rc<RefCell<FakeBusTrace>>>,
        uncertain: bool,
        profile: GamepadProfile,
    }

    unsafe extern "C" fn fake_alloc() -> VigemClient {
        std::ptr::null_mut()
    }

    unsafe extern "C" fn fake_free(client: VigemClient) {
        // SAFETY: fake_connection allocates this exact FakeClient with Box.
        drop(unsafe { Box::from_raw(client.cast::<FakeClient>()) });
    }

    unsafe extern "C" fn fake_connect(_: VigemClient) -> VigemError {
        VIGEM_ERROR_NONE
    }

    unsafe extern "C" fn fake_disconnect(client: VigemClient) {
        // SAFETY: the test connection owns this live FakeClient until fake_free.
        let client = unsafe { &*client.cast::<FakeClient>() };
        let mut trace = client.trace.borrow_mut();
        trace.attached = 0;
        trace.disconnects += 1;
    }

    unsafe extern "C" fn fake_target_alloc() -> VigemTarget {
        Box::into_raw(Box::new(FakeTarget {
            generation: 0,
            trace: None,
            uncertain: false,
            profile: GamepadProfile::X360,
        }))
        .cast()
    }

    unsafe extern "C" fn fake_ds4_target_alloc() -> VigemTarget {
        Box::into_raw(Box::new(FakeTarget {
            generation: 0,
            trace: None,
            uncertain: false,
            profile: GamepadProfile::Ds4,
        }))
        .cast()
    }

    unsafe extern "C" fn fake_target_free(target: VigemTarget) {
        // SAFETY: fake_target_alloc creates this target, freed exactly once by
        // VigemConnection after removal or disconnect.
        let target = unsafe { Box::from_raw(target.cast::<FakeTarget>()) };
        if let Some(trace) = &target.trace {
            trace.borrow_mut().freed += 1;
        }
    }

    unsafe extern "C" fn fake_target_add(client: VigemClient, target: VigemTarget) -> VigemError {
        // SAFETY: both pointers are live Box allocations for this test client.
        let client = unsafe { &*client.cast::<FakeClient>() };
        let target = unsafe { &mut *target.cast::<FakeTarget>() };
        target.generation = client.generation;
        target.trace = Some(client.trace.clone());
        let mut trace = client.trace.borrow_mut();
        trace.attached += 1;
        trace.target_profiles.push(target.profile);
        VIGEM_ERROR_NONE
    }

    unsafe extern "C" fn fake_target_remove(client: VigemClient, _: VigemTarget) -> VigemError {
        // SAFETY: the client remains owned by its VigemConnection.
        let client = unsafe { &*client.cast::<FakeClient>() };
        let mut trace = client.trace.borrow_mut();
        trace.removals += 1;
        if trace.fail_remove || trace.fail_remove_on_attempt == Some(trace.removals) {
            return 0xE000_0004;
        }
        trace.attached -= 1;
        VIGEM_ERROR_NONE
    }

    unsafe extern "C" fn fake_x360_update(
        client: VigemClient,
        target: VigemTarget,
        report: XusbReport,
    ) -> VigemError {
        // SAFETY: the client remains owned by its VigemConnection.
        let client = unsafe { &*client.cast::<FakeClient>() };
        // SAFETY: the target remains allocated until a successful removal.
        let target = unsafe { &mut *target.cast::<FakeTarget>() };
        let mut trace = client.trace.borrow_mut();
        if target.uncertain {
            trace.stale_reports += 1;
            return 0xE000_0004;
        }
        if let Some(status) = trace
            .fail_next_update
            .take()
            .or_else(|| (!trace.fail_updates.is_empty()).then(|| trace.fail_updates.remove(0)))
        {
            target.uncertain = true;
            return status;
        }
        trace.updates.push((client.generation, report.buttons));
        VIGEM_ERROR_NONE
    }

    unsafe extern "C" fn fake_ds4_update(
        client: VigemClient,
        target: VigemTarget,
        report: Ds4Report,
    ) -> VigemError {
        // SAFETY: the client remains owned by its VigemConnection.
        let client = unsafe { &*client.cast::<FakeClient>() };
        // SAFETY: the target remains allocated until a successful removal.
        let target = unsafe { &mut *target.cast::<FakeTarget>() };
        let mut trace = client.trace.borrow_mut();
        if target.uncertain {
            trace.stale_reports += 1;
            return 0xE000_0004;
        }
        if let Some(status) = trace
            .fail_next_update
            .take()
            .or_else(|| (!trace.fail_updates.is_empty()).then(|| trace.fail_updates.remove(0)))
        {
            target.uncertain = true;
            return status;
        }
        trace.updates.push((client.generation, report.buttons));
        trace.ds4_special_updates.push(report.special_buttons);
        VIGEM_ERROR_NONE
    }

    unsafe extern "C" fn fake_ds4_update_ex(
        client: VigemClient,
        target: VigemTarget,
        report: Ds4ReportEx,
    ) -> VigemError {
        // SAFETY: the client remains owned by its VigemConnection.
        let client = unsafe { &*client.cast::<FakeClient>() };
        // SAFETY: the target remains allocated until a successful removal.
        let target = unsafe { &mut *target.cast::<FakeTarget>() };
        let mut trace = client.trace.borrow_mut();
        if target.uncertain {
            trace.stale_reports += 1;
            return 0xE000_0004;
        }
        if let Some(status) = trace
            .fail_next_update
            .take()
            .or_else(|| (!trace.fail_updates.is_empty()).then(|| trace.fail_updates.remove(0)))
        {
            target.uncertain = true;
            return status;
        }
        trace
            .updates
            .push((client.generation, u16::from_le_bytes(report.buttons)));
        trace.ds4_special_updates.push(report.special_buttons);
        trace.extended_reports.push(report);
        VIGEM_ERROR_NONE
    }

    fn fake_connection(generation: u8, trace: Rc<RefCell<FakeBusTrace>>) -> VigemConnection {
        fake_connection_with_ds4(generation, trace, false)
    }

    fn fake_connection_with_ds4(
        generation: u8,
        trace: Rc<RefCell<FakeBusTrace>>,
        ds4: bool,
    ) -> VigemConnection {
        let client = Box::into_raw(Box::new(FakeClient { generation, trace })).cast();
        VigemConnection {
            // A null library handle is test-only; Drop skips FreeLibrary for it.
            module: std::ptr::null_mut(),
            client: NonNull::new(client).expect("boxed fake client"),
            functions: VigemFunctions {
                alloc: fake_alloc,
                free: fake_free,
                connect: fake_connect,
                disconnect: fake_disconnect,
                target_x360_alloc: fake_target_alloc,
                target_free: fake_target_free,
                target_add: fake_target_add,
                target_remove: fake_target_remove,
                target_x360_update: fake_x360_update,
                target_ds4_alloc: ds4.then_some(fake_ds4_target_alloc),
                target_ds4_update: ds4.then_some(fake_ds4_update),
                target_ds4_update_ex: None,
            },
            allocated_targets: RefCell::new(HashSet::new()),
            reconnect_required: Cell::new(false),
        }
    }

    fn fake_connection_with_ds4_extended(
        generation: u8,
        trace: Rc<RefCell<FakeBusTrace>>,
    ) -> VigemConnection {
        let mut connection = fake_connection_with_ds4(generation, trace, true);
        connection.functions.target_ds4_update_ex = Some(fake_ds4_update_ex);
        connection
    }

    fn controller_event(number: u16, active_mask: u16, buttons: u32) -> Event {
        Event::Controller {
            number,
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

    fn arrival_event(number: u16, controller_type: u8, capabilities: u32) -> Event {
        Event::ControllerArrival {
            number,
            controller_type,
            capabilities,
            supported_buttons: u32::MAX,
        }
    }

    fn touch_event(
        number: u16,
        pointer_id: u32,
        phase: ControllerTouchPhase,
        x: f32,
        y: f32,
        pressure: f32,
    ) -> Event {
        Event::ControllerTouch {
            number,
            touchpad_index: 0,
            pointer_id,
            phase,
            x,
            y,
            pressure,
        }
    }

    fn motion_event(
        number: u16,
        motion_type: ControllerMotionType,
        x: f32,
        y: f32,
        z: f32,
    ) -> Event {
        Event::ControllerMotion {
            number,
            motion_type,
            x,
            y,
            z,
        }
    }

    fn battery_event(number: u16, state: ControllerBatteryState, percentage: Option<u8>) -> Event {
        Event::ControllerBattery {
            number,
            state,
            percentage,
        }
    }

    #[test]
    fn auto_profile_uses_metadata_then_enabled_auxiliary_capabilities() {
        let fallback = GamepadProfile::X360;
        let select = |controller_type, capabilities, motion, touchpad| {
            select_gamepad_profile(
                GamepadProfile::Auto,
                fallback,
                Some(ControllerArrivalMetadata {
                    controller_type,
                    capabilities,
                }),
                true,
                motion,
                touchpad,
            )
            .unwrap()
        };

        assert_eq!(select(LI_CTYPE_PS, 0, false, false), GamepadProfile::Ds4);
        assert_eq!(
            select(LI_CTYPE_XBOX, LI_CCAP_GYRO | LI_CCAP_TOUCHPAD, true, true),
            GamepadProfile::X360
        );
        assert_eq!(
            select(0x7f, LI_CCAP_ACCEL, true, false),
            GamepadProfile::Ds4
        );
        assert_eq!(
            select(0x7f, LI_CCAP_ACCEL, false, true),
            GamepadProfile::X360
        );
        assert_eq!(
            select(0x7f, LI_CCAP_TOUCHPAD, false, true),
            GamepadProfile::Ds4
        );
        assert_eq!(
            select(0x7f, LI_CCAP_TOUCHPAD, true, false),
            GamepadProfile::X360
        );
        assert_eq!(
            select(0x7f, LI_CCAP_GYRO | LI_CCAP_TOUCHPAD, false, false),
            GamepadProfile::X360
        );
        assert_eq!(select(0x03, 0, true, true), GamepadProfile::X360);

        assert_eq!(
            select_gamepad_profile(
                GamepadProfile::X360,
                fallback,
                Some(ControllerArrivalMetadata {
                    controller_type: LI_CTYPE_PS,
                    capabilities: LI_CCAP_TOUCHPAD,
                }),
                true,
                true,
                true,
            )
            .unwrap(),
            GamepadProfile::X360,
            "an explicit profile wins over client metadata"
        );
        assert!(
            select_gamepad_profile(GamepadProfile::Ds4, fallback, None, false, true, true,)
                .is_err()
        );
        assert!(
            select_gamepad_profile(
                GamepadProfile::Auto,
                fallback,
                Some(ControllerArrivalMetadata {
                    controller_type: LI_CTYPE_PS,
                    capabilities: 0,
                }),
                false,
                true,
                true,
            )
            .is_err()
        );
        assert!(
            select_gamepad_profile(GamepadProfile::VhfSwitch, fallback, None, true, true, true,)
                .is_err()
        );
    }

    #[test]
    fn arrival_selects_per_slot_and_auxiliary_reports_do_not_rebuild_targets() {
        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::X360, || {
                Ok(fake_connection_with_ds4_extended(1, trace.clone()))
            })
            .unwrap();
        gamepads.set_profile(GamepadProfile::Auto);
        gamepads
            .inject(&motion_event(0, ControllerMotionType::Gyro, 2.0, 3.0, 4.0))
            .unwrap();
        assert!(trace.borrow().target_profiles.is_empty());

        gamepads.inject(&arrival_event(0, LI_CTYPE_PS, 0)).unwrap();
        gamepads
            .inject(&arrival_event(
                1,
                LI_CTYPE_XBOX,
                LI_CCAP_GYRO | LI_CCAP_TOUCHPAD,
            ))
            .unwrap();
        assert_eq!(
            trace.borrow().target_profiles,
            [GamepadProfile::Ds4, GamepadProfile::X360]
        );

        gamepads
            .inject(&controller_event(0, 0b11, 1 << 12))
            .unwrap();
        let allocated_profiles = trace.borrow().target_profiles.clone();
        gamepads
            .inject(&arrival_event(0, LI_CTYPE_XBOX, LI_CCAP_TOUCHPAD))
            .unwrap();
        gamepads
            .inject(&touch_event(
                0,
                42,
                ControllerTouchPhase::Down,
                0.5,
                0.5,
                0.7,
            ))
            .unwrap();
        gamepads
            .inject(&motion_event(
                0,
                ControllerMotionType::Gyro,
                12.0,
                -4.0,
                1.5,
            ))
            .unwrap();
        gamepads
            .inject(&battery_event(
                0,
                ControllerBatteryState::Charging,
                Some(70),
            ))
            .unwrap();
        assert_eq!(trace.borrow().target_profiles, allocated_profiles);
        assert_eq!(trace.borrow().attached, 2);
        assert_eq!(trace.borrow().extended_reports.len(), 5);
        assert_eq!(trace.borrow().updates.len(), 6);

        gamepads.stop().unwrap();
        assert_eq!(trace.borrow().attached, 0);
        assert_eq!(trace.borrow().disconnects, 1);
    }

    #[test]
    fn ds4_extended_touch_tracks_two_contacts_ignores_hover_and_cancels_all() {
        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::Ds4, || {
                Ok(fake_connection_with_ds4_extended(1, trace.clone()))
            })
            .unwrap();
        gamepads
            .inject(&arrival_event(0, LI_CTYPE_PS, LI_CCAP_TOUCHPAD))
            .unwrap();

        for phase in [
            ControllerTouchPhase::Hover,
            ControllerTouchPhase::ButtonOnly,
            ControllerTouchPhase::HoverLeave,
        ] {
            gamepads
                .inject(&touch_event(0, 99, phase, 0.5, 0.5, 0.2))
                .unwrap();
        }
        assert_eq!(trace.borrow().extended_reports.len(), 1);

        gamepads
            .inject(&touch_event(
                0,
                10,
                ControllerTouchPhase::Down,
                0.5,
                0.5,
                0.3,
            ))
            .unwrap();
        let down = *trace.borrow().extended_reports.last().unwrap();
        assert_eq!(down.current_touch.pointer_one & 0x80, 0);
        assert_eq!(down.current_touch.pointer_two & 0x80, 0x80);
        assert_eq!(down.current_touch.pointer_one_data, [0xc0, 0x73, 0x1d]);

        gamepads
            .inject(&touch_event(
                0,
                11,
                ControllerTouchPhase::Down,
                0.25,
                0.75,
                0.8,
            ))
            .unwrap();
        gamepads
            .inject(&touch_event(0, 10, ControllerTouchPhase::Up, 0.6, 0.4, 0.0))
            .unwrap();
        let up = *trace.borrow().extended_reports.last().unwrap();
        assert_ne!(up.current_touch.pointer_one & 0x80, 0);
        assert_eq!(up.current_touch.pointer_two & 0x80, 0);

        gamepads
            .inject(&touch_event(
                0,
                11,
                ControllerTouchPhase::CancelAll,
                0.0,
                0.0,
                0.0,
            ))
            .unwrap();
        let cancelled = *trace.borrow().extended_reports.last().unwrap();
        assert_ne!(cancelled.current_touch.pointer_one & 0x80, 0);
        assert_ne!(cancelled.current_touch.pointer_two & 0x80, 0);
        let report_count = trace.borrow().extended_reports.len();
        gamepads
            .inject(&touch_event(
                0,
                11,
                ControllerTouchPhase::Move,
                0.1,
                0.1,
                1.0,
            ))
            .unwrap();
        assert_eq!(trace.borrow().extended_reports.len(), report_count);

        gamepads.inject(&controller_event(0, 0, 0)).unwrap();
        assert_eq!(trace.borrow().attached, 0);
        let reports_after_removal = trace.borrow().extended_reports.len();
        gamepads
            .inject(&motion_event(0, ControllerMotionType::Accel, 1.0, 2.0, 3.0))
            .unwrap();
        assert_eq!(trace.borrow().extended_reports.len(), reports_after_removal);
        gamepads.stop().unwrap();
    }

    #[test]
    fn ds4_extended_motion_and_battery_follow_report_scales() {
        let mut report = Ds4ReportEx::neutral();
        assert_eq!(size_of::<Ds4ReportEx>(), 63);
        assert_eq!(
            i16::from_le_bytes(report.accel_y),
            ds4_accel(DS4_EARTH_G, -42, 1.014614)
        );

        report.set_motion(ControllerMotionType::Gyro, 5000.0, -5000.0, 125.0);
        assert_eq!(
            i16::from_le_bytes(report.gyro_x),
            ds4_gyro(4000.0, 1, 0.977596)
        );
        assert_eq!(
            i16::from_le_bytes(report.gyro_y),
            ds4_gyro(-4000.0, 0, 0.972370)
        );
        assert_eq!(
            i16::from_le_bytes(report.gyro_z),
            ds4_gyro(125.0, 0, 0.971550)
        );
        report.set_motion(ControllerMotionType::Accel, 0.0, DS4_EARTH_G, 0.0);
        assert_eq!(
            i16::from_le_bytes(report.accel_y),
            ds4_accel(DS4_EARTH_G, -42, 1.014614)
        );

        report.set_battery(ControllerBatteryState::Charging, Some(70));
        assert_eq!(report.battery_level, 178);
        assert_eq!(report.battery_special, 0x17);
        report.set_battery(ControllerBatteryState::Discharging, Some(50));
        assert_eq!(report.battery_level, 127);
        assert_eq!(report.battery_special, 0x07);
        report.set_battery(ControllerBatteryState::Full, None);
        assert_eq!(report.battery_special, 0x1b);
        assert_eq!(report.battery_level, 0xff);
        report.set_battery(ControllerBatteryState::NotPresent, None);
        assert_eq!(report.battery_special, 0x1f);
    }

    #[test]
    fn x360_and_basic_ds4_safely_ignore_unsupported_auxiliary_reports() {
        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::X360, || {
                Ok(fake_connection_with_ds4(1, trace.clone(), true))
            })
            .unwrap();
        gamepads
            .inject(&arrival_event(0, LI_CTYPE_XBOX, 0))
            .unwrap();
        gamepads.inject(&controller_event(0, 1, 1 << 12)).unwrap();
        let accepted_reports = trace.borrow().updates.len();
        gamepads
            .inject(&touch_event(
                0,
                1,
                ControllerTouchPhase::Down,
                0.2,
                0.3,
                0.4,
            ))
            .unwrap();
        gamepads
            .inject(&motion_event(0, ControllerMotionType::Gyro, 1.0, 2.0, 3.0))
            .unwrap();
        gamepads
            .inject(&battery_event(
                0,
                ControllerBatteryState::Charging,
                Some(20),
            ))
            .unwrap();
        assert_eq!(trace.borrow().updates.len(), accepted_reports);
        assert!(trace.borrow().extended_reports.is_empty());
        gamepads.stop().unwrap();

        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::Ds4, || {
                Ok(fake_connection_with_ds4(1, trace.clone(), true))
            })
            .unwrap();
        gamepads.inject(&arrival_event(0, LI_CTYPE_PS, 0)).unwrap();
        let reports_before = trace.borrow().updates.len();
        gamepads
            .inject(&touch_event(
                0,
                1,
                ControllerTouchPhase::Down,
                0.2,
                0.3,
                0.4,
            ))
            .unwrap();
        gamepads
            .inject(&motion_event(0, ControllerMotionType::Accel, 1.0, 2.0, 3.0))
            .unwrap();
        gamepads
            .inject(&battery_event(0, ControllerBatteryState::Full, None))
            .unwrap();
        assert_eq!(trace.borrow().updates.len(), reports_before);
        assert!(trace.borrow().extended_reports.is_empty());
        gamepads.stop().unwrap();
    }

    #[test]
    fn failed_extended_auxiliary_write_recreates_target_and_preserves_state() {
        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::Ds4, || {
                Ok(fake_connection_with_ds4_extended(1, trace.clone()))
            })
            .unwrap();
        gamepads
            .inject(&arrival_event(0, LI_CTYPE_PS, LI_CCAP_TOUCHPAD))
            .unwrap();
        gamepads
            .inject(&touch_event(
                0,
                7,
                ControllerTouchPhase::Down,
                0.4,
                0.6,
                0.5,
            ))
            .unwrap();
        trace.borrow_mut().fail_next_update = Some(0xE000_0004);
        gamepads
            .inject(&motion_event(0, ControllerMotionType::Gyro, 4.0, 5.0, 6.0))
            .unwrap();

        let trace_snapshot = trace.borrow();
        assert_eq!(trace_snapshot.removals, 1);
        assert_eq!(trace_snapshot.attached, 1);
        assert_eq!(trace_snapshot.disconnects, 0);
        assert_eq!(trace_snapshot.target_profiles, [GamepadProfile::Ds4; 2]);
        assert_eq!(trace_snapshot.stale_reports, 0);
        let recovered = trace_snapshot.extended_reports.last().unwrap();
        assert_eq!(recovered.current_touch.pointer_one & 0x80, 0);
        assert_ne!(i16::from_le_bytes(recovered.gyro_z), 0);
        drop(trace_snapshot);

        gamepads.stop().unwrap();
        assert_eq!(trace.borrow().attached, 0);
        assert_eq!(trace.borrow().disconnects, 1);
    }

    #[test]
    fn malformed_auxiliary_input_is_rejected_and_unknown_type_falls_back_safely() {
        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::X360, || {
                Ok(fake_connection(1, trace.clone()))
            })
            .unwrap();
        let malformed = touch_event(16, 1, ControllerTouchPhase::Down, f32::NAN, 0.5, 0.5);
        assert_eq!(
            gamepads.inject(&malformed).unwrap_err().code,
            "invalid_event"
        );

        gamepads.set_profile(GamepadProfile::Auto);
        gamepads.inject(&arrival_event(0, 0x7f, 0)).unwrap();
        assert_eq!(trace.borrow().target_profiles, [GamepadProfile::X360]);
        gamepads.stop().unwrap();
    }

    #[test]
    fn auto_selected_ds4_without_vigem_support_fails_without_x360_substitution() {
        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::X360, || {
                Ok(fake_connection(1, trace.clone()))
            })
            .unwrap();
        gamepads.set_profile(GamepadProfile::Auto);

        let error = gamepads
            .inject(&arrival_event(0, LI_CTYPE_PS, 0))
            .unwrap_err();
        assert_eq!(error.code, "controller_unavailable");
        assert!(error.message.contains("selected controller requires"));
        assert!(trace.borrow().target_profiles.is_empty());
        gamepads.stop().unwrap();
    }

    #[test]
    fn explicit_vhf_switch_profile_remains_unavailable() {
        let mut gamepads = WindowsGamepads::new();
        gamepads.set_profile(GamepadProfile::VhfSwitch);
        gamepads.start().unwrap();

        assert!(!gamepads.supported());
        assert_eq!(gamepads.effective_profile(), None);
        assert!(
            gamepads
                .unavailable_reason()
                .is_some_and(|reason| reason.contains("profile 'vhf_switch'"))
        );
        assert_eq!(
            gamepads
                .validate(&arrival_event(0, 0x03, LI_CCAP_TOUCHPAD))
                .unwrap_err()
                .code,
            "controller_unavailable"
        );
    }

    #[test]
    fn failed_x360_update_reconnects_both_slots_without_old_target_removal() {
        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::X360, || {
                Ok(fake_connection(1, trace.clone()))
            })
            .unwrap();
        gamepads.set_back_button_timeout(200);
        gamepads
            .inject(&controller_event(0, 1, BACK_BUTTON_MASK))
            .unwrap();
        gamepads.inject(&controller_event(1, 3, 1 << 13)).unwrap();
        assert_eq!(trace.borrow().attached, 2);

        trace.borrow_mut().fail_next_update = Some(VIGEM_ERROR_WINAPI);
        trace.borrow_mut().fail_remove = true;
        assert!(gamepads.inject(&controller_event(1, 3, 1 << 14)).is_err());
        assert!(!gamepads.supported());
        assert_eq!(trace.borrow().removals, 0);
        assert_eq!(trace.borrow().attached, 0);
        assert_eq!(trace.borrow().disconnects, 1);
        assert_eq!(trace.borrow().freed, 2);

        trace.borrow_mut().fail_remove = false;
        let now = Instant::now();
        gamepads.reconnect_with(now, || Ok(fake_connection(2, trace.clone())));
        gamepads.advance(now);
        assert_eq!(trace.borrow().attached, 2);
        assert!(trace.borrow().updates.contains(&(2, 0x0020)));
        assert!(trace.borrow().updates.contains(&(2, 0x4000)));
        gamepads.advance(now + Duration::from_millis(200));
        assert!(
            trace.borrow().updates.contains(&(2, 0x0400)),
            "Back timer and Guide pulse must survive a connection change"
        );
        gamepads.stop().unwrap();
        assert_eq!(trace.borrow().attached, 0);
        assert_eq!(trace.borrow().freed, 4);
        assert_eq!(trace.borrow().disconnects, 2);
    }

    #[test]
    fn stop_during_pending_reconnect_releases_all_old_targets_without_probe() {
        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::X360, || {
                Ok(fake_connection(1, trace.clone()))
            })
            .unwrap();
        gamepads.inject(&controller_event(0, 1, 1 << 12)).unwrap();
        trace.borrow_mut().fail_next_update = Some(VIGEM_ERROR_WINAPI);
        trace.borrow_mut().fail_remove = true;
        assert!(gamepads.inject(&controller_event(0, 1, 1 << 13)).is_err());
        assert!(gamepads.reconnect_at.is_some());
        gamepads.reconnect_with(Instant::now(), || {
            Err(InputError::new(
                "controller_unavailable",
                "bus still absent",
            ))
        });
        assert!(gamepads.reconnect_at.is_some());
        gamepads.stop().unwrap();
        assert!(gamepads.reconnect_at.is_none());
        assert_eq!(trace.borrow().attached, 0);
        assert_eq!(trace.borrow().freed, 1);
        gamepads.advance(Instant::now() + Duration::from_secs(2));
        assert!(!gamepads.supported());
        assert_eq!(trace.borrow().disconnects, 1);
    }

    #[test]
    fn ds4_bus_failure_retires_all_targets_and_replays_state_on_new_client() {
        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::Ds4, || {
                Ok(fake_connection_with_ds4(1, trace.clone(), true))
            })
            .unwrap();
        gamepads.inject(&controller_event(0, 1, 1 << 12)).unwrap();
        gamepads.inject(&controller_event(1, 3, 1 << 13)).unwrap();

        trace.borrow_mut().fail_next_update = Some(VIGEM_ERROR_BUS_INVALID_HANDLE);
        assert!(gamepads.inject(&controller_event(1, 3, 1 << 14)).is_err());
        assert!(!gamepads.supported());
        assert_eq!(trace.borrow().removals, 0);
        assert_eq!(trace.borrow().disconnects, 1);
        assert_eq!(trace.borrow().freed, 2);

        let now = Instant::now();
        gamepads.reconnect_with(now, || Ok(fake_connection_with_ds4(2, trace.clone(), true)));
        gamepads.advance(now);
        assert_eq!(trace.borrow().attached, 2);
        assert_eq!(gamepads.effective_profile(), Some("ds4"));
        assert!(trace.borrow().updates.contains(&(2, 0x0028)));
        assert!(trace.borrow().updates.contains(&(2, 0x0018)));
        gamepads.stop().unwrap();
        assert_eq!(trace.borrow().attached, 0);
    }

    #[test]
    fn held_ds4_back_mapping_replays_on_tick_and_recovers_a_failed_report() {
        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::Ds4, || {
                Ok(fake_connection_with_ds4(1, trace.clone(), true))
            })
            .unwrap();
        gamepads
            .inject(&controller_event(0, 1, BACK_BUTTON_MASK))
            .unwrap();
        assert_eq!(trace.borrow().ds4_special_updates, [DS4_SPECIAL_TOUCHPAD]);

        gamepads.set_ds4_back_as_touchpad_click(false);
        gamepads.advance(Instant::now());
        assert_eq!(
            trace.borrow().ds4_special_updates,
            [DS4_SPECIAL_TOUCHPAD, 0]
        );

        trace.borrow_mut().fail_next_update = Some(0xE000_0004);
        gamepads.set_ds4_back_as_touchpad_click(true);
        gamepads.advance(Instant::now());
        assert_eq!(trace.borrow().removals, 1);
        assert_eq!(trace.borrow().attached, 1);
        assert_eq!(trace.borrow().disconnects, 0);
        assert_eq!(
            trace.borrow().ds4_special_updates,
            [DS4_SPECIAL_TOUCHPAD, 0, DS4_SPECIAL_TOUCHPAD]
        );
        assert_eq!(trace.borrow().stale_reports, 0);

        gamepads.stop().unwrap();
        assert_eq!(trace.borrow().attached, 0);
        assert_eq!(trace.borrow().freed, 2);
        assert_eq!(trace.borrow().disconnects, 1);
    }

    #[test]
    fn ds4_mapping_change_does_not_replay_an_x360_report() {
        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::X360, || {
                Ok(fake_connection(1, trace.clone()))
            })
            .unwrap();
        gamepads
            .inject(&controller_event(0, 1, BACK_BUTTON_MASK))
            .unwrap();
        assert_eq!(trace.borrow().updates, [(1, 0x0020)]);

        gamepads.set_ds4_back_as_touchpad_click(false);
        gamepads.advance(Instant::now());
        assert_eq!(trace.borrow().updates, [(1, 0x0020)]);
        assert_eq!(trace.borrow().removals, 0);

        gamepads.stop().unwrap();
        assert_eq!(trace.borrow().attached, 0);
        assert_eq!(trace.borrow().freed, 1);
    }

    #[test]
    fn non_bus_update_failure_recreates_only_its_target() {
        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::X360, || {
                Ok(fake_connection(1, trace.clone()))
            })
            .unwrap();
        gamepads.inject(&controller_event(0, 1, 1 << 12)).unwrap();
        trace.borrow_mut().fail_next_update = Some(0xE000_0004);
        gamepads.inject(&controller_event(0, 1, 1 << 13)).unwrap();
        assert!(gamepads.supported());
        assert_eq!(trace.borrow().removals, 1);
        assert_eq!(trace.borrow().attached, 1);
        assert_eq!(trace.borrow().disconnects, 0);
        gamepads.stop().unwrap();
    }

    #[test]
    fn failed_target_detach_retries_before_replay_without_disturbing_other_slot() {
        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::X360, || {
                Ok(fake_connection(1, trace.clone()))
            })
            .unwrap();
        gamepads.inject(&controller_event(0, 1, 1 << 12)).unwrap();
        gamepads.inject(&controller_event(1, 3, 1 << 13)).unwrap();

        trace.borrow_mut().fail_next_update = Some(0xE000_0004);
        trace.borrow_mut().fail_remove = true;
        assert!(gamepads.inject(&controller_event(0, 3, 1 << 14)).is_err());
        assert_eq!(trace.borrow().attached, 2);
        assert_eq!(trace.borrow().disconnects, 0);
        assert_eq!(trace.borrow().updates.len(), 2);

        // A timer retry must not send the uncertain state to the old target.
        gamepads.advance(Instant::now());
        assert_eq!(trace.borrow().updates.len(), 2);
        assert_eq!(trace.borrow().attached, 2);
        gamepads.inject(&controller_event(1, 3, 1 << 15)).unwrap();
        assert!(trace.borrow().updates.contains(&(1, 0x8000)));
        assert_eq!(trace.borrow().disconnects, 0);

        trace.borrow_mut().fail_remove = false;
        gamepads.advance(Instant::now());
        assert!(trace.borrow().updates.contains(&(1, 0x4000)));
        assert_eq!(trace.borrow().attached, 2);
        assert_eq!(trace.borrow().removals, 3);

        trace.borrow_mut().fail_remove = true;
        assert!(gamepads.stop().is_err());
        assert_eq!(trace.borrow().attached, 2);
        assert_eq!(trace.borrow().disconnects, 0);
        trace.borrow_mut().fail_remove = false;
        gamepads.stop().unwrap();
        assert_eq!(trace.borrow().attached, 0);
        assert_eq!(trace.borrow().disconnects, 1);
        assert_eq!(trace.borrow().freed, 3);
    }

    #[test]
    fn stop_retries_uncertain_target_detach_without_sending_a_neutral_report() {
        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::X360, || {
                Ok(fake_connection(1, trace.clone()))
            })
            .unwrap();
        gamepads.inject(&controller_event(0, 1, 1 << 12)).unwrap();
        trace.borrow_mut().fail_next_update = Some(0xE000_0004);
        trace.borrow_mut().fail_remove = true;
        assert!(gamepads.inject(&controller_event(0, 1, 1 << 13)).is_err());
        let delivered = trace.borrow().updates.len();

        assert!(gamepads.stop().is_err());
        assert_eq!(trace.borrow().updates.len(), delivered);
        assert_eq!(trace.borrow().attached, 1);
        trace.borrow_mut().fail_remove = false;
        gamepads.stop().unwrap();
        assert_eq!(trace.borrow().updates.len(), delivered);
        assert_eq!(trace.borrow().attached, 0);
        assert_eq!(trace.borrow().freed, 1);
        assert_eq!(trace.borrow().disconnects, 1);
    }

    #[test]
    fn failed_replacement_replay_keeps_its_target_for_stop_without_stale_report() {
        let trace = Rc::new(RefCell::new(FakeBusTrace::default()));
        let mut gamepads = WindowsGamepads::new();
        gamepads
            .start_with_profile(GamepadProfile::X360, || {
                Ok(fake_connection(1, trace.clone()))
            })
            .unwrap();
        gamepads.inject(&controller_event(0, 1, 1 << 12)).unwrap();
        gamepads.inject(&controller_event(1, 3, 1 << 13)).unwrap();

        {
            let mut bus = trace.borrow_mut();
            bus.fail_updates = vec![0xE000_0004, 0xE000_0004];
            bus.fail_remove_on_attempt = Some(2);
        }
        assert!(gamepads.inject(&controller_event(0, 3, 1 << 14)).is_err());
        assert!(gamepads.supported());
        assert_eq!(trace.borrow().removals, 2);
        assert_eq!(trace.borrow().attached, 2);
        assert_eq!(trace.borrow().freed, 1);
        assert_eq!(trace.borrow().updates, [(1, 0x1000), (1, 0x2000)]);

        // The failed replay must not retire the peer controller or prevent
        // another report from reaching it before lease cleanup.
        gamepads.inject(&controller_event(1, 3, 1 << 15)).unwrap();
        assert_eq!(trace.borrow().updates.last(), Some(&(1, 0x8000)));

        // Stop before a timer retry. The uncertain replacement is detached
        // directly, without a neutral report sent through that target.
        gamepads.stop().unwrap();
        assert_eq!(trace.borrow().removals, 4);
        assert_eq!(trace.borrow().attached, 0);
        assert_eq!(trace.borrow().freed, 3);
        assert_eq!(trace.borrow().disconnects, 1);
        assert_eq!(
            trace.borrow().updates,
            [(1, 0x1000), (1, 0x2000), (1, 0x8000), (1, 0)]
        );
        assert_eq!(trace.borrow().stale_reports, 0);
    }

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
