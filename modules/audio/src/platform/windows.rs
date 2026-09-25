//! All COM references and WASAPI buffers stay on one dedicated apartment thread.
use crate::{
    model::{AudioResult, Endpoint, Format, Packet},
    packet::{Timeline, copy_samples},
    platform::RecoveryConfig,
    route_policy::{
        AcquirePlan, DefaultEndpoints, DefaultRole, ReleasePlan, RoleAssignment, RoutePolicy,
    },
    virtual_sink::{
        ChannelLayout, VirtualFormatCandidate, VirtualSampleFormat, channel_layout,
        preferred_format_candidates, select_endpoint, select_policy_virtual_sink,
    },
};
use amaterasu_media::{
    AUDIO_DISCONTINUITY, AUDIO_SILENT, AUDIO_TIMESTAMP_ESTIMATED, PCM_F32, PCM_I16,
};
use std::{
    ffi::c_void,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Devices::FunctionDiscovery::{
            PKEY_Device_DeviceDesc, PKEY_Device_FriendlyName, PKEY_DeviceInterface_FriendlyName,
        },
        Media::{
            Audio::*, KernelStreaming::KSDATAFORMAT_SUBTYPE_PCM,
            Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
        },
        System::Com::{StructuredStorage::PropVariantToStringAlloc, *},
    },
    core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, PCWSTR, PWSTR},
};

const POLICY_CONFIG_CLASS: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
const POLICY_CONFIG_INTERFACE: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);
const HRESULT_NOT_FOUND: HRESULT = HRESULT(0x80070490u32 as i32);
const DEFAULT_ENDPOINT_POLL_INTERVAL: Duration = Duration::from_millis(100);

static ROUTE_POLICY: OnceLock<Mutex<RoutePolicy>> = OnceLock::new();

// IPolicyConfig is an undocumented Windows COM interface. The ten method
// slots after IUnknown precede SetDefaultEndpoint in the pinned interface ABI.
// Placeholder function pointers preserve the slot layout without calling those
// methods. The pointer is queried from the COM class and released by PolicyConfig.
#[repr(C)]
struct PolicyConfigVtable {
    base__: IUnknown_Vtbl,
    preceding_set_device_format: [unsafe extern "system" fn() -> HRESULT; 3],
    set_device_format: unsafe extern "system" fn(
        *mut c_void,
        PCWSTR,
        *mut WAVEFORMATEX,
        *mut WAVEFORMATEX,
    ) -> HRESULT,
    intervening_methods: [unsafe extern "system" fn() -> HRESULT; 6],
    set_default_endpoint: unsafe extern "system" fn(*mut c_void, PCWSTR, ERole) -> HRESULT,
}

struct PolicyConfig(*mut c_void);

impl PolicyConfig {
    fn new() -> AudioResult<Self> {
        // SAFETY: the COM apartment is initialized on this worker thread. The
        // returned IUnknown owns one reference; QueryInterface transfers a
        // second reference for PolicyConfig, which this wrapper releases.
        unsafe {
            let unknown: IUnknown = CoCreateInstance::<_, IUnknown>(
                &POLICY_CONFIG_CLASS,
                None::<&IUnknown>,
                CLSCTX_ALL,
            )
            .map_err(|_| "audio_route_unavailable")?;
            let unknown_vtable = *(unknown.as_raw() as *mut *const IUnknown_Vtbl);
            let mut interface = std::ptr::null_mut();
            ((*unknown_vtable).QueryInterface)(
                unknown.as_raw(),
                &POLICY_CONFIG_INTERFACE,
                &mut interface,
            )
            .ok()
            .map_err(|_| "audio_route_unavailable")?;
            if interface.is_null() {
                return Err("audio_route_unavailable");
            }
            Ok(Self(interface))
        }
    }

    fn set_default(&self, endpoint: &str, role: DefaultRole) -> AudioResult<()> {
        let wide: Vec<u16> = endpoint.encode_utf16().chain(Some(0)).collect();
        let role = match role {
            DefaultRole::Console => eConsole,
            DefaultRole::Multimedia => eMultimedia,
            DefaultRole::Communications => eCommunications,
        };
        // SAFETY: this vtable slot is SetDefaultEndpoint at IPolicyConfig slot
        // 13 (three IUnknown slots plus ten preceding interface methods). The
        // UTF-16 endpoint remains alive for the synchronous COM call.
        unsafe {
            let vtable = &**(self.0 as *mut *const PolicyConfigVtable);
            (vtable.set_default_endpoint)(self.0, PCWSTR(wide.as_ptr()), role)
                .ok()
                .map_err(|_| "audio_route_change_failed")
        }
    }

