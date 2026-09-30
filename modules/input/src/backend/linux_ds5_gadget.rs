//! Optional `/dev/vibeshine-ds5` transport for the Linux DualSense profile.
//!
//! This mirrors the pinned kernel UAPI layout without importing the module's
//! private header. Opening and creating the UAPI slot makes one USB gadget that
//! includes both HID input and four-channel USB audio; callers use UHID only if
//! this transport cannot be acquired.

#[cfg(not(test))]
use super::DS5_REPORT_INTERVAL;
use super::Ds5HapticsPacketizer;
#[cfg(not(test))]
use super::Ds5ReportState;
use super::dispatch_output_report;
use crate::feedback::FeedbackProducer;
#[cfg(not(test))]
use crate::model::{InputError, InputResult};
#[cfg(not(test))]
use std::fs::{File, OpenOptions};
#[cfg(test)]
use std::mem::offset_of;
use std::mem::size_of;
#[cfg(not(test))]
use std::os::fd::{AsRawFd, RawFd};
#[cfg(not(test))]
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
#[cfg(not(test))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(test))]
use std::sync::{Arc, Mutex};
#[cfg(not(test))]
use std::thread::{self, JoinHandle};
#[cfg(test)]
use std::time::Instant;
#[cfg(not(test))]
use std::time::{Duration, Instant};

#[cfg(not(test))]
const DEVICE_PATH: &str = "/dev/vibeshine-ds5";
#[cfg(not(test))]
const UAPI_SLOT_COUNT: u8 = 4;
pub(super) const EVENT_OUTPUT: u32 = 1;
pub(super) const EVENT_PCM: u32 = 2;
const UAPI_IOCTL_MAGIC: u64 = 0xB5;
const UAPI_IOCTL_WRITE: u64 = 1;
const IOCTL_CREATE: libc::c_ulong = ((UAPI_IOCTL_WRITE << 30)
    | ((size_of::<CreateRequest>() as u64) << 16)
    | (UAPI_IOCTL_MAGIC << 8)
    | 1) as libc::c_ulong;
const IOCTL_DESTROY: libc::c_ulong = ((UAPI_IOCTL_MAGIC << 8) | 2) as libc::c_ulong;
const MAX_EVENT_DATA: usize = 512;

#[repr(C)]
#[derive(Clone, Copy)]
struct CreateRequest {
    slot: u32,
    mac: [u8; 6],
    reserved: [u8; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NativeEvent {
    event_type: u32,
    size: u32,
    data: [u8; MAX_EVENT_DATA],
}

impl Default for NativeEvent {
    fn default() -> Self {
        Self {
            event_type: 0,
            size: 0,
            data: [0; MAX_EVENT_DATA],
        }
    }
}

impl NativeEvent {
    fn payload(&self) -> Option<(u32, &[u8])> {
        let size = usize::try_from(self.size).ok()?;
        if size > self.data.len() || !matches!(self.event_type, EVENT_OUTPUT | EVENT_PCM) {
            return None;
        }
        Some((self.event_type, &self.data[..size]))
    }
}

fn dispatch_native_event(
    event: &NativeEvent,
    packetizer: &mut Ds5HapticsPacketizer,
    feedback: &FeedbackProducer,
    now: Instant,
) {
    let Some((event_type, payload)) = event.payload() else {
        return;
    };
    match event_type {
        EVENT_OUTPUT => {
            dispatch_output_report(payload, feedback);
        }
        EVENT_PCM => packetizer.push(payload, feedback, now),
        _ => {}
    }
}

#[cfg(not(test))]
pub(super) struct GadgetDevice {
    file: Arc<Mutex<File>>,
    report: Arc<Mutex<Ds5ReportState>>,
    failure: Arc<Mutex<Option<InputError>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    destroyed: bool,
}

#[cfg(not(test))]
impl GadgetDevice {
    pub(super) fn create(
        client_slot: u8,
        report: Arc<Mutex<Ds5ReportState>>,
        address: [u8; 6],
        feedback: FeedbackProducer,
    ) -> InputResult<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(DEVICE_PATH)
            .map_err(gadget_error)?;
        if !file
            .metadata()
            .map_err(gadget_error)?
            .file_type()
            .is_char_device()
        {
            return Err(InputError::new(
                "invalid_device",
                "/dev/vibeshine-ds5 must be a character device",
            ));
        }

        let file = Arc::new(Mutex::new(file));
        let request = CreateRequest {
            // The UAPI exposes four named gadget slots. Stable/random MACs use
            // the same final-octet mapping as the pinned reference implementation.
            slot: u32::from(address[5] % UAPI_SLOT_COUNT),
            mac: address,
            reserved: [0; 2],
        };
        create_slot(&file, &request)?;

        let failure = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let worker_file = Arc::clone(&file);
            let worker_report = Arc::clone(&report);
            let worker_failure = Arc::clone(&failure);
            let worker_stop = Arc::clone(&stop);
            let worker_feedback = feedback;
            thread::Builder::new()
                .name(format!("amaterasu-ds5-gadget-{client_slot}"))
                .spawn(move || {
                    if let Err(error) = run_gadget_loop(
                        &worker_file,
                        &worker_report,
                        &worker_feedback,
                        &worker_stop,
                    ) {
                        *worker_failure
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                    }
                })
                .map_err(|error| {
                    let _ = destroy_slot(&file);
                    InputError::new(
                        "controller_unavailable",
                        format!("cannot start DS5 gadget event thread: {error}"),
                    )
                })?
        };

        Ok(Self {
            file,
            report,
            failure,
            stop,
            worker: Some(worker),
            destroyed: false,
        })
    }

