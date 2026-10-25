//! Fakes for ownership and state tests. They do not qualify real capture.
use crate::{
    backend::Backend,
    discovery::{
        Connection, ConnectorState, CrtcState, DiscoveredOutput, Discovery, DisplayMode,
        OutputBinding,
    },
    frames::FrameSource,
    model::{CaptureError, CaptureResult, Settings},
    output_name::OutputName,
};
use amaterasu_vkms_frame_helper::{
    client::{ClientError, ExportedFrame, FrameExport},
    drm_node::DeviceNumber,
    protocol::{Failure, FrameDescriptor, MAX_PLANES},
};
use std::{
    collections::VecDeque,
    fs::File,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug)]
pub enum Scripted {
    Frame { sequence: u64, timestamp_ns: u64 },
    FencedFrame { sequence: u64 },
    Empty,
    Refuse(Failure),
    HelperGone,
}

/// Shared script and observations for every source a fake backend creates.
#[derive(Default)]
pub struct SourceScript {
    pub answers: Mutex<VecDeque<Scripted>>,
    /// Write ends of every lent descriptor; each reports when the read end
    /// (the descriptor the sink borrowed) has been closed.
    pub writers: Mutex<Vec<File>>,
    pub requested_crtcs: Mutex<Vec<u32>>,
}

impl SourceScript {
    pub fn push(&self, answer: Scripted) {
        self.answers.lock().unwrap().push_back(answer);
    }

    pub fn all_lent_descriptors_closed(&self) -> bool {
        self.writers.lock().unwrap().iter().all(reader_closed)
    }

    fn tracked_descriptor(&self) -> OwnedFd {
        let mut fds = [-1; 2];
        // SAFETY: `fds` is a writable two-element array as pipe2 requires.
        assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        // SAFETY: pipe2 succeeded and returned two new descriptors owned here.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
        self.writers.lock().unwrap().push(write);
        read
    }

    fn export(&self, sequence: u64, timestamp_ns: u64, fenced: bool) -> ExportedFrame {
        let mut pitches = [0; MAX_PLANES];
        pitches[0] = 7680;
        let mut fences: [Option<OwnedFd>; MAX_PLANES] = Default::default();
        if fenced {
            fences[0] = Some(self.tracked_descriptor());
        }
        ExportedFrame {
            descriptor: FrameDescriptor {
                crtc_id: 41,
                sequence,
                timestamp_ns,
                width: 1920,
                height: 1080,
                fourcc: u32::from_le_bytes(*b"XR24"),
                modifier: 0x0100_0000_0000_0004,
                plane_count: 1,
                pitches,
                offsets: [0; MAX_PLANES],
                fence_mask: u32::from(fenced),
            },
            dma_bufs: vec![self.tracked_descriptor()],
            fences,
        }
    }
}

fn reader_closed(writer: &File) -> bool {
    let mut poll = libc::pollfd {
        fd: writer.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: one initialized pollfd is passed with a zero timeout.
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 1);
    poll.revents & libc::POLLERR != 0
}

pub struct FakeSource(pub Arc<SourceScript>);

impl FrameSource for FakeSource {
    fn frame(&mut self, crtc_id: u32) -> Result<FrameExport, ClientError> {
        self.0.requested_crtcs.lock().unwrap().push(crtc_id);
        let answer = self
            .0
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Scripted::Empty);
        match answer {
            Scripted::Frame {
                sequence,
                timestamp_ns,
            } => Ok(FrameExport::Ready(self.0.export(
                sequence,
                timestamp_ns,
                false,
            ))),
            Scripted::FencedFrame { sequence } => {
                Ok(FrameExport::Ready(self.0.export(sequence, 1_000, true)))
            }
            Scripted::Empty => Ok(FrameExport::Empty),
            Scripted::Refuse(failure) => Err(ClientError::Refused {
                failure,
                os_error: 0,
            }),
            Scripted::HelperGone => Err(ClientError::Closed),
        }
    }
}

