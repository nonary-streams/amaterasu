use crate::Snapshot;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

pub fn observe() -> Option<Snapshot> {
    #[cfg(target_os = "linux")]
    {
        linux::observe()
    }
    #[cfg(windows)]
    {
        windows::observe()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        None
    }
}