    pub(super) fn send_report(&mut self) -> InputResult<()> {
        self.check_failure()?;
        let report = self
            .report
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .input_report();
        write_input_report(&self.file, &report)
    }

    pub(super) fn check_failure(&self) -> InputResult<()> {
        if let Some(error) = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            Err(error.clone())
        } else {
            Ok(())
        }
    }

    fn stop_worker(&mut self) -> InputResult<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| {
                InputError::new("release_failed", "DS5 gadget event thread panicked")
            })?;
        }
        Ok(())
    }

    pub(super) fn destroy(&mut self) -> InputResult<()> {
        if self.destroyed {
            return Ok(());
        }
        self.stop_worker()?;
        destroy_slot(&self.file)?;
        self.destroyed = true;
        Ok(())
    }
}

#[cfg(not(test))]
impl Drop for GadgetDevice {
    fn drop(&mut self) {
        let _ = self.stop_worker();
        if !self.destroyed {
            let _ = destroy_slot(&self.file);
        }
    }
}

#[cfg(not(test))]
fn create_slot(file: &Arc<Mutex<File>>, request: &CreateRequest) -> InputResult<()> {
    let file = file
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: request has the exact repr(C) layout and remains initialized until
    // the synchronous ioctl returns; the kernel copies these 12 bytes.
    let status = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            IOCTL_CREATE,
            std::ptr::from_ref(request).cast::<libc::c_void>(),
        )
    };
    if status < 0 {
        return Err(gadget_error(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(test))]
fn destroy_slot(file: &Arc<Mutex<File>>) -> InputResult<()> {
    let file = file
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: the no-argument UAPI ioctl uses only this live, owned file handle.
    let status = unsafe { libc::ioctl(file.as_raw_fd(), IOCTL_DESTROY) };
    if status < 0 {
        return Err(gadget_error(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(test))]
fn run_gadget_loop(
    file: &Arc<Mutex<File>>,
    report: &Arc<Mutex<Ds5ReportState>>,
    feedback: &FeedbackProducer,
    stop: &AtomicBool,
) -> InputResult<()> {
    let mut packetizer = Ds5HapticsPacketizer::default();
    let mut next_report = Instant::now();
    while !stop.load(Ordering::Acquire) {
        let timeout = next_report
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(20));
        let mut descriptor = libc::pollfd {
            fd: file
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: descriptor is one initialized record and the Arc-owned file
        // remains alive for the duration of poll.
        let ready = unsafe {
            libc::poll(
                &mut descriptor,
                1,
                timeout.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(gadget_error(error));
        }
        if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(InputError::new(
                "controller_unavailable",
                "Linux DS5 gadget stopped accepting events",
            ));
        }
        if descriptor.revents & libc::POLLIN != 0
            && let Some(event) = read_event(descriptor.fd)?
        {
            dispatch_native_event(&event, &mut packetizer, feedback, Instant::now());
        }
        if Instant::now() >= next_report {
            let input_report = report
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .input_report();
            write_input_report(file, &input_report)?;
            next_report = Instant::now() + DS5_REPORT_INTERVAL;
        }
    }
    Ok(())
}

#[cfg(not(test))]
fn read_event(fd: RawFd) -> InputResult<Option<NativeEvent>> {
    let mut event = NativeEvent::default();
    loop {
        // SAFETY: event is fully initialized and writable for the complete
        // native record; the driver copies one fixed-size record synchronously.
        let bytes = unsafe {
            libc::read(
                fd,
                std::ptr::from_mut(&mut event).cast::<libc::c_void>(),
                size_of::<NativeEvent>(),
            )
        };
        if bytes < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() == std::io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(gadget_error(error));
        }
        if bytes as usize != size_of::<NativeEvent>() {
            return Err(InputError::new(
                "controller_unavailable",
                "Linux DS5 gadget returned an incomplete event record",
            ));
        }
        return Ok(Some(event));
    }
}

#[cfg(not(test))]
fn write_input_report(
    file: &Arc<Mutex<File>>,
    report: &[u8; super::DS5_INPUT_REPORT_SIZE],
) -> InputResult<()> {
    let file = file
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        // SAFETY: report remains initialized and borrowed until the synchronous
        // kernel write returns. The driver copies these bytes before returning.
        let bytes = unsafe {
            libc::write(
                file.as_raw_fd(),
                report.as_ptr().cast::<libc::c_void>(),
                report.len(),
            )
        };
        if bytes < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(gadget_error(error));
        }
        if bytes as usize != report.len() {
            return Err(InputError::new(
                "injection_failed",
                "Linux DS5 gadget accepted an incomplete input report",
            ));
        }
        return Ok(());
    }
}