    fn set_device_format(
        &self,
        endpoint: &str,
        format: &mut WAVEFORMATEXTENSIBLE,
    ) -> AudioResult<()> {
        let wide: Vec<u16> = endpoint.encode_utf16().chain(Some(0)).collect();
        let mut default_format = WAVEFORMATEXTENSIBLE::default();
        // SAFETY: the vtable slot is SetDeviceFormat, the fourth IPolicyConfig
        // method after IUnknown. The UTF-16 ID and both extensible format
        // structures stay live for the synchronous COM call; the zeroed
        // default-format buffer has the full extension size expected by the
        // ABI. This undocumented call is isolated here and used only for an
        // explicitly selected virtual sink.
        unsafe {
            let vtable = &**(self.0 as *mut *const PolicyConfigVtable);
            (vtable.set_device_format)(
                self.0,
                PCWSTR(wide.as_ptr()),
                (&mut format.Format) as *mut WAVEFORMATEX,
                &mut default_format.Format,
            )
            .ok()
            .map_err(|_| "virtual_sink_format_unavailable")
        }
    }
}

impl Drop for PolicyConfig {
    fn drop(&mut self) {
        // SAFETY: QueryInterface returned one owned COM reference. Its Release
        // method is the first IUnknown vtable slot, and this wrapper drops on
        // the apartment thread that created it.
        unsafe {
            let vtable = &**(self.0 as *mut *const PolicyConfigVtable);
            (vtable.base__.Release)(self.0);
        }
    }
}

struct RouteLease {
    endpoint: String,
    active: bool,
}

impl RouteLease {
    fn unchanged(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_owned(),
            active: false,
        }
    }

    fn acquire(
        enumerator: &IMMDeviceEnumerator,
        endpoint: &str,
        capture_sink_without_routing: bool,
    ) -> AudioResult<Self> {
        if capture_sink_without_routing || endpoint == "default" {
            return Ok(Self::unchanged(endpoint));
        }

        let mutex = ROUTE_POLICY.get_or_init(|| Mutex::new(RoutePolicy::new()));
        let mut policy = mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !restore_pending(enumerator, &mut policy).map_err(|_| "audio_route_restore_failed")? {
            return Err("audio_route_restore_failed");
        }
        let original = default_endpoints(enumerator)?;
        match policy.acquire(endpoint, &original)? {
            AcquirePlan::Unchanged => Ok(Self::unchanged(endpoint)),
            AcquirePlan::Shared => Ok(Self {
                endpoint: endpoint.to_owned(),
                active: true,
            }),
            AcquirePlan::Assign(assignments) => {
                let result =
                    assign_defaults(enumerator, &mut policy, endpoint, &original, &assignments);
                if result.is_err() {
                    policy.abort_acquire(endpoint)?;
                    if !restore_pending(enumerator, &mut policy).unwrap_or(false) {
                        return Err("audio_route_restore_failed");
                    }
                    return Err("audio_route_change_failed");
                }
                Ok(Self {
                    endpoint: endpoint.to_owned(),
                    active: true,
                })
            }
        }
    }
}

impl Drop for RouteLease {
    fn drop(&mut self) {
        if self.active {
            release_route(&self.endpoint);
        }
    }
}

fn default_endpoints(enumerator: &IMMDeviceEnumerator) -> AudioResult<DefaultEndpoints> {
    let mut endpoints: DefaultEndpoints = std::array::from_fn(|_| None);
    for role in DefaultRole::ALL {
        endpoints[role.index()] = role_endpoint(enumerator, role)?;
    }
    Ok(endpoints)
}

fn role_endpoint(
    enumerator: &IMMDeviceEnumerator,
    role: DefaultRole,
) -> AudioResult<Option<String>> {
    let role = match role {
        DefaultRole::Console => eConsole,
        DefaultRole::Multimedia => eMultimedia,
        DefaultRole::Communications => eCommunications,
    };
    // SAFETY: this is a read-only query in the caller's live COM apartment.
    unsafe {
        match enumerator.GetDefaultAudioEndpoint(eRender, role) {
            Ok(device) => identifier(&device).map(Some),
            Err(error) if error.code() == HRESULT_NOT_FOUND => Ok(None),
            Err(_) => Err("audio_route_snapshot_failed"),
        }
    }
}

fn assign_defaults(
    enumerator: &IMMDeviceEnumerator,
    route_policy: &mut RoutePolicy,
    endpoint: &str,
    original: &DefaultEndpoints,
    assignments: &[RoleAssignment],
) -> AudioResult<()> {
    let policy = PolicyConfig::new()?;
    for assignment in assignments {
        let current = role_endpoint(enumerator, assignment.role)?;
        if current.as_deref() == Some(endpoint) {
            continue;
        }
        if current != original[assignment.role.index()] {
            // Another actor changed this role after our snapshot. Preserve it.
            continue;
        }
        policy.set_default(endpoint, assignment.role)?;
        route_policy.record_assignment(endpoint, assignment.role)?;
    }

    let current = default_endpoints(enumerator)?;
    if assignments
        .iter()
        .any(|assignment| current[assignment.role.index()] == original[assignment.role.index()])
    {
        return Err("audio_route_change_failed");
    }
    Ok(())
}

