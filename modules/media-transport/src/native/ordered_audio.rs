//! Owner-scoped admission into the selected ordered audio transport.
use super::{Shared, guarded};
use amaterasu_media::{
    AudioTransportSelectionV2, MEDIA_BACKEND_ERROR, MEDIA_BUSY, MEDIA_UNSUPPORTED,
    NegotiatedAudioPacket, OrderedAudioTransportApiV2,
};
use amaterasu_module_sdk::abi::*;
use std::{ffi::c_void, sync::TryLockError};

pub struct Factory {
    pub api: NativeServiceFactory,
    shared: Shared,
}
impl Factory {
    pub fn new(shared: Shared) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind),
            },
            shared,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}
struct Binding {
    api: OrderedAudioTransportApiV2,
    shared: Shared,
    owner: String,
}
unsafe extern "C" fn bind(
    context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    guarded(|| {
        if context.is_null()
            || query.is_null()
            || output.is_null()
            || !(query as usize).is_multiple_of(align_of::<NativeInterfaceQuery>())
            || !(output as usize).is_multiple_of(align_of::<*const NativeInterface>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime supplies valid query and writable output.
        let query = unsafe { &*query };
        unsafe { *output = std::ptr::null() };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != 2
            || query.min_minor > 0
            || query.min_size as usize > size_of::<OrderedAudioTransportApiV2>()
        {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: principal bytes are borrowed through this call.
        let owner = match unsafe { principal.read() }
            .ok()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        {
            Some(owner) if !owner.is_empty() && owner.len() <= 128 => owner.to_owned(),
            _ => return STATUS_INVALID_ARGUMENT,
        };
        // SAFETY: registered factory remains live until binding release.
        let factory = unsafe { &*context.cast::<Factory>() };
        let mut binding = Box::new(Binding {
            api: OrderedAudioTransportApiV2 {
                base: NativeInterface {
                    header: AbiHeader {
                        size: size_of::<OrderedAudioTransportApiV2>() as u32,
                        major: 2,
                        minor: 0,
                    },
                    context: std::ptr::null_mut(),
                    release: Some(release),
                },
                probe,
                prepare,
                submit,
            },
            shared: factory.shared.clone(),
            owner,
        });
        binding.api.base.context = (&mut *binding as *mut Binding).cast();
        let binding = Box::into_raw(binding);
        // SAFETY: runtime owns returned binding until release.
        unsafe { *output = &(*binding).api.base };
        STATUS_OK
    })
}
unsafe extern "C" fn release(context: *mut c_void) {
    let _ = guarded(|| {
        if !context.is_null() {
            unsafe { drop(Box::from_raw(context.cast::<Binding>())) };
        }
        STATUS_OK
    });
}
unsafe fn with_binding(
    context: *mut c_void,
    operation: impl FnOnce(&mut super::Controller, &str) -> Status,
) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: runtime keeps this binding alive during the callback.
    let binding = unsafe { &*context.cast::<Binding>() };
    let mut controller = match binding.shared.try_lock() {
        Ok(controller) => controller,
        Err(TryLockError::WouldBlock) => return MEDIA_BUSY,
        Err(TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
    };
    operation(&mut controller, &binding.owner)
}
unsafe extern "C" fn probe(
    context: *mut c_void,
    stream: u64,
    selection: *const AudioTransportSelectionV2,
) -> Status {
    guarded(|| {
        if selection.is_null()
            || !(selection as usize).is_multiple_of(align_of::<AudioTransportSelectionV2>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller lends an initialized selection for the call.
        unsafe {
            with_binding(context, |controller, owner| {
                controller.probe_ordered_audio(owner, stream, &*selection)
            })
        }
    })
}
unsafe extern "C" fn prepare(
    context: *mut c_void,
    stream: u64,
    selection: *const AudioTransportSelectionV2,
) -> Status {
    guarded(|| {
        if selection.is_null()
            || !(selection as usize).is_multiple_of(align_of::<AudioTransportSelectionV2>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller lends an initialized selection for the call.
        unsafe {
            with_binding(context, |controller, owner| {
                controller.prepare_ordered_audio(owner, stream, &*selection)
            })
        }
    })
}
unsafe extern "C" fn submit(
    context: *mut c_void,
    stream: u64,
    packet: *const NegotiatedAudioPacket,
) -> Status {
    guarded(|| {
        if packet.is_null()
            || !(packet as usize).is_multiple_of(align_of::<NegotiatedAudioPacket>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller retains packet and nested bytes during the call.
        unsafe {
            with_binding(context, |controller, owner| {
                controller.submit_ordered_audio(owner, stream, &*packet)
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        controller::Controller,
        model::{Operation, Settings},
        stream::{
            ordered_audio::tests::{packet, selection},
            tests::{receivers, request},
        },
    };
    use amaterasu_media::{AUDIO_CODEC_AC3, AUDIO_FRAMING_AC3_SYNCFRAME, MEDIA_CLOSED};
    use std::sync::{Arc, Mutex};

    #[test]
    fn native_ordered_transport_enforces_owner_selection_and_retirement() {
        let (video, audio) = receivers();
        let shared = Arc::new(Mutex::new(Controller::new(Settings {
            enabled: true,
            allowed_principals: vec!["owner".into()],
            ordered_audio_service: Some("example.audio-plan/1".into()),
            ..Default::default()
        })));
        let id = shared
            .lock()
            .unwrap()
            .handle("owner", Operation::Open(request(&video, &audio, false)))
            .unwrap()
            .created
            .unwrap();
        let factory = Factory::new(shared.clone());
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: 2,
            min_minor: 0,
            min_size: size_of::<OrderedAudioTransportApiV2>() as u32,
        };
        let select = selection(AUDIO_CODEC_AC3, AUDIO_FRAMING_AC3_SYNCFRAME, 4);
        for owner in ["other", "owner"] {
            let mut output = std::ptr::null();
            // SAFETY: test-owned factory, query and output remain live; releases are paired.
            unsafe {
                assert_eq!(
                    factory.api.bind.unwrap()(
                        factory.api.context,
                        BorrowedBytes::new(owner.as_bytes()),
                        &query,
                        &mut output
                    ),
                    STATUS_OK
                );
                let api = &*output.cast::<OrderedAudioTransportApiV2>();
                let expected = if owner == "owner" {
                    STATUS_OK
                } else {
                    MEDIA_CLOSED
                };
                assert_eq!((api.probe)(api.base.context, id, &select), expected);
                assert_eq!((api.prepare)(api.base.context, id, &select), expected);
                let bytes = [1, 2, 3, 4];
                assert_eq!(
                    (api.submit)(api.base.context, id, &packet(&bytes, 0, 123_000, 0)),
                    expected
                );
                if owner == "owner" {
                    shared.lock().unwrap().tick();
                    let mut received = [0u8; 128];
                    let len = audio.recv(&mut received).unwrap();
                    assert_eq!(&received[12..len], &bytes);
                    shared.lock().unwrap().close(owner, id).unwrap();
                    assert_eq!(
                        (api.submit)(api.base.context, id, &packet(&bytes, 1, 155_000, 0)),
                        MEDIA_CLOSED
                    );
                }
                api.base.release.unwrap()(api.base.context);
            }
        }
    }
}