#[cfg(not(test))]
fn gadget_error(error: std::io::Error) -> InputError {
    InputError::new(
        "controller_unavailable",
        format!("Linux DS5 gadget: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_gamestream::control::HostFeedback;

    #[test]
    fn uapi_struct_layout_and_ioctl_numbers_match_the_pinned_header() {
        assert_eq!(size_of::<CreateRequest>(), 12);
        assert_eq!(offset_of!(CreateRequest, slot), 0);
        assert_eq!(offset_of!(CreateRequest, mac), 4);
        assert_eq!(offset_of!(CreateRequest, reserved), 10);
        assert_eq!(size_of::<NativeEvent>(), 520);
        assert_eq!(offset_of!(NativeEvent, event_type), 0);
        assert_eq!(offset_of!(NativeEvent, size), 4);
        assert_eq!(offset_of!(NativeEvent, data), 8);
        assert_eq!(IOCTL_CREATE, 0x400c_b501);
        assert_eq!(IOCTL_DESTROY, 0x0000_b502);
    }

    #[test]
    fn native_event_decoder_bounds_payload_and_rejects_unknown_types() {
        let mut event = NativeEvent {
            event_type: EVENT_PCM,
            size: 8,
            ..NativeEvent::default()
        };
        event.data[..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            event.payload(),
            Some((EVENT_PCM, &[1, 2, 3, 4, 5, 6, 7, 8][..]))
        );

        event.size = (MAX_EVENT_DATA + 1) as u32;
        assert!(event.payload().is_none());
        event.size = 1;
        event.event_type = 99;
        assert!(event.payload().is_none());
    }

    #[test]
    fn uapi_output_and_pcm_events_reach_the_slot_owned_feedback_queue() {
        let queue = crate::feedback::FeedbackQueue::default();
        let generation = queue.begin_lease();
        let producer = queue.producer(generation, 5, 0x8000);
        let mut packetizer = Ds5HapticsPacketizer::default();
        let now = Instant::now();

        let mut output = NativeEvent {
            event_type: EVENT_OUTPUT,
            size: u32::try_from(super::super::DS5_OUTPUT_REPORT_SIZE).unwrap(),
            ..NativeEvent::default()
        };
        output.data[0] = super::super::DS5_OUTPUT_REPORT_ID;
        output.data[1] = 1;
        output.data[3] = 64;
        output.data[4] = 128;
        dispatch_native_event(&output, &mut packetizer, &producer, now);
        assert_eq!(
            queue.peek(1)[0].feedback,
            HostFeedback::Rumble {
                controller: 5,
                low_frequency: 128 * 257,
                high_frequency: 64 * 257,
            }
        );
        assert!(queue.acknowledge(1));

        let source_frame = [1, 2, 3, 4, 0x81, 0x82, 0x83, 0x84];
        let source = source_frame.repeat(60);
        for _ in 0..4 {
            let mut pcm = NativeEvent {
                event_type: EVENT_PCM,
                size: u32::try_from(source.len()).unwrap(),
                ..NativeEvent::default()
            };
            pcm.data[..source.len()].copy_from_slice(&source);
            dispatch_native_event(&pcm, &mut packetizer, &producer, now);
        }

        let events = queue.peek(1);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0].feedback,
            HostFeedback::HapticsPcm {
                controller: 5,
                sequence: 0,
                frames: 240,
                samples_s16le,
            } if samples_s16le == &[0x81, 0x82, 0x83, 0x84].repeat(240)
        ));
    }
}