fn restore_pending(
    enumerator: &IMMDeviceEnumerator,
    policy: &mut RoutePolicy,
) -> AudioResult<bool> {
    let Some(endpoint) = policy.endpoint_with_pending_restore().map(str::to_owned) else {
        return Ok(true);
    };
    let current = default_endpoints(enumerator)?;
    let assignments = policy
        .pending_restore_assignments(&current)
        .ok_or("audio_route_unavailable")?;
    if !assignments.is_empty() {
        let endpoint_policy = PolicyConfig::new()?;
        for assignment in assignments {
            if role_endpoint(enumerator, assignment.role)?.as_deref() == Some(endpoint.as_str()) {
                // Keep trying the other roles after a failure. The final query
                // decides whether any role still needs restoration.
                let _ = endpoint_policy.set_default(&assignment.endpoint, assignment.role);
            }
        }
    }

    let current = default_endpoints(enumerator)?;
    let restored = policy
        .pending_restore_assignments(&current)
        .is_some_and(|assignments| assignments.is_empty());
    policy.complete_restore(restored);
    Ok(restored)
}

fn release_route(endpoint: &str) {
    let mutex = ROUTE_POLICY.get_or_init(|| Mutex::new(RoutePolicy::new()));
    let mut policy = mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Ok(enumerator) = enumerator() else {
        let _ = policy.release_without_snapshot(endpoint);
        return;
    };
    let current = match default_endpoints(&enumerator) {
        Ok(current) => current,
        Err(_) => {
            let _ = policy.release_without_snapshot(endpoint);
            return;
        }
    };
    match policy.release(endpoint, &current) {
        Ok(ReleasePlan::Shared) => {}
        Ok(ReleasePlan::Restore(_)) => {
            let _ = restore_pending(&enumerator, &mut policy);
        }
        Err(_) => {}
    }
}

pub fn route_restore_pending() -> bool {
    ROUTE_POLICY.get().is_some_and(|mutex| {
        let policy = mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        policy.endpoint_with_pending_restore().is_some()
    })
}

struct Apartment;
impl Apartment {
    fn start() -> AudioResult<Self> {
        // SAFETY: dedicated thread initializes once, balanced by its guard.
        // STA also satisfies IAudioClient's Windows 8 first-use requirement.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                .ok()
                .map_err(|_| "com_unavailable")?;
        }
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

struct TaskString(PWSTR);
impl TaskString {
    fn text(&self) -> AudioResult<String> {
        if self.0.is_null() {
            return Err("invalid_endpoint_metadata");
        }
        // SAFETY: COM returns a NUL-terminated allocation; stop at its terminator.
        // The metadata cap avoids unbounded control replies from endpoint names.
        unsafe {
            for length in 0..4096 {
                if self.0.0.add(length).read() == 0 {
                    return String::from_utf16(std::slice::from_raw_parts(self.0.0, length))
                        .map_err(|_| "invalid_endpoint_metadata");
                }
            }
        }
        Err("endpoint_metadata_limit")
    }
}

impl Drop for TaskString {
    fn drop(&mut self) {
        // SAFETY: endpoint and property helpers transfer one task allocation.
        unsafe {
            CoTaskMemFree(Some(self.0.0.cast()));
        }
    }
}

fn enumerator() -> AudioResult<IMMDeviceEnumerator> {
    // SAFETY: apartment is live; generated COM bindings own the result.
    unsafe {
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|_| "endpoint_enumeration_failed")
    }
}

fn identifier(device: &IMMDevice) -> AudioResult<String> {
    // SAFETY: GetId transfers a task allocation, freed by TaskString.
    unsafe { TaskString(device.GetId().map_err(|_| "endpoint_metadata_failed")?).text() }
}

pub fn enumerate() -> AudioResult<Vec<Endpoint>> {
    let _apartment = Apartment::start()?;
    let enumerator = enumerator()?;
    active_render_endpoints(&enumerator)
}

fn active_render_endpoints(enumerator: &IMMDeviceEnumerator) -> AudioResult<Vec<Endpoint>> {
    // SAFETY: active render endpoint enumeration opens no capture stream.
    unsafe {
        let default = enumerator
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .ok()
            .and_then(|device| identifier(&device).ok());
        let collection = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .map_err(|_| "endpoint_enumeration_failed")?;
        let count = collection
            .GetCount()
            .map_err(|_| "endpoint_enumeration_failed")?;
        if count > 64 {
            return Err("endpoint_limit");
        }
        let mut endpoints = Vec::new();
        for index in 0..count {
            let device = collection
                .Item(index)
                .map_err(|_| "endpoint_enumeration_failed")?;
            let id = identifier(&device)?;
            let properties = device
                .OpenPropertyStore(STGM_READ)
                .map_err(|_| "endpoint_metadata_failed")?;
            let name = property_text(&properties, &PKEY_Device_FriendlyName)
                .ok_or("endpoint_metadata_failed")?;
            let description = property_text(&properties, &PKEY_Device_DeviceDesc);
            let adapter_name = property_text(&properties, &PKEY_DeviceInterface_FriendlyName);
            endpoints.push(Endpoint {
                default_multimedia: default.as_ref() == Some(&id),
                id,
                name,
                description,
                adapter_name,
            });
        }
        Ok(endpoints)
    }
}

