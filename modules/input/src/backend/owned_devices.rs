use crate::model::InputResult;

/// Remove both native input devices, retaining each descriptor whose destroy
/// operation fails so the owning lease can retry without reopening it.
pub(super) fn release_input_pair<Device>(
    keyboard: &mut Option<Device>,
    pointer: &mut Option<Device>,
    mut destroy: impl FnMut(&mut Device) -> InputResult<()>,
) -> InputResult<()> {
    fn release<Device>(
        slot: &mut Option<Device>,
        destroy: &mut impl FnMut(&mut Device) -> InputResult<()>,
    ) -> InputResult<()> {
        if let Some(device) = slot.as_mut() {
            destroy(device)?;
            *slot = None;
        }
        Ok(())
    }

    let keyboard_result = release(keyboard, &mut destroy);
    let pointer_result = release(pointer, &mut destroy);
    keyboard_result?;
    pointer_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::InputError;

    #[test]
    fn input_pair_stop_retries_failed_destroy_without_reopening_owned_device() {
        struct FakeInputDevice {
            id: u8,
            updated: bool,
        }

        let allocations = std::cell::Cell::new(0);
        let acquire = |id| {
            allocations.set(allocations.get() + 1);
            FakeInputDevice { id, updated: false }
        };
        let mut keyboard = Some(acquire(1));
        let mut pointer = Some(acquire(2));
        keyboard.as_mut().unwrap().updated = true;
        pointer.as_mut().unwrap().updated = true;
        assert_eq!(allocations.get(), 2);

        let mut attempts = Vec::new();
        let mut fail_keyboard_once = true;
        {
            let mut destroy = |device: &mut FakeInputDevice| {
                assert!(device.updated);
                attempts.push(device.id);
                if device.id == 1 && std::mem::take(&mut fail_keyboard_once) {
                    Err(InputError::new("release_failed", "UI_DEV_DESTROY failed"))
                } else {
                    Ok(())
                }
            };
            assert_eq!(
                release_input_pair(&mut keyboard, &mut pointer, &mut destroy)
                    .unwrap_err()
                    .code,
                "release_failed"
            );
            assert_eq!(keyboard.as_ref().map(|device| device.id), Some(1));
            assert!(pointer.is_none());
            assert_eq!(allocations.get(), 2);

            release_input_pair(&mut keyboard, &mut pointer, &mut destroy).unwrap();
        }
        assert!(keyboard.is_none());
        assert_eq!(attempts, [1, 2, 1]);
        keyboard = Some(acquire(3));
        assert_eq!(keyboard.as_ref().map(|device| device.id), Some(3));
        assert_eq!(allocations.get(), 3);
    }
}
