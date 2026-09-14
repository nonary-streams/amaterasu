//! All COM references and WASAPI buffers stay on one dedicated apartment thread.
use crate::{
    model::{AudioResult, Endpoint, Format, Packet},
    packet::{Timeline, copy_samples},
};
use amaterasu_media::{
    AUDIO_DISCONTINUITY, AUDIO_SILENT, AUDIO_TIMESTAMP_ESTIMATED, PCM_F32, PCM_I16,
};
use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Media::{
            Audio::*, KernelStreaming::KSDATAFORMAT_SUBTYPE_PCM,
            Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
        },
        System::Com::{StructuredStorage::PropVariantToStringAlloc, *},
    },
    core::{Interface, PCWSTR, PWSTR},
};

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
            let value = properties
                .GetValue(&PKEY_Device_FriendlyName)
                .map_err(|_| "endpoint_metadata_failed")?;
            let name = TaskString(
                PropVariantToStringAlloc(&value).map_err(|_| "endpoint_metadata_failed")?,
            )
            .text()?;
            endpoints.push(Endpoint {
                default_multimedia: default.as_ref() == Some(&id),
                id,
                name,
            });
        }
        Ok(endpoints)
    }
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

pub struct Capture {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    endpoint: String,
    format: Format,
    timeline: Timeline,
    _apartment: Apartment,
}

impl Capture {
    pub fn open(requested: &str) -> AudioResult<Self> {
        let apartment = Apartment::start()?;
        let enumerator = enumerator()?;
        // SAFETY: activation and capture interface references never leave this
        // apartment. The mix-format allocation stays live through Initialize.
        unsafe {
            let device = if requested == "default" {
                enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia)
            } else {
                let wide: Vec<u16> = requested.encode_utf16().chain(Some(0)).collect();
                enumerator.GetDevice(PCWSTR(wide.as_ptr()))
            }
            .map_err(|_| "endpoint_not_found")?;
            let endpoint_type: IMMEndpoint = device.cast().map_err(|_| "invalid_endpoint")?;
            if endpoint_type
                .GetDataFlow()
                .map_err(|_| "invalid_endpoint")?
                != eRender
            {
                return Err("not_render_endpoint");
            }
            let endpoint = identifier(&device)?;
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|_| "audio_activation_failed")?;
            let mix = MixFormat(client.GetMixFormat().map_err(|_| "unsupported_format")?);
            let format = mix.describe()?;
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK,
                    1_000_000,
                    0,
                    mix.0,
                    None,
                )
                .map_err(|_| "audio_initialize_failed")?;
            let capture = client
                .GetService()
                .map_err(|_| "audio_capture_unavailable")?;
            client.Start().map_err(|_| "audio_start_failed")?;
            let capture = Self {
                capture,
                client,
                endpoint,
                format,
                timeline: Timeline::new(),
                _apartment: apartment,
            };
            Ok(capture)
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn format(&self) -> Format {
        self.format
    }

    pub fn read(&mut self) -> AudioResult<Option<Packet>> {
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
        let mut capture = Capture::open(&endpoint.id).unwrap();
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