pub const CARD3: DeviceNumber = DeviceNumber {
    major: 226,
    minor: 3,
};

pub fn connector(index: u32, crtc_id: u32, width: u32) -> ConnectorState {
    ConnectorState {
        connector_id: 100 + index,
        connector_index: index,
        connection: Connection::Connected,
        crtc: Some(CrtcState {
            crtc_id,
            mode: Some(DisplayMode {
                width,
                height: 1080,
                refresh_hz: 60,
            }),
        }),
    }
}

/// A backend over a mutable list of connectors on card3.
pub struct FakeBackend {
    pub connectors: Arc<Mutex<Result<Vec<ConnectorState>, CaptureError>>>,
    pub helper: Arc<Mutex<CaptureResult<()>>>,
    pub script: Arc<SourceScript>,
    pub connects: Arc<Mutex<Vec<(String, u32)>>>,
}

impl FakeBackend {
    pub fn new() -> Self {
        Self {
            connectors: Arc::new(Mutex::new(Ok(vec![
                connector(9, 41, 1920),
                connector(10, 42, 1920),
            ]))),
            helper: Arc::new(Mutex::new(Ok(()))),
            script: Arc::new(SourceScript::default()),
            connects: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn handles(&self) -> Self {
        Self {
            connectors: self.connectors.clone(),
            helper: self.helper.clone(),
            script: self.script.clone(),
            connects: self.connects.clone(),
        }
    }
}

impl Backend for FakeBackend {
    fn discover(&self) -> CaptureResult<Discovery> {
        let connectors = self.connectors.lock().unwrap().clone()?;
        Ok(Discovery {
            outputs: connectors
                .into_iter()
                .map(|connector| {
                    (
                        DiscoveredOutput {
                            output: OutputName::new(connector.connector_index).unwrap(),
                            card: "card3".into(),
                            connector,
                        },
                        CARD3,
                    )
                })
                .collect(),
        })
    }

    fn resolve(&self, output: OutputName) -> CaptureResult<OutputBinding> {
        let probe = FakeProbe(self.connectors.lock().unwrap().clone());
        crate::discovery::resolve(&probe, output)
    }

    fn probe_helper(&self, _helper: &str) -> CaptureResult<()> {
        self.helper.lock().unwrap().clone()
    }

    fn connect(
        &self,
        helper: &str,
        binding: &OutputBinding,
    ) -> CaptureResult<Box<dyn FrameSource>> {
        self.helper.lock().unwrap().clone()?;
        self.connects
            .lock()
            .unwrap()
            .push((helper.to_owned(), binding.crtc_id));
        Ok(Box::new(FakeSource(self.script.clone())))
    }

    fn monotonic_now_ns(&self) -> u64 {
        1_000_000
    }
}

struct FakeProbe(Result<Vec<ConnectorState>, CaptureError>);

impl crate::discovery::DisplayProbe for FakeProbe {
    fn sysfs_cards(&self) -> Result<Vec<crate::discovery::SysfsCard>, String> {
        let connectors = self.0.clone().map_err(|error| error.message)?;
        Ok(vec![crate::discovery::SysfsCard {
            name: "card3".into(),
            device: CARD3,
            virtual_connectors: connectors.iter().map(|c| c.connector_index).collect(),
        }])
    }
    fn query_card(
        &self,
        _: &crate::discovery::SysfsCard,
    ) -> Result<Vec<ConnectorState>, crate::discovery::CardQueryError> {
        self.0
            .clone()
            .map_err(|error| crate::discovery::CardQueryError::Unavailable(error.message))
    }
}

pub const HELPER: &str = "/usr/libexec/amaterasu/amaterasu-vkms-frame-helper";

pub fn settings() -> Settings {
    Settings {
        enabled: true,
        capture_principals: ["amaterasu.video".into()].into(),
        frame_helper: Some(HELPER.into()),
    }
}
