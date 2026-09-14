#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Capture, enumerate};
#[cfg(not(windows))]
mod unsupported {
    use crate::model::{AudioResult, Endpoint, Format, Packet};
    pub fn enumerate() -> AudioResult<Vec<Endpoint>> {
        Ok(Vec::new())
    }

    pub struct Capture;
    impl Capture {
        pub fn open(_: &str) -> AudioResult<Self> {
            Err("unsupported_platform")
        }
        pub fn format(&self) -> Format {
            unreachable!("unsupported capture never opens")
        }
        pub fn endpoint(&self) -> &str {
            unreachable!("unsupported capture never opens")
        }
        pub fn read(&mut self) -> AudioResult<Option<Packet>> {
            Err("unsupported_platform")
        }
    }
}

#[cfg(not(windows))]
pub use unsupported::{Capture, enumerate};
