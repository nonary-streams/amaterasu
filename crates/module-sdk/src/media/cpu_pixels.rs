use std::{ops::Deref, sync::Arc};
/// Shared immutable CPU pixels. The owner may be a capture allocation or a
/// complete provider frame; wrapping or cloning it never copies pixel bytes.
#[derive(Clone)]
pub struct CpuPixels {
    owner: Arc<dyn AsRef<[u8]> + Send + Sync>,
}
impl CpuPixels {
    pub fn from_owner<T: AsRef<[u8]> + Send + Sync + 'static>(owner: Arc<T>) -> Self {
        Self { owner }
    }
}
impl From<Arc<Vec<u8>>> for CpuPixels {
    fn from(owner: Arc<Vec<u8>>) -> Self {
        Self::from_owner(owner)
    }
}
impl Deref for CpuPixels {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.owner.as_ref().as_ref()
    }
}
impl AsRef<[u8]> for CpuPixels {
    fn as_ref(&self) -> &[u8] {
        self
    }
}