fn property_text(
    properties: &windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore,
    key: &windows::Win32::Foundation::PROPERTYKEY,
) -> Option<String> {
    // SAFETY: property store is live in its caller's apartment, and the
    // returned PROPVARIANT is converted synchronously before it is dropped.
    let value = unsafe { properties.GetValue(key).ok()? };
    // SAFETY: PropVariantToStringAlloc accepts the live PROPVARIANT and returns
    // one task allocation, owned by TaskString below.
    let text = unsafe { PropVariantToStringAlloc(&value).ok()? };
    TaskString(text).text().ok()
}

struct MixFormat(*mut WAVEFORMATEX);
impl Drop for MixFormat {
    fn drop(&mut self) {
        // SAFETY: GetMixFormat transfers one CoTaskMem allocation.
        unsafe {
            CoTaskMemFree(Some(self.0.cast()));
        }
    }
}

impl MixFormat {
    fn valid_bits(&self) -> Option<u16> {
        if self.0.is_null() {
            return None;
        }
        // SAFETY: GetMixFormat owns a live WAVEFORMATEX plus any declared
        // WAVEFORMATEXTENSIBLE bytes until this wrapper is dropped.
        unsafe {
            let base = self.0.read_unaligned();
            let valid_bits = if base.wFormatTag == 0xfffe && base.cbSize >= 22 {
                self.0
                    .cast::<WAVEFORMATEXTENSIBLE>()
                    .read_unaligned()
                    .Samples
                    .wValidBitsPerSample
            } else {
                base.wBitsPerSample
            };
            Some(if valid_bits == 0 {
                base.wBitsPerSample
            } else {
                valid_bits
            })
        }
    }

    fn channel_layout(&self) -> AudioResult<(u32, u32)> {
        if self.0.is_null() {
            return Err("unsupported_channel_layout");
        }
        // SAFETY: GetMixFormat owns a live WAVEFORMATEX plus any declared
        // WAVEFORMATEXTENSIBLE bytes until this wrapper is dropped. Copy packed
        // structures by value rather than forming potentially unaligned refs.
        unsafe {
            let base = self.0.read_unaligned();
            let channels = u32::from(base.nChannels);
            let mask = if base.wFormatTag == 0xfffe && base.cbSize >= 22 {
                self.0
                    .cast::<WAVEFORMATEXTENSIBLE>()
                    .read_unaligned()
                    .dwChannelMask
            } else {
                match channels {
                    1 => 0x0004,
                    2 => 0x0003,
                    _ => 0,
                }
            };
            if mask == 0 || mask.count_ones() != channels {
                return Err("unsupported_channel_layout");
            }
            Ok((channels, mask))
        }
    }

    fn describe(&self) -> AudioResult<Format> {
        if self.0.is_null() {
            return Err("unsupported_format");
        }
        // SAFETY: GetMixFormat provides WAVEFORMATEX and cbSize extension bytes;
        // read packed structures by value, never take aligned field references.
        unsafe {
            let base = self.0.read_unaligned();
            let mut valid_bits = base.wBitsPerSample;
            let mut mask = None;
            let subtype = if base.wFormatTag == 0xfffe {
                if base.cbSize < 22 {
                    return Err("unsupported_format");
                }
                let extended = self.0.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned();
                valid_bits = extended.Samples.wValidBitsPerSample;
                mask = Some(extended.dwChannelMask);
                extended.SubFormat
            } else if base.wFormatTag == 1 {
                KSDATAFORMAT_SUBTYPE_PCM
            } else if base.wFormatTag == 3 {
                KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
            } else {
                return Err("unsupported_format");
            };
            let code = if subtype == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
                && base.wBitsPerSample == 32
                && valid_bits == 32
            {
                PCM_F32
            } else if subtype == KSDATAFORMAT_SUBTYPE_PCM
                && base.wBitsPerSample == 16
                && valid_bits == 16
            {
                PCM_I16
            } else {
                return Err("unsupported_format");
            };
            let mut format = Format::new(base.nSamplesPerSec, u32::from(base.nChannels), code)?;
            if base.nBlockAlign as usize != format.block_align()
                || base.nAvgBytesPerSec != base.nSamplesPerSec * u32::from(base.nBlockAlign)
            {
                return Err("unsupported_format");
            }
            if let Some(mask) = mask {
                if mask != 0 && mask.count_ones() != format.channels {
                    return Err("unsupported_channel_layout");
                }
                format.channel_mask = mask;
            }
            Ok(format)
        }
    }
}

