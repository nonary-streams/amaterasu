//! Modern GameStream negotiation bound to an already authorized launch epoch.
#[cfg(test)]
mod tests;
mod validation;

use super::{Method, Request, serialize_response};
use crate::sdp;

pub struct TrustedLaunch {
    pub epoch: u64,
    pub rtsp_target: String,
    pub session_token: String,
    pub encrypted_rtsp: bool,
    pub audio_port: u16,
    pub video_port: u16,
    pub control_port: u16,
    pub control_connect_data: u32,
    /// Ticket-scoped bearer tokens for selecting an authorized peer endpoint per media lane.
    pub udp_discovery: Option<UdpDiscovery>,
}

#[derive(Clone, Eq, PartialEq)]
pub struct UdpDiscovery {
    pub audio_ping_payload: String,
    pub video_ping_payload: String,
}

pub struct Capabilities {
    pub transport: sdp::TransportSupport,
    /// Color modes implemented by the selected capture-to-encoder path.
    pub supported_colors: Vec<sdp::ColorMode>,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    pub max_bitrate_kbps: u32,
    pub require_video_encryption: bool,
    pub require_audio_encryption: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Backend must compare this against its actual prepared wire/capture config.
    Announce(sdp::Announce),
    Play,
    Teardown,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Reply(Vec<u8>),
    Action { ticket: u64, action: Action },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidLaunch,
    WrongEpoch,
    WrongSession,
    InvalidSequence,
    InvalidTarget,
    InvalidHeaders,
    InvalidBody,
    UnsupportedTransport,
    UnsupportedMedia,
    WrongState,
    Pending,
    WrongTicket,
    Serialization,
}

