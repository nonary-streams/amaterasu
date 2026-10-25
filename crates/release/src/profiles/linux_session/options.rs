//! Operator options for the Linux session product profile.
//!
//! Every effect-bearing capability is off unless an option names it, and every
//! listener binds loopback unless the operator supplies a validated LAN option.
//! This module only validates and derives settings. It never opens a socket,
//! touches a device or changes a firewall.
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Moonlight's conventional base port, used when no LAN option is supplied.
pub const DEFAULT_BASE_PORT: u16 = 47_989;
/// Lowest base port NVHTTP and RTSP accept; HTTPS sits five ports below it.
pub const MIN_BASE_PORT: u16 = 1_029;
/// Highest base port NVHTTP and RTSP accept; RTSP sits twenty-one ports above it.
pub const MAX_BASE_PORT: u16 = 65_514;
const HTTPS_PORT_OFFSET: u16 = 5;
const RTSP_PORT_OFFSET: u16 = 21;
const MAX_AUDIO_ENDPOINT_BYTES: usize = 4096;

/// The default audio endpoint selector passed through to the audio module.
pub const DEFAULT_AUDIO_ENDPOINT: &str = "default";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OptionError {
    UnknownOption(String),
    MissingValue(&'static str),
    RepeatedOption(&'static str),
    InvalidLanValue(String),
    PortOutOfRange(u16),
    UnusableBindAddress(IpAddr),
    WildcardRequiresAllowRemote,
    AllowRemoteWithoutWildcard,
    InvalidManagementPort(String),
    InvalidCatalogOwner(String),
    InvalidFrameHelper(String),
    InvalidLabel(&'static str),
}

impl fmt::Display for OptionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOption(option) => write!(formatter, "unknown option {option}"),
            Self::MissingValue(option) => write!(formatter, "{option} requires a value"),
            Self::RepeatedOption(option) => write!(formatter, "{option} was given twice"),
            Self::InvalidLanValue(value) => write!(
                formatter,
                "--lan expects ADDRESS:PORT such as 192.168.1.20:47989 or [fd00::20]:47989, got {value:?}"
            ),
            Self::PortOutOfRange(port) => write!(
                formatter,
                "base port {port} is outside {MIN_BASE_PORT}..={MAX_BASE_PORT}; \
                 HTTPS uses the base minus {HTTPS_PORT_OFFSET} and RTSP the base plus {RTSP_PORT_OFFSET}"
            ),
            Self::UnusableBindAddress(address) => write!(
                formatter,
                "{address} cannot be a listener address (multicast, broadcast or IPv4-mapped)"
            ),
            Self::WildcardRequiresAllowRemote => formatter.write_str(
                "a wildcard --lan address listens on every interface; add --allow-remote to request that explicitly, \
                 or name the concrete interface address instead",
            ),
            Self::AllowRemoteWithoutWildcard => formatter.write_str(
                "--allow-remote only applies to a wildcard --lan address such as 0.0.0.0:47989; \
                 a concrete address already names the interface",
            ),
            Self::InvalidManagementPort(value) => write!(
                formatter,
                "--management-port expects a loopback TCP port from 1024 through 65535, got {value:?}"
            ),
            Self::InvalidFrameHelper(value) => write!(
                formatter,
                "--frame-helper expects an absolute path to amaterasu-vkms-frame-helper, got {value:?}"
            ),
            Self::InvalidCatalogOwner(value) => write!(
                formatter,
                "--catalog-owner expects management or console, got {value:?}"
            ),
            Self::InvalidLabel(option) => write!(
                formatter,
                "{option} must be a non-empty label without control characters or surrounding whitespace"
            ),
        }
    }
}

impl std::error::Error for OptionError {}

/// Address families the NVHTTP and RTSP listener settings understand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressFamily {
    Ipv4,
    /// IPv4 and IPv6 listeners together.
    Both,
}

impl AddressFamily {
    pub fn setting(self) -> &'static str {
        match self {
            Self::Ipv4 => "ipv4",
            Self::Both => "both",
        }
    }
}

/// Where the Moonlight listeners bind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListenerScope {
    /// One concrete loopback address. Remote binding stays disabled.
    Loopback(IpAddr),
    /// One concrete, non-loopback interface address named by the operator.
    Interface(IpAddr),
    /// Every interface of the family, requested with `--allow-remote`.
    AllInterfaces(AddressFamily),
}

/// A validated base port and listener scope shared by NVHTTP, RTSP, and the
/// media and control transports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkOption {
    base_port: u16,
    scope: ListenerScope,
}

