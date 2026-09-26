use crate::model::InputResult;

/// Publish each created device to its owning backend before acquiring the next.
/// If pointer creation fails, startup cleanup can still retry keyboard destroy.
pub(super) fn acquire_input_pair<Device>(
    keyboard: &mut Option<Device>,
    pointer: &mut Option<Device>,
    mut create: impl FnMut(bool) -> InputResult<Device>,
) -> InputResult<()> {
    *keyboard = Some(create(true)?);
    *pointer = Some(create(false)?);
    Ok(())
}

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
    fn partial_startup_retains_keyboard_through_failed_cleanup_and_reacquires() {
        struct FakeInputDevice {
            id: u8,
            updated: bool,
        }

        let mut keyboard = None;
        let mut pointer = None;
        let mut next_id = 0;
        let mut fail_pointer_once = true;
        let mut create = |keyboard: bool| {
            if !keyboard && std::mem::take(&mut fail_pointer_once) {
                return Err(InputError::new("acquire_failed", "pointer creation failed"));
            }
            next_id += 1;
            Ok(FakeInputDevice {
                id: next_id,
                updated: false,
            })
        };

        assert_eq!(
            acquire_input_pair(&mut keyboard, &mut pointer, &mut create)
                .unwrap_err()
                .code,
            "acquire_failed"
        );
        keyboard.as_mut().unwrap().updated = true;
        assert_eq!(keyboard.as_ref().map(|device| device.id), Some(1));
        assert!(pointer.is_none());

        let mut fail_destroy_once = true;
        let mut destroy = |device: &mut FakeInputDevice| {
            assert!(device.updated);
            if std::mem::take(&mut fail_destroy_once) {
                Err(InputError::new("release_failed", "keyboard destroy failed"))
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
        release_input_pair(&mut keyboard, &mut pointer, &mut destroy).unwrap();
        assert!(keyboard.is_none());

        acquire_input_pair(&mut keyboard, &mut pointer, &mut create).unwrap();
        keyboard.as_mut().unwrap().updated = true;
        pointer.as_mut().unwrap().updated = true;
        assert_eq!(keyboard.as_ref().map(|device| device.id), Some(2));
        assert_eq!(pointer.as_ref().map(|device| device.id), Some(3));
        release_input_pair(&mut keyboard, &mut pointer, &mut destroy).unwrap();
        assert!(keyboard.is_none() && pointer.is_none());
    }

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