fn virtual_waveformat(candidate: VirtualFormatCandidate, channels: u32) -> WAVEFORMATEXTENSIBLE {
    let sample_format = candidate.sample_format;
    let bits_per_sample = sample_format.container_bits();
    let block_align = (channels * u32::from(bits_per_sample) / 8) as u16;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: 0xfffe,
            nChannels: channels as u16,
            nSamplesPerSec: 48_000,
            nAvgBytesPerSec: 48_000 * u32::from(block_align),
            nBlockAlign: block_align,
            wBitsPerSample: bits_per_sample,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: sample_format.valid_bits(),
        },
        dwChannelMask: candidate.channel_mask,
        SubFormat: if sample_format.is_float() {
            KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        } else {
            KSDATAFORMAT_SUBTYPE_PCM
        },
    }
}

fn default_render_valid_bits(enumerator: &IMMDeviceEnumerator) -> u16 {
    // SAFETY: endpoint activation and GetMixFormat are read-only COM calls in
    // the worker's live apartment. MixFormat frees the returned allocation.
    unsafe {
        let Ok(device) = enumerator.GetDefaultAudioEndpoint(eRender, eConsole) else {
            return 32;
        };
        let Ok(client) = device.Activate::<IAudioClient>(CLSCTX_ALL, None) else {
            return 32;
        };
        let Ok(format) = client.GetMixFormat() else {
            return 32;
        };
        MixFormat(format).valid_bits().unwrap_or(32)
    }
}

fn set_virtual_device_layout(
    endpoint: &str,
    layout: ChannelLayout,
    preferred_valid_bits: u16,
) -> AudioResult<VirtualFormatCandidate> {
    let policy = PolicyConfig::new().map_err(|_| "virtual_sink_format_unavailable")?;
    let candidates = preferred_format_candidates(layout.channels, preferred_valid_bits)?;
    for candidate in candidates {
        let mut format = virtual_waveformat(candidate, layout.channels);
        if policy.set_device_format(endpoint, &mut format).is_ok() {
            return Ok(candidate);
        }
    }
    Err("virtual_sink_format_unavailable")
}

fn validate_virtual_mix(
    mix: &MixFormat,
    layout: ChannelLayout,
    candidate: VirtualFormatCandidate,
) -> AudioResult<()> {
    let (channels, channel_mask) = mix.channel_layout()?;
    if channels != layout.channels || channel_mask != candidate.channel_mask {
        return Err("unsupported_channel_layout");
    }
    Ok(())
}

struct OpenSelectedOptions {
    desired_layout: Option<ChannelLayout>,
    preferred_valid_bits: Option<u16>,
    capture_sink_without_routing: bool,
    follow_console_default: bool,
}

pub struct Capture {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    endpoint: String,
    follow_console_default: bool,
    next_console_default_check: Instant,
    format: Format,
    timeline: Timeline,
    recovery_config: RecoveryConfig,
    _route_lease: RouteLease,
    _apartment: Apartment,
}

fn validate_render_device(device: &IMMDevice) -> AudioResult<()> {
    // SAFETY: the device was obtained from the current COM apartment's enumerator.
    unsafe {
        let endpoint_type: IMMEndpoint = device.cast().map_err(|_| "invalid_endpoint")?;
        if endpoint_type
            .GetDataFlow()
            .map_err(|_| "invalid_endpoint")?
            != eRender
        {
            return Err("not_render_endpoint");
        }
        Ok(())
    }
}

