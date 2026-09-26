/// A pending readback cannot be submitted again until its pixels are consumed.
/// At most one completed resource remains idle for the next frame.
pub struct StagingLifecycle<T, P> {
    pending: Option<P>,
    idle: Option<(StagingKey, T)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StagingKey {
    pub width: u32,
    pub height: u32,
    pub format: u32,
}

impl<T, P> Default for StagingLifecycle<T, P> {
    fn default() -> Self {
        Self {
            pending: None,
            idle: None,
        }
    }
}

impl<T, P> StagingLifecycle<T, P> {
    pub fn pending(&self) -> Option<&P> {
        self.pending.as_ref()
    }

    pub fn submit(&mut self, pending: P) {
        assert!(
            self.pending.is_none(),
            "pending readback must be consumed first"
        );
        self.pending = Some(pending);
    }

    pub fn take_pending(&mut self) -> Option<P> {
        self.pending.take()
    }

    pub fn take_idle(&mut self, key: StagingKey) -> Option<T> {
        assert!(
            self.pending.is_none(),
            "pending readback must be consumed first"
        );
        match self.idle.take() {
            Some((cached_key, texture)) if cached_key == key => Some(texture),
            _ => None,
        }
    }

    pub fn store_idle(&mut self, key: StagingKey, texture: T) {
        assert!(
            self.pending.is_none(),
            "pending readback must be consumed first"
        );
        self.idle = Some((key, texture));
    }

    pub fn invalidate(&mut self) {
        self.pending = None;
        self.idle = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct FakeTexture {
        id: usize,
        pixels: Vec<u8>,
        drops: Arc<AtomicUsize>,
    }

    impl Drop for FakeTexture {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn compatible_frames_reuse_only_after_readback_finishes() {
        let key = StagingKey {
            width: 2,
            height: 1,
            format: 87,
        };
        let mut slot = StagingLifecycle::default();
        let drops = Arc::new(AtomicUsize::new(0));
        let pending = FakeTexture {
            id: 7,
            pixels: vec![1, 2],
            drops: drops.clone(),
        };
        slot.submit(pending);
        assert_eq!(slot.pending().unwrap().pixels, [1, 2]);
        // A busy Map leaves ownership with the pending readback. Nothing is
        // available for a second CopyResource until those pixels are read.
        assert!(slot.pending().is_some());
        let mut pending = slot.take_pending().unwrap();
        pending.pixels.copy_from_slice(&[3, 4]);
        let delivered = pending.pixels.clone();
        slot.store_idle(key, pending);
        let mut reused = slot.take_idle(key).expect("completed readback is reusable");
        assert_eq!(reused.id, 7);
        assert_eq!(delivered, [3, 4]);
        reused.pixels.copy_from_slice(&[5, 6]);
        assert_eq!(reused.pixels, [5, 6]);
        assert!(slot.take_idle(key).is_none());
        drop(reused);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn mode_change_and_failure_discard_cached_resource() {
        let key = StagingKey {
            width: 2,
            height: 1,
            format: 87,
        };
        let changed = StagingKey { width: 3, ..key };
        let changed_format = StagingKey { format: 28, ..key };
        let drops = Arc::new(AtomicUsize::new(0));
        let make_texture = |id| FakeTexture {
            id,
            pixels: vec![id as u8; 8],
            drops: drops.clone(),
        };
        let mut slot = StagingLifecycle::<FakeTexture, FakeTexture>::default();
        slot.store_idle(key, make_texture(1));
        assert!(slot.take_idle(changed).is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        slot.store_idle(changed, make_texture(2));
        assert!(slot.take_idle(changed_format).is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        slot.store_idle(changed, make_texture(3));
        slot.submit(make_texture(4));
        // Device loss or terminal readback errors clear the resource.
        slot.invalidate();
        assert_eq!(drops.load(Ordering::SeqCst), 4);
        assert!(slot.pending().is_none());
        assert!(slot.take_idle(changed).is_none());
    }

    #[test]
    #[should_panic(expected = "pending readback must be consumed first")]
    fn pending_readback_cannot_be_replaced() {
        let mut slot = StagingLifecycle::<(), ()>::default();
        slot.submit(());
        slot.submit(());
    }
}