impl Default for NetworkOption {
    fn default() -> Self {
        Self {
            base_port: DEFAULT_BASE_PORT,
            scope: ListenerScope::Loopback(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        }
    }
}

impl NetworkOption {
    /// Parses `--lan ADDRESS:PORT` together with whether `--allow-remote` was given.
    pub fn parse(lan: &str, allow_remote: bool) -> Result<Self, OptionError> {
        let socket: SocketAddr = lan
            .parse()
            .map_err(|_| OptionError::InvalidLanValue(lan.to_owned()))?;
        Self::new(socket.ip(), socket.port(), allow_remote)
    }

    pub fn new(bind: IpAddr, base_port: u16, allow_remote: bool) -> Result<Self, OptionError> {
        if !(MIN_BASE_PORT..=MAX_BASE_PORT).contains(&base_port) {
            return Err(OptionError::PortOutOfRange(base_port));
        }
        if unusable_listener(bind) {
            return Err(OptionError::UnusableBindAddress(bind));
        }
        let scope = if bind.is_unspecified() {
            if !allow_remote {
                return Err(OptionError::WildcardRequiresAllowRemote);
            }
            ListenerScope::AllInterfaces(if bind.is_ipv4() {
                AddressFamily::Ipv4
            } else {
                AddressFamily::Both
            })
        } else if allow_remote {
            return Err(OptionError::AllowRemoteWithoutWildcard);
        } else if bind.is_loopback() {
            ListenerScope::Loopback(bind)
        } else {
            ListenerScope::Interface(bind)
        };
        Ok(Self { base_port, scope })
    }

    #[cfg(test)]
    pub fn scope(&self) -> ListenerScope {
        self.scope
    }

    pub fn http_port(&self) -> u16 {
        self.base_port
    }

    pub fn https_port(&self) -> u16 {
        self.base_port - HTTPS_PORT_OFFSET
    }

    pub fn rtsp_port(&self) -> u16 {
        self.base_port + RTSP_PORT_OFFSET
    }

    /// NVHTTP, RTSP, media, control and session settings share this flag.
    pub fn allow_remote(&self) -> bool {
        !matches!(self.scope, ListenerScope::Loopback(_))
    }

    pub fn address_family(&self) -> AddressFamily {
        match self.scope {
            ListenerScope::Loopback(address) | ListenerScope::Interface(address) => {
                if address.is_ipv4() {
                    AddressFamily::Ipv4
                } else {
                    AddressFamily::Both
                }
            }
            ListenerScope::AllInterfaces(family) => family,
        }
    }

    /// The `bind_address` setting: blank selects the family's wildcard addresses.
    pub fn bind_address(&self) -> String {
        match self.scope {
            ListenerScope::Loopback(address) | ListenerScope::Interface(address) => {
                address.to_string()
            }
            ListenerScope::AllInterfaces(_) => String::new(),
        }
    }

    /// The address used for the legacy socket-style bind settings.
    pub fn legacy_ip(&self) -> IpAddr {
        match self.scope {
            ListenerScope::Loopback(address) | ListenerScope::Interface(address) => address,
            ListenerScope::AllInterfaces(AddressFamily::Ipv4) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            ListenerScope::AllInterfaces(AddressFamily::Both) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        }
    }

    pub fn http_bind(&self) -> SocketAddr {
        SocketAddr::new(self.legacy_ip(), self.http_port())
    }

    pub fn https_bind(&self) -> SocketAddr {
        SocketAddr::new(self.legacy_ip(), self.https_port())
    }

    pub fn rtsp_bind(&self) -> SocketAddr {
        SocketAddr::new(self.legacy_ip(), self.rtsp_port())
    }
}

/// Mirrors the multicast, broadcast and IPv4-mapped rejections in NVHTTP and RTSP.
fn unusable_listener(address: IpAddr) -> bool {
    address.is_multicast()
        || address == IpAddr::V4(Ipv4Addr::BROADCAST)
        || matches!(address, IpAddr::V6(address) if address.to_ipv4_mapped().is_some())
}

/// The catalog namespace Moonlight's application list reads.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CatalogOwner {
    /// Entries authored through the management API and web interface.
    #[default]
    Management,
    /// Entries authored through the development console on standard input.
    Console,
}

impl CatalogOwner {
    pub fn principal(self) -> &'static str {
        match self {
            Self::Management => "amaterasu.management",
            Self::Console => "amaterasu.console",
        }
    }
}

/// Everything an operator can change when initialising a Linux session root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinuxSessionOptions {
    pub network: NetworkOption,
    pub catalog_owner: CatalogOwner,
    /// Enables the loopback management listener on this port.
    pub management_port: Option<u16>,
    pub audio_endpoint: String,
    /// Enables video streams after portal source selection.
    pub enable_capture: bool,
    /// Enables host audio capture.
    pub enable_audio: bool,
    /// Enables the owned virtual display provider.
    pub enable_virtual_display: bool,
    /// Allows stream sessions to create virtual input devices.
    pub enable_input: bool,
    /// Allows the display module to change the physical display layout.
    pub enable_display_mutation: bool,
    /// Absolute path of the root-owned `amaterasu-vkms-frame-helper` that the
    /// operator granted `cap_sys_admin+p`. Enables direct scanout capture.
    pub frame_helper: Option<String>,
}