impl Capture {
    pub fn open(
        requested: &str,
        virtual_sink: Option<&str>,
        channels: Option<u32>,
        host_audio_enabled: bool,
        capture_sink_without_routing: bool,
    ) -> AudioResult<Self> {
        let apartment = Apartment::start()?;
        let enumerator = enumerator()?;
        let active_endpoints = if virtual_sink.is_some() {
            Some(active_render_endpoints(&enumerator)?)
        } else if !host_audio_enabled || requested.is_empty() || requested == "default" {
            active_render_endpoints(&enumerator).ok()
        } else {
            None
        };
        let automatic_virtual_sink = active_endpoints.as_deref().and_then(|endpoints| {
            select_policy_virtual_sink(endpoints, requested, virtual_sink, host_audio_enabled)
        });
        let uses_virtual_sink = virtual_sink.is_some() || automatic_virtual_sink.is_some();
        let effective_channels = channels.or_else(|| automatic_virtual_sink.map(|_| 2));
        let desired_layout = uses_virtual_sink
            .then(|| channel_layout(effective_channels.unwrap_or(2)))
            .transpose()?;
        if virtual_sink.is_some() && capture_sink_without_routing {
            return Err("invalid_open_policy");
        }
        let effective_capture_only =
            capture_sink_without_routing && automatic_virtual_sink.is_none();
        let resolve_console_default =
            super::should_resolve_console_default(requested, uses_virtual_sink);
        let follow_console_default = super::should_follow_console_default(
            requested,
            uses_virtual_sink,
            effective_capture_only,
        );
        let preferred_valid_bits = desired_layout.map(|_| default_render_valid_bits(&enumerator));
        // SAFETY: activation and capture interface references never leave this
        // apartment. The mix-format allocation stays live through Initialize.
        unsafe {
            let device = if let Some(selector) = virtual_sink {
                let endpoints = active_endpoints.as_deref().ok_or("endpoint_not_found")?;
                let selected = select_endpoint(endpoints, selector).ok_or("endpoint_not_found")?;
                let wide: Vec<u16> = selected.id.encode_utf16().chain(Some(0)).collect();
                enumerator.GetDevice(PCWSTR(wide.as_ptr()))
            } else if let Some(selected) = automatic_virtual_sink {
                let wide: Vec<u16> = selected.id.encode_utf16().chain(Some(0)).collect();
                enumerator.GetDevice(PCWSTR(wide.as_ptr()))
            } else if requested.is_empty() || requested == "default" {
                let role = if resolve_console_default {
                    eConsole
                } else {
                    eMultimedia
                };
                enumerator.GetDefaultAudioEndpoint(eRender, role)
            } else {
                let wide: Vec<u16> = requested.encode_utf16().chain(Some(0)).collect();
                enumerator.GetDevice(PCWSTR(wide.as_ptr()))
            }
            .map_err(|_| "endpoint_not_found")?;
            validate_render_device(&device)?;
            let endpoint = identifier(&device)?;
            Self::open_selected(
                apartment,
                &enumerator,
                device,
                endpoint,
                OpenSelectedOptions {
                    desired_layout,
                    preferred_valid_bits,
                    capture_sink_without_routing: effective_capture_only,
                    follow_console_default,
                },
            )
        }
    }

    pub fn reopen(config: &RecoveryConfig) -> AudioResult<Self> {
        let apartment = Apartment::start()?;
        let enumerator = enumerator()?;
        // SAFETY: the endpoint ID was returned by this backend and all COM
        // interfaces remain local to the worker's apartment thread.
        unsafe {
            let device = if config.follow_console_default {
                enumerator.GetDefaultAudioEndpoint(eRender, eConsole)
            } else {
                let wide: Vec<u16> = config.endpoint.encode_utf16().chain(Some(0)).collect();
                enumerator.GetDevice(PCWSTR(wide.as_ptr()))
            }
            .map_err(|_| "endpoint_not_found")?;
            validate_render_device(&device)?;
            let endpoint = identifier(&device)?;
            if !config.follow_console_default && endpoint != config.endpoint {
                return Err("audio_endpoint_changed");
            }
            let desired_layout = config.virtual_channels.map(channel_layout).transpose()?;
            Self::open_selected(
                apartment,
                &enumerator,
                device,
                endpoint,
                OpenSelectedOptions {
                    desired_layout,
                    preferred_valid_bits: config.preferred_valid_bits,
                    capture_sink_without_routing: config.capture_sink_without_routing,
                    follow_console_default: config.follow_console_default,
                },
            )
        }
    }

    pub fn recovery_config(&self) -> RecoveryConfig {
        self.recovery_config.clone()
    }

    fn open_selected(
        apartment: Apartment,
        enumerator: &IMMDeviceEnumerator,
        device: IMMDevice,
        endpoint: String,
        options: OpenSelectedOptions,
    ) -> AudioResult<Self> {
        let OpenSelectedOptions {
            desired_layout,
            preferred_valid_bits,
            capture_sink_without_routing,
            follow_console_default,
        } = options;
        // SAFETY: the caller created this device and enumerator in the live
        // worker apartment; all COM operations remain on that thread.
        unsafe {
            let route_lease = RouteLease::acquire(
                enumerator,
                &endpoint,
                capture_sink_without_routing || follow_console_default,
            )?;
            let selected_virtual_format = match (desired_layout, preferred_valid_bits) {
                (Some(layout), Some(valid_bits)) => {
                    Some(set_virtual_device_layout(&endpoint, layout, valid_bits)?)
                }
                (None, None) => None,
                _ => return Err("virtual_sink_format_unavailable"),
            };
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|_| "audio_activation_failed")?;
            let mix = MixFormat(client.GetMixFormat().map_err(|_| "unsupported_format")?);
            let mut capture_format = match (desired_layout, selected_virtual_format) {
                (Some(layout), Some(candidate)) => {
                    validate_virtual_mix(&mix, layout, candidate)?;
                    let mut format = Format::new(48_000, layout.channels, PCM_F32)?;
                    format.channel_mask = candidate.channel_mask;
                    let waveformat = virtual_waveformat(
                        VirtualFormatCandidate {
                            sample_format: VirtualSampleFormat::Float32,
                            channel_mask: candidate.channel_mask,
                        },
                        layout.channels,
                    );
                    Some((format, waveformat))
                }
                (None, None) => None,
                _ => return Err("virtual_sink_format_unavailable"),
            };
            let format = match &capture_format {
                Some((format, _)) => *format,
                None => mix.describe()?,
            };
            let mut stream_flags = AUDCLNT_STREAMFLAGS_LOOPBACK;
            let format_pointer = if let Some((_, waveformat)) = capture_format.as_mut() {
                stream_flags |=
                    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
                &mut waveformat.Format as *mut WAVEFORMATEX
            } else {
                mix.0
            };
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    stream_flags,
                    1_000_000,
                    0,
                    format_pointer,
                    None,
                )
                .map_err(|_| "audio_initialize_failed")?;
            let capture = client
                .GetService()
                .map_err(|_| "audio_capture_unavailable")?;
            client.Start().map_err(|_| "audio_start_failed")?;
            let recovery_config = RecoveryConfig {
                endpoint: endpoint.clone(),
                follow_console_default,
                virtual_channels: desired_layout.map(|layout| layout.channels),
                preferred_valid_bits,
                capture_sink_without_routing,
            };
            Ok(Self {
                capture,
                client,
                endpoint,
                follow_console_default,
                next_console_default_check: Instant::now() + DEFAULT_ENDPOINT_POLL_INTERVAL,
                format,
                timeline: Timeline::new(),
                recovery_config,
                _route_lease: route_lease,
                _apartment: apartment,
            })
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn format(&self) -> Format {
        self.format
    }

