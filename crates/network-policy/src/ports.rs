pub const DEFAULT_BASE_PORT: u16 = 47_989;
pub const MIN_BASE_PORT: u16 = 1_029;
pub const MAX_BASE_PORT: u16 = 65_514;
/// HTTPS is below the HTTP base port; the remaining offsets are above it.
pub const HTTPS_OFFSET: u16 = 5;
pub const RTSP_OFFSET: u16 = 21;
pub const VIDEO_OFFSET: u16 = 9;
pub const CONTROL_OFFSET: u16 = 10;
pub const AUDIO_OFFSET: u16 = 11;

/// Validated once so every GameStream service derives its port without overflow
/// or entering the privileged port range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameStreamPorts {
    base: u16,
}

impl GameStreamPorts {
    pub fn new(base: u16) -> Result<Self, &'static str> {
        if !(MIN_BASE_PORT..=MAX_BASE_PORT).contains(&base) {
            return Err("invalid_port");
        }
        Ok(Self { base })
    }

    pub const fn base(self) -> u16 {
        self.base
    }

    pub const fn http(self) -> u16 {
        self.base
    }

    pub const fn https(self) -> u16 {
        self.base - HTTPS_OFFSET
    }

    pub const fn rtsp(self) -> u16 {
        self.base + RTSP_OFFSET
    }

    pub const fn video(self) -> u16 {
        self.base + VIDEO_OFFSET
    }

    pub const fn control(self) -> u16 {
        self.base + CONTROL_OFFSET
    }

    pub const fn audio(self) -> u16 {
        self.base + AUDIO_OFFSET
    }
}

#[cfg(test)]
mod tests;