impl Error {
    pub fn status(self) -> u16 {
        match self {
            Self::WrongEpoch => 403,
            Self::WrongSession => 454,
            Self::InvalidTarget => 404,
            Self::UnsupportedTransport => 461,
            Self::UnsupportedMedia => 406,
            Self::WrongState | Self::Pending => 455,
            Self::InvalidLaunch | Self::WrongTicket | Self::Serialization => 500,
            _ => 400,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Options,
    Describe,
    SetupAudio,
    SetupVideo,
    SetupControl,
    Announce,
    Play,
    Playing,
    Failed,
    Closed,
}

struct Pending {
    ticket: u64,
    cseq: u32,
    success: State,
}

pub struct Negotiation {
    launch: TrustedLaunch,
    capabilities: Capabilities,
    state: State,
    last_cseq: Option<u32>,
    pending: Option<Pending>,
}

impl Negotiation {
    pub fn new(launch: TrustedLaunch, capabilities: Capabilities) -> Result<Self, Error> {
        validation::launch(&launch, &capabilities)?;
        Ok(Self {
            launch,
            capabilities,
            state: State::Options,
            last_cseq: None,
            pending: None,
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Requests must come from the transport authorized for this exact epoch.
    /// When encrypted_rtsp is true, transport must authenticate CR envelopes
    /// before parsing requests and seal every response under the HR domain.
    pub fn handle(&mut self, epoch: u64, request: &Request) -> Result<Step, Error> {
        if epoch != self.launch.epoch {
            return Err(Error::WrongEpoch);
        }
        if self.pending.is_some() {
            return Err(Error::Pending);
        }
        if self
            .last_cseq
            .is_some_and(|previous| request.cseq <= previous)
        {
            return Err(Error::InvalidSequence);
        }
        validation::request(request)?;
        self.validate_session(request)?;
        let (step, next) = self.transition(request)?;
        self.last_cseq = Some(request.cseq);
        if let Step::Action { ticket, .. } = &step {
            self.pending = Some(Pending {
                ticket: *ticket,
                cseq: request.cseq,
                success: next,
            });
        } else {
            self.state = next;
        }
        Ok(step)
    }

    /// Only acknowledge success after the backend has committed its operation.
    /// Failure is terminal except that TEARDOWN remains available for cleanup.
    pub fn complete(&mut self, epoch: u64, ticket: u64, success: bool) -> Result<Vec<u8>, Error> {
        if epoch != self.launch.epoch {
            return Err(Error::WrongEpoch);
        }
        let pending = self.pending.as_ref().ok_or(Error::WrongTicket)?;
        if pending.ticket != ticket {
            return Err(Error::WrongTicket);
        }
        let response = self.response(if success { 200 } else { 503 }, pending.cseq, &[], &[])?;
        self.state = if success {
            pending.success
        } else {
            State::Failed
        };
        self.pending = None;
        Ok(response)
    }

    fn validate_session(&self, request: &Request) -> Result<(), Error> {
        let established = !matches!(
            self.state,
            State::Options | State::Describe | State::SetupAudio
        );
        match (established, request.header("session")) {
            (false, None) => Ok(()),
            (true, Some(token)) if token == self.launch.session_token => Ok(()),
            _ => Err(Error::WrongSession),
        }
    }

    fn transition(&self, request: &Request) -> Result<(Step, State), Error> {
        if request.method == Method::Teardown {
            if matches!(
                self.state,
                State::Options | State::Describe | State::SetupAudio | State::Closed
            ) {
                return Err(Error::WrongState);
            }
            validation::target(request, "/")?;
            return Ok((action(request, Action::Teardown), State::Closed));
        }
        match (self.state, request.method) {
            (State::Options, Method::Options) => {
                validation::target(request, &self.launch.rtsp_target)?;
                let reply = self.response(
                    200,
                    request.cseq,
                    &[(
                        "Public",
                        "OPTIONS, DESCRIBE, SETUP, ANNOUNCE, PLAY, TEARDOWN",
                    )],
                    &[],
                )?;
                Ok((Step::Reply(reply), State::Describe))
            }
            (State::Describe, Method::Describe) => {
                validation::target(request, &self.launch.rtsp_target)?;
                if request.header("accept") != Some("application/sdp") {
                    return Err(Error::InvalidHeaders);
                }
                let body = validation::description(&self.capabilities);
                let reply = self.response(
                    200,
                    request.cseq,
                    &[("Content-Type", "application/sdp")],
                    body.as_bytes(),
                )?;
                Ok((Step::Reply(reply), State::SetupAudio))
            }
            (State::SetupAudio, Method::Setup) => self.setup(
                request,
                "streamid=audio/0/0",
                self.launch.audio_port,
                self.launch
                    .udp_discovery
                    .as_ref()
                    .map(|discovery| discovery.audio_ping_payload.as_str()),
                State::SetupVideo,
            ),
            (State::SetupVideo, Method::Setup) => self.setup(
                request,
                "streamid=video/0/0",
                self.launch.video_port,
                self.launch
                    .udp_discovery
                    .as_ref()
                    .map(|discovery| discovery.video_ping_payload.as_str()),
                State::SetupControl,
            ),
            (State::SetupControl, Method::Setup) => self.setup(
                request,
                "streamid=control/13/0",
                self.launch.control_port,
                None,
                State::Announce,
            ),
            (State::Announce, Method::Announce) => {
                validation::target(request, "streamid=control/13/0")?;
                if request.header("content-type") != Some("application/sdp") {
                    return Err(Error::InvalidHeaders);
                }
                let announce = sdp::parse_announce(&request.body).map_err(|error| {
                    eprintln!("amaterasu rtsp announce parse rejection reason={error:?}");
                    Error::UnsupportedMedia
                })?;
                validation::media(&announce, &self.capabilities)?;
                Ok((action(request, Action::Announce(announce)), State::Play))
            }
            (State::Play, Method::Play) => {
                validation::target(request, "/")?;
                Ok((action(request, Action::Play), State::Playing))
            }
            _ => Err(Error::WrongState),
        }
    }

    fn setup(
        &self,
        request: &Request,
        target: &str,
        port: u16,
        ping_payload: Option<&str>,
        next: State,
    ) -> Result<(Step, State), Error> {
        validation::target(request, target)?;
        validation::transport(request.header("transport"))?;
        let transport = format!("unicast;server_port={port}");
        let connect = self.launch.control_connect_data.to_string();
        let mut headers = vec![("Transport", transport.as_str())];
        if let Some(ping_payload) = ping_payload {
            headers.push(("X-SS-Ping-Payload", ping_payload));
        }
        if next == State::Announce {
            headers.push(("X-SS-Connect-Data", connect.as_str()));
        }
        Ok((
            Step::Reply(self.response(200, request.cseq, &headers, &[])?),
            next,
        ))
    }

    fn response(
        &self,
        status: u16,
        cseq: u32,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Vec<u8>, Error> {
        let mut headers = headers.to_vec();
        headers.push(("Session", &self.launch.session_token));
        serialize_response(status, cseq, &headers, body).map_err(|_| Error::Serialization)
    }
}

fn action(request: &Request, action: Action) -> Step {
    Step::Action {
        ticket: u64::from(request.cseq),
        action,
    }
}