    pub fn read(&mut self) -> AudioResult<Option<Packet>> {
        if self.follow_console_default && Instant::now() >= self.next_console_default_check {
            self.next_console_default_check = Instant::now() + DEFAULT_ENDPOINT_POLL_INTERVAL;
            if !self.is_current_console_default() {
                return Err("default_endpoint_changed");
            }
        }
        // SAFETY: GetBuffer/ReleaseBuffer are paired on the same worker thread;
        // copy_samples never borrows uninitialized padding or silent pointers.
        unsafe {
            if self.capture.GetNextPacketSize().map_err(capture_error)? == 0 {
                return Ok(None);
            }
            let mut pointer = std::ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            let mut qpc = 0;
            self.capture
                .GetBuffer(&mut pointer, &mut frames, &mut flags, None, Some(&mut qpc))
                .map_err(capture_error)?;
            if frames == 0 {
                return Ok(None);
            }
            let buffer = BorrowedPacket {
                capture: &self.capture,
                frames,
            };
            let flags = packet_flags(flags);
            let bytes = copy_samples(self.format, frames, flags, pointer);
            buffer.release()?;
            let packet = self.timeline.packet(frames, flags, bytes?, qpc)?;
            Ok(Some(packet))
        }
    }

    fn is_current_console_default(&self) -> bool {
        let Ok(enumerator) = enumerator() else {
            // A failed check triggers the worker's recoverable default reopen.
            return false;
        };
        // SAFETY: this read-only query runs on the capture's live apartment
        // thread; no COM interface leaves the worker.
        let Ok(device) = (unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }) else {
            return false;
        };
        identifier(&device).is_ok_and(|endpoint| endpoint == self.endpoint)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: worker stopped polling before capture teardown. COM references
        // drop before the apartment guard, even if Stop reports device loss.
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

struct BorrowedPacket<'a> {
    capture: &'a IAudioCaptureClient,
    frames: u32,
}

impl BorrowedPacket<'_> {
    fn release(mut self) -> AudioResult<()> {
        let frames = std::mem::take(&mut self.frames);
        // SAFETY: releases exactly the count returned by GetBuffer, once.
        unsafe { self.capture.ReleaseBuffer(frames).map_err(capture_error) }
    }
}

impl Drop for BorrowedPacket<'_> {
    fn drop(&mut self) {
        if self.frames != 0 {
            // SAFETY: fallback during unwinding/error before explicit release.
            unsafe {
                let _ = self.capture.ReleaseBuffer(self.frames);
            }
        }
    }
}

fn packet_flags(flags: u32) -> u32 {
    let mut result = 0;
    if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
        result |= AUDIO_DISCONTINUITY;
    }
    if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
        result |= AUDIO_SILENT;
    }
    if flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0 {
        result |= AUDIO_TIMESTAMP_ESTIMATED;
    }
    result
}