impl Default for LinuxSessionOptions {
    fn default() -> Self {
        Self {
            network: NetworkOption::default(),
            catalog_owner: CatalogOwner::default(),
            management_port: None,
            audio_endpoint: DEFAULT_AUDIO_ENDPOINT.into(),
            enable_capture: false,
            enable_audio: false,
            enable_virtual_display: false,
            enable_input: false,
            enable_display_mutation: false,
            frame_helper: None,
        }
    }
}

impl LinuxSessionOptions {
    /// Parses the arguments that follow `init-linux-session ROOT`.
    pub fn from_arguments(arguments: &[String]) -> Result<Self, OptionError> {
        let mut options = Self::default();
        let mut lan: Option<String> = None;
        let mut allow_remote = false;
        let mut seen = SeenOptions::default();
        let mut arguments = arguments.iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--lan" => {
                    seen.once("--lan")?;
                    lan = Some(required_value(&mut arguments, "--lan")?);
                }
                "--allow-remote" => {
                    seen.once("--allow-remote")?;
                    allow_remote = true;
                }
                "--catalog-owner" => {
                    seen.once("--catalog-owner")?;
                    options.catalog_owner =
                        parse_catalog_owner(&required_value(&mut arguments, "--catalog-owner")?)?;
                }
                "--management-port" => {
                    seen.once("--management-port")?;
                    options.management_port = Some(parse_management_port(&required_value(
                        &mut arguments,
                        "--management-port",
                    )?)?);
                }
                "--audio-endpoint" => {
                    seen.once("--audio-endpoint")?;
                    options.audio_endpoint = label(
                        required_value(&mut arguments, "--audio-endpoint")?,
                        "--audio-endpoint",
                        MAX_AUDIO_ENDPOINT_BYTES,
                    )?;
                }
                "--enable-capture" => {
                    seen.once("--enable-capture")?;
                    options.enable_capture = true;
                }
                "--enable-audio" => {
                    seen.once("--enable-audio")?;
                    options.enable_audio = true;
                }
                "--frame-helper" => {
                    seen.once("--frame-helper")?;
                    options.frame_helper = Some(parse_frame_helper(&required_value(
                        &mut arguments,
                        "--frame-helper",
                    )?)?);
                }
                "--enable-virtual-display" => {
                    seen.once("--enable-virtual-display")?;
                    options.enable_virtual_display = true;
                }
                "--enable-input" => {
                    seen.once("--enable-input")?;
                    options.enable_input = true;
                }
                "--enable-display-mutation" => {
                    seen.once("--enable-display-mutation")?;
                    options.enable_display_mutation = true;
                }
                other => return Err(OptionError::UnknownOption(other.to_owned())),
            }
        }
        match lan {
            Some(lan) => options.network = NetworkOption::parse(&lan, allow_remote)?,
            None if allow_remote => return Err(OptionError::AllowRemoteWithoutWildcard),
            None => {}
        }
        Ok(options)
    }
}

#[derive(Default)]
struct SeenOptions(Vec<&'static str>);

impl SeenOptions {
    fn once(&mut self, option: &'static str) -> Result<(), OptionError> {
        if self.0.contains(&option) {
            return Err(OptionError::RepeatedOption(option));
        }
        self.0.push(option);
        Ok(())
    }
}

fn required_value<'a>(
    arguments: &mut impl Iterator<Item = &'a String>,
    option: &'static str,
) -> Result<String, OptionError> {
    arguments
        .next()
        .cloned()
        .ok_or(OptionError::MissingValue(option))
}

fn parse_frame_helper(value: &str) -> Result<String, OptionError> {
    let path = std::path::Path::new(value);
    let normal = path.components().all(|component| {
        matches!(
            component,
            std::path::Component::RootDir | std::path::Component::Normal(_)
        )
    });
    if !path.is_absolute() || !normal || value.ends_with('/') {
        return Err(OptionError::InvalidFrameHelper(value.into()));
    }
    Ok(value.into())
}

fn parse_catalog_owner(value: &str) -> Result<CatalogOwner, OptionError> {
    match value {
        "management" => Ok(CatalogOwner::Management),
        "console" => Ok(CatalogOwner::Console),
        other => Err(OptionError::InvalidCatalogOwner(other.to_owned())),
    }
}

fn parse_management_port(value: &str) -> Result<u16, OptionError> {
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port >= 1024)
        .ok_or_else(|| OptionError::InvalidManagementPort(value.to_owned()))
}

fn label(value: String, option: &'static str, maximum: usize) -> Result<String, OptionError> {
    if value.is_empty()
        || value.len() > maximum
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(OptionError::InvalidLabel(option));
    }
    Ok(value)
}
