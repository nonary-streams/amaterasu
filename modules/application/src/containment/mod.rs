//! Platform ownership keeps OS identifiers live until the final tree signal.
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::ContainedChild;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::ContainedChild;