fn capture_error(error: windows::core::Error) -> &'static str {
    if error.code() == AUDCLNT_E_DEVICE_INVALIDATED || error.code() == AUDCLNT_E_SERVICE_NOT_RUNNING
    {
        "device_lost"
    } else {
        "audio_capture_failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mix_format_preserves_channel_mask_and_rejects_unsupported_container_bits() {
        let mut mix = WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: 0xfffe,
                nChannels: 6,
                nSamplesPerSec: 48_000,
                nAvgBytesPerSec: 48_000 * 24,
                nBlockAlign: 24,
                wBitsPerSample: 32,
                cbSize: 22,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 {
                wValidBitsPerSample: 32,
            },
            dwChannelMask: 0x3f,
            SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
        };
        // This borrowed test descriptor is not a COM allocation; ManuallyDrop
        // prevents the production owner from freeing stack storage.
        let descriptor =
            std::mem::ManuallyDrop::new(MixFormat((&mut mix as *mut WAVEFORMATEXTENSIBLE).cast()));
        let format = descriptor.describe().unwrap();
        assert_eq!(
            (format.channels, format.channel_mask, format.code()),
            (6, 0x3f, PCM_F32)
        );
        mix.Format.wBitsPerSample = 24;
        let changed =
            std::mem::ManuallyDrop::new(MixFormat((&mut mix as *mut WAVEFORMATEXTENSIBLE).cast()));
        assert_eq!(changed.describe().unwrap_err(), "unsupported_format");
    }

    #[test]
    fn mix_format_preference_reads_extensible_valid_bits_for_default_depth_selection() {
        let mut default_mix = virtual_waveformat(
            VirtualFormatCandidate {
                sample_format: VirtualSampleFormat::Signed24In32,
                channel_mask: 0x0003,
            },
            2,
        );
        let descriptor = std::mem::ManuallyDrop::new(MixFormat(
            (&mut default_mix as *mut WAVEFORMATEXTENSIBLE).cast(),
        ));
        let preferred_bits = descriptor.valid_bits().unwrap();
        assert_eq!(preferred_bits, 24);
        assert_eq!(
            preferred_format_candidates(2, preferred_bits)
                .unwrap()
                .iter()
                .map(|candidate| candidate.sample_format)
                .collect::<Vec<_>>(),
            [
                VirtualSampleFormat::Signed24In32,
                VirtualSampleFormat::Signed24
            ]
        );
    }

    #[test]
    fn virtual_waveformats_keep_container_depth_subtype_and_valid_bits_distinct() {
        for (sample_format, expected_container_bits, expected_valid_bits, expect_float) in [
            (VirtualSampleFormat::Float32, 32, 32, true),
            (VirtualSampleFormat::Signed32, 32, 32, false),
            (VirtualSampleFormat::Signed24In32, 32, 24, false),
            (VirtualSampleFormat::Signed24, 24, 24, false),
            (VirtualSampleFormat::Signed16, 16, 16, false),
        ] {
            let waveformat = virtual_waveformat(
                VirtualFormatCandidate {
                    sample_format,
                    channel_mask: 0x003f,
                },
                6,
            );
            let container_bits = waveformat.Format.wBitsPerSample;
            let valid_bits = unsafe { waveformat.Samples.wValidBitsPerSample };
            let sample_rate = waveformat.Format.nSamplesPerSec;
            let block_align = waveformat.Format.nBlockAlign;
            let average_bytes_per_second = waveformat.Format.nAvgBytesPerSec;
            let sub_format = waveformat.SubFormat;

            assert_eq!(container_bits, expected_container_bits);
            assert_eq!(valid_bits, expected_valid_bits);
            assert_eq!(sample_rate, 48_000);
            assert_eq!(block_align, 6 * expected_container_bits / 8);
            assert_eq!(average_bytes_per_second, 48_000 * u32::from(block_align));
            assert_eq!(
                sub_format,
                if expect_float {
                    KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
                } else {
                    KSDATAFORMAT_SUBTYPE_PCM
                }
            );
        }
    }

    fn defaults() -> Vec<Option<String>> {
        let _apartment = Apartment::start().unwrap();
        let enumerator = enumerator().unwrap();
        [eConsole, eMultimedia, eCommunications]
            .into_iter()
            .map(|role| {
                // SAFETY: read-only role query in this test's live COM apartment.
                unsafe {
                    enumerator
                        .GetDefaultAudioEndpoint(eRender, role)
                        .ok()
                        .map(|device| identifier(&device).unwrap())
                }
            })
            .collect()
    }

    #[test]
    #[ignore = "requires a Windows test endpoint playing controlled audio; records loopback PCM"]
    fn explicit_endpoint_captures_without_changing_any_default_role() {
        let devices = enumerate().unwrap();
        let endpoint = devices
            .iter()
            .find(|device| !device.default_multimedia)
            .or(devices.first())
            .expect("test needs an active render endpoint");
        let before = defaults();
        let mut capture = Capture::open(&endpoint.id, None, None, true, true).unwrap();
        assert_eq!(capture.endpoint(), endpoint.id);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut packets = 0;
        while std::time::Instant::now() < deadline {
            if let Some(packet) = capture.read().unwrap() {
                assert_eq!(
                    packet.bytes.len(),
                    capture.format().packet_bytes(packet.frames).unwrap()
                );
                packets += 1;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        drop(capture);
        assert_eq!(
            defaults(),
            before,
            "capture must not change default render roles"
        );
        assert!(
            packets > 0,
            "play controlled test audio on the explicitly selected endpoint"
        );
    }
}
