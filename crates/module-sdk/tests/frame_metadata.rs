use amaterasu_media::*;
use amaterasu_module_sdk::{abi::*, encode::FormatId, media::metadata::*};
use std::{ffi::c_void, mem::size_of, ptr};

fn schema(value: u64) -> FormatId {
    FormatId {
        namespace_id: 1,
        value,
        schema: 1,
    }
}
fn identity() -> FrameMetadataIdentity {
    FrameMetadataIdentity {
        session_id: 11,
        submission_id: 12,
        source_id: 13,
        source_generation: 14,
        config_generation: 15,
        timestamp_us: 16,
        clock_domain: FRAME_CLOCK_HOST_MONOTONIC,
        clock_epoch: [9; 16],
    }
}
fn context(id: FrameMetadataIdentity) -> FrameMetadataContext {
    FrameMetadataContext {
        session_id: id.session_id,
        source_id: id.source_id,
        source_generation: id.source_generation,
        config_generation: id.config_generation,
        clock_domain: id.clock_domain,
        clock_epoch: id.clock_epoch,
    }
}
fn bundle() -> FrameMetadataBundle {
    FrameMetadataBundle {
        identity: identity(),
        owner: CarriageOwner::FramerInsert,
        items: vec![
            FrameMetadataItem {
                schema: schema(1),
                required: true,
                payload: vec![0xb5, 1],
            },
            FrameMetadataItem {
                schema: schema(2),
                required: false,
                payload: vec![0x26, 2],
            },
        ],
    }
}
fn policy(
    stage: MetadataStage,
    owner: CarriageOwner,
    entries: &[(u64, MetadataTreatment)],
    drop: bool,
) -> MetadataPolicy {
    MetadataPolicy {
        session_id: identity().session_id,
        capability_generation: 1,
        stage,
        owner,
        capabilities: entries
            .iter()
            .map(|(id, treatment)| MetadataCapability {
                schema: schema(*id),
                treatment: *treatment,
            })
            .collect(),
        allow_drop_unknown_optional: drop,
    }
}
fn descriptor(kind: u32) -> FrameDescriptor {
    FrameDescriptor {
        kind,
        version: 0,
        size: 0,
        reserved: 0,
        namespace: [0; 16],
        data: ptr::null(),
    }
}
unsafe extern "C" fn retain(_: *mut c_void) -> Status {
    STATUS_OK
}
unsafe extern "C" fn release(_: *mut c_void, _: *const FrameDescriptor) {}
fn frame(bytes: &[u8], id: FrameMetadataIdentity) -> FrameResourceMetadataFrameV1 {
    FrameResourceMetadataFrameV1 {
        size: FRAME_RESOURCE_METADATA_FRAME_V1_SIZE,
        version: 1,
        frame: CaptureResourceFrame {
            offer_generation: 1,
            offer_id: 2,
            timestamp_us: id.timestamp_us,
            capture_pacing_timestamp_us: 16,
            sequence: id.submission_id,
            width: 1,
            height: 1,
            freshness: 1,
            reserved: 0,
            color: FrameColorMetadata::default(),
            clock: FrameClockMetadata {
                version: 1,
                size: size_of::<FrameClockMetadata>() as u32,
                domain: id.clock_domain,
                reserved: 0,
                epoch_id: id.clock_epoch,
            },
            pacing_clock: FrameClockMetadata::default(),
            resource: descriptor(FRAME_RESOURCE_CPU),
            acquire_sync: descriptor(FRAME_SYNC_NONE),
            context: ptr::null_mut(),
            retain,
            release,
        },
        metadata: BorrowedBytes::new(bytes),
    }
}

#[test]
fn canonical_envelope_binds_schema_and_frame_identity() {
    let original = bundle();
    let bytes = original.encode().unwrap();
    assert_eq!(FrameMetadataBundle::decode(&bytes), Ok(original.clone()));
    assert_eq!(bytes.len(), 80 + 2 * 32 + 4);
    assert_eq!(&bytes[0..4], b"AFM1");
    assert_eq!(&bytes[4..8], &2u32.to_le_bytes());
    assert_eq!(&bytes[8..16], &11u64.to_le_bytes());
    assert_eq!(&bytes[16..24], &12u64.to_le_bytes());
    assert_eq!(&bytes[32..40], &14u64.to_le_bytes());
    assert_eq!(&bytes[40..48], &15u64.to_le_bytes());
    assert_eq!(&bytes[56..60], &FRAME_CLOCK_HOST_MONOTONIC.to_le_bytes());
    assert_eq!(&bytes[60..76], &[9; 16]);
    assert_eq!(&bytes[76..80], &2u32.to_le_bytes());
    assert_eq!(
        &bytes[104..108],
        &FRAME_METADATA_ITEM_REQUIRED.to_le_bytes()
    );
    assert_eq!(&bytes[108..112], &2u32.to_le_bytes());
    assert_eq!(&bytes[112..114], &[0xb5, 1]);
    let policy = policy(
        MetadataStage::Processor,
        CarriageOwner::FramerInsert,
        &[
            (1, MetadataTreatment::Forward),
            (2, MetadataTreatment::Forward),
        ],
        false,
    );
    let (copied, treatment) =
        unsafe { copy_frame_metadata(&frame(&bytes, identity()), identity(), &policy) }.unwrap();
    assert_eq!(copied, original);
    assert_eq!(
        treatment,
        [MetadataDisposition::Forward, MetadataDisposition::Forward]
    );
    let mut wrong = identity();
    wrong.source_generation += 1;
    assert_eq!(
        unsafe { copy_frame_metadata(&frame(&bytes, identity()), wrong, &policy) },
        Err(MetadataError::Foreign)
    );
    let mut wrong_config = identity();
    wrong_config.config_generation += 1;
    assert_eq!(
        unsafe { copy_frame_metadata(&frame(&bytes, identity()), wrong_config, &policy) },
        Err(MetadataError::Foreign)
    );
    let mut wrong_session = identity();
    wrong_session.session_id += 1;
    assert_eq!(
        unsafe { copy_frame_metadata(&frame(&bytes, identity()), wrong_session, &policy) },
        Err(MetadataError::Foreign)
    );
    let mut wrong = identity();
    wrong.clock_epoch = [8; 16];
    assert_eq!(
        unsafe { copy_frame_metadata(&frame(&bytes, identity()), wrong, &policy) },
        Err(MetadataError::Foreign)
    );
    let mut bad = bytes.clone();
    bad.push(0);
    assert_eq!(
        FrameMetadataBundle::decode(&bad),
        Err(MetadataError::Invalid)
    );
    let mut wrong_size = frame(&bytes, identity());
    wrong_size.size -= 1;
    assert_eq!(
        unsafe { copy_frame_metadata(&wrong_size, identity(), &policy) },
        Err(MetadataError::Invalid)
    );
}

#[test]
fn encoded_output_association_enforces_selected_carriage_and_identity() {
    for owner in [CarriageOwner::EncoderEmbed, CarriageOwner::FramerInsert] {
        let mut admitted = bundle();
        admitted.owner = owner;
        let source = admitted.encode().unwrap();
        let treatment = match owner {
            CarriageOwner::EncoderEmbed => MetadataTreatment::Apply,
            CarriageOwner::FramerInsert => MetadataTreatment::Forward,
        };
        let p = policy(
            MetadataStage::Encoder,
            owner,
            &[(1, treatment), (2, treatment)],
            false,
        );
        let mut input = FrameMetadataAdmission::new(context(identity()), p, 4096).unwrap();
        unsafe { input.prepare(&frame(&source, identity()), identity(), 1) }.unwrap();
        let encoded = input.pending(1).unwrap().unwrap().bytes().to_vec();
        let codec = EncoderFormatId {
            namespace_id: 1,
            value: 50,
            schema: 1,
        };
        let framing = EncoderFormatId {
            namespace_id: 1,
            value: 51,
            schema: 1,
        };
        let selection = EncodedMetadataSelection {
            stream: 2,
            generation: 3,
            offer_id: 4,
            codec,
            framing,
            clock_epoch: [7; 16],
        };
        let invalid_selection = EncodedMetadataSelection {
            stream: 0,
            ..selection
        };
        assert!(matches!(
            EncodedMetadataAssociation::new(&mut input, 1, invalid_selection),
            Err(MetadataError::Invalid)
        ));
        let mut association = EncodedMetadataAssociation::new(&mut input, 1, selection).unwrap();
        assert!(matches!(
            EncodedMetadataAssociation::new(&mut input, 1, selection),
            Err(MetadataError::Duplicate)
        ));
        let payload = [1, 2, 3];
        let unit = NegotiatedVideoUnit {
            size: size_of::<NegotiatedVideoUnit>() as u32,
            version: VIDEO_UNIT_VERSION,
            stream: 2,
            generation: 3,
            offer_id: 4,
            codec,
            framing,
            access_unit_id: 5,
            submission_id: identity().submission_id,
            source_id: identity().source_id,
            config_generation: identity().config_generation,
            timestamp_us: 999,
            clock: FrameClockMetadata {
                version: 1,
                size: size_of::<FrameClockMetadata>() as u32,
                domain: FRAME_CLOCK_STREAM_RELATIVE,
                reserved: 0,
                epoch_id: [7; 16],
            },
            flags: VIDEO_UNIT_KEYFRAME,
            reserved: 0,
            data: BorrowedBytes::new(&payload),
        };
        let expected = match owner {
            CarriageOwner::EncoderEmbed => MetadataApplication::EncoderApplied,
            CarriageOwner::FramerInsert => MetadataApplication::Forwarded,
        };
        assert_eq!(
            association.accept_encoder_output(&unit, &encoded, expected, 2),
            Err(MetadataError::Stale)
        );
        assert_eq!(
            association.accept_encoder_output(
                &NegotiatedVideoUnit {
                    submission_id: 13,
                    ..unit
                },
                &encoded,
                expected,
                1
            ),
            Err(MetadataError::Foreign)
        );
        assert_eq!(
            association.accept_encoder_output(&unit, &encoded[..encoded.len() - 1], expected, 1),
            Err(MetadataError::Foreign)
        );
        assert_eq!(
            association.accept_encoder_output(
                &unit,
                &encoded,
                MetadataApplication::FramerApplied,
                1
            ),
            Err(MetadataError::WrongOwner)
        );
        association
            .accept_encoder_output(&unit, &encoded, expected, 1)
            .unwrap();
        assert!(matches!(
            EncodedMetadataAssociation::new(&mut input, 1, selection),
            Err(MetadataError::Duplicate)
        ));
        assert_eq!(
            association.accept_encoder_output(&unit, &encoded, expected, 1),
            Err(MetadataError::Duplicate)
        );
        if owner == CarriageOwner::FramerInsert {
            assert_eq!(
                association.retire(&unit, &encoded, 1),
                Err(MetadataError::WrongOwner)
            );
            assert_eq!(
                association.accept_framer_output(
                    &unit,
                    &encoded,
                    MetadataApplication::EncoderApplied,
                    1
                ),
                Err(MetadataError::WrongOwner)
            );
            association
                .accept_framer_output(&unit, &encoded, MetadataApplication::FramerApplied, 1)
                .unwrap();
        } else {
            assert_eq!(
                association.retire(&unit, &encoded, 1),
                Err(MetadataError::WrongOwner)
            );
            assert_eq!(
                association.accept_framer_output(
                    &unit,
                    &encoded,
                    MetadataApplication::FramerApplied,
                    1
                ),
                Err(MetadataError::WrongOwner)
            );
            association
                .accept_framer_output(&unit, &encoded, MetadataApplication::EncoderApplied, 1)
                .unwrap();
        }
        assert_eq!(
            association.accept_framer_output(
                &unit,
                &encoded,
                MetadataApplication::FramerApplied,
                1
            ),
            Err(MetadataError::Duplicate)
        );
        association.check_transport(&unit, &encoded, 1).unwrap();
        association.check_transport(&unit, &encoded, 1).unwrap(); // BUSY did not retire it.
        assert_eq!(
            association.check_transport(&unit, &encoded, 2),
            Err(MetadataError::Stale)
        );
        assert_eq!(
            association.check_transport(
                &NegotiatedVideoUnit {
                    timestamp_us: 1000,
                    ..unit
                },
                &encoded,
                1
            ),
            Err(MetadataError::Foreign)
        );
        assert_eq!(
            association.retire(
                &NegotiatedVideoUnit {
                    access_unit_id: 6,
                    ..unit
                },
                &encoded,
                1
            ),
            Err(MetadataError::Foreign)
        );
        association.retire(&unit, &encoded, 1).unwrap();
        assert!(matches!(
            EncodedMetadataAssociation::new(&mut input, 1, selection),
            Err(MetadataError::Duplicate)
        ));
        assert_eq!(
            association.retire(&unit, &encoded, 1),
            Err(MetadataError::Closed)
        );
        input.retire(identity().submission_id).unwrap();
        let mut next = admitted.clone();
        next.identity.submission_id += 1;
        let next_bytes = next.encode().unwrap();
        unsafe { input.prepare(&frame(&next_bytes, next.identity), next.identity, 1) }.unwrap();
        assert!(EncodedMetadataAssociation::new(&mut input, 1, selection).is_ok());
    }
}

#[test]
fn trusted_policy_rejects_unknown_required_and_controls_optional_passthrough() {
    let bundle = bundle();
    let unknown = policy(
        MetadataStage::Processor,
        CarriageOwner::FramerInsert,
        &[],
        true,
    );
    assert_eq!(
        bundle.verify(identity(), &unknown),
        Err(MetadataError::Unsupported)
    );
    let no_optional = policy(
        MetadataStage::Processor,
        CarriageOwner::FramerInsert,
        &[(1, MetadataTreatment::Forward)],
        false,
    );
    assert_eq!(
        bundle.verify(identity(), &no_optional),
        Err(MetadataError::Unsupported)
    );
    let optional_drop = policy(
        MetadataStage::Processor,
        CarriageOwner::FramerInsert,
        &[(1, MetadataTreatment::Forward)],
        true,
    );
    assert_eq!(
        bundle.verify(identity(), &optional_drop),
        Ok(vec![
            MetadataDisposition::Forward,
            MetadataDisposition::DropOptional
        ])
    );
    let pass = policy(
        MetadataStage::Processor,
        CarriageOwner::FramerInsert,
        &[
            (1, MetadataTreatment::Forward),
            (2, MetadataTreatment::Forward),
        ],
        false,
    );
    assert_eq!(
        bundle.verify(identity(), &pass),
        Ok(vec![
            MetadataDisposition::Forward,
            MetadataDisposition::Forward
        ])
    );
    let wrong_owner = policy(
        MetadataStage::Processor,
        CarriageOwner::EncoderEmbed,
        &[(1, MetadataTreatment::Forward)],
        true,
    );
    assert_eq!(
        bundle.verify(identity(), &wrong_owner),
        Err(MetadataError::WrongOwner)
    );
    let duplicate_application = policy(
        MetadataStage::Encoder,
        CarriageOwner::FramerInsert,
        &[(1, MetadataTreatment::Apply)],
        true,
    );
    assert_eq!(
        bundle.verify(identity(), &duplicate_application),
        Err(MetadataError::WrongOwner)
    );
    let framer = policy(
        MetadataStage::Framer,
        CarriageOwner::FramerInsert,
        &[(1, MetadataTreatment::Apply), (2, MetadataTreatment::Apply)],
        false,
    );
    assert_eq!(
        bundle.verify(identity(), &framer),
        Ok(vec![MetadataDisposition::Apply, MetadataDisposition::Apply])
    );
}

#[test]
fn bounded_admission_copies_filters_retries_and_retires_once() {
    let mut source = bundle().encode().unwrap();
    let original = source.clone();
    let policy = policy(
        MetadataStage::Encoder,
        CarriageOwner::FramerInsert,
        &[(1, MetadataTreatment::Forward)],
        true,
    );
    let mut guard = FrameMetadataAdmission::new(
        context(identity()),
        policy,
        2 * FRAME_METADATA_MAX_BUNDLE_BYTES,
    )
    .unwrap();
    assert_eq!(
        unsafe { guard.prepare(&frame(&source, identity()), identity(), 1) },
        Ok(MetadataAdmissionResult::Prepared)
    );
    source.fill(0);
    let pending = guard.pending(1).unwrap().unwrap();
    let emitted = FrameMetadataBundle::decode(pending.bytes()).unwrap();
    assert_eq!(emitted.items.len(), 1);
    assert_eq!(emitted.items[0].payload, [0xb5, 1]);
    let options = pending.encoder_options().unwrap();
    let options = options.descriptor();
    assert_eq!(options.submission_id, identity().submission_id);
    assert_eq!(
        (
            options.side_data_schema.namespace_id,
            options.side_data_schema.value,
            options.side_data_schema.schema
        ),
        (
            BUNDLE_SCHEMA.namespace_id,
            BUNDLE_SCHEMA.value,
            BUNDLE_SCHEMA.schema
        )
    );
    assert_eq!(
        unsafe { options.side_data.read() }.unwrap(),
        pending.bytes()
    );
    assert_eq!(
        unsafe { guard.prepare(&frame(&original, identity()), identity(), 1) },
        Ok(MetadataAdmissionResult::Retry)
    );
    assert!(matches!(guard.pending(2), Err(MetadataError::Stale)));
    assert_eq!(
        unsafe { guard.prepare(&frame(&original, identity()), identity(), 2) },
        Err(MetadataError::Stale)
    );
    let mut changed_frame = frame(&original, identity());
    changed_frame.frame.offer_generation += 1;
    assert_eq!(
        unsafe { guard.prepare(&changed_frame, identity(), 1) },
        Err(MetadataError::Conflict)
    );
    let mut changed = bundle();
    changed.items[0].payload.push(3);
    let changed = changed.encode().unwrap();
    assert_eq!(
        unsafe { guard.prepare(&frame(&changed, identity()), identity(), 1) },
        Err(MetadataError::Conflict)
    );
    let mut next = identity();
    next.submission_id += 1;
    let mut other = bundle();
    other.identity = next;
    let other_bytes = other.encode().unwrap();
    assert_eq!(
        unsafe { guard.prepare(&frame(&other_bytes, next), next, 1) },
        Err(MetadataError::Busy)
    );
    assert_eq!(
        guard.retire(identity().submission_id + 1),
        Err(MetadataError::Foreign)
    );
    guard.retire(identity().submission_id).unwrap();
    assert!(guard.pending(1).unwrap().is_none());
    assert_eq!(
        unsafe { guard.prepare(&frame(&original, identity()), identity(), 1) },
        Err(MetadataError::Stale)
    );
    assert_eq!(
        unsafe { guard.prepare(&frame(&other_bytes, next), next, 1) },
        Ok(MetadataAdmissionResult::Prepared)
    );
    guard.cancel();
    assert!(matches!(guard.pending(1), Err(MetadataError::Closed)));
    assert_eq!(
        unsafe { guard.prepare(&frame(&other_bytes, next), next, 1) },
        Err(MetadataError::Closed)
    );
}

#[test]
fn malformed_bundles_and_unbounded_storage_fail_closed() {
    let mut duplicate = bundle();
    duplicate.items[1].schema = duplicate.items[0].schema;
    assert_eq!(duplicate.encode(), Err(MetadataError::Duplicate));
    let mut large = bundle();
    large.items[0].payload = vec![1; FRAME_METADATA_MAX_ITEM_BYTES + 1];
    assert_eq!(large.encode(), Err(MetadataError::TooLarge));
    let mut unknown_flags = bundle().encode().unwrap();
    unknown_flags[104] = 0x80;
    assert_eq!(
        FrameMetadataBundle::decode(&unknown_flags),
        Err(MetadataError::Invalid)
    );
    let mut truncated = bundle().encode().unwrap();
    truncated.pop();
    assert_eq!(
        FrameMetadataBundle::decode(&truncated),
        Err(MetadataError::Invalid)
    );
    let p = policy(
        MetadataStage::Encoder,
        CarriageOwner::FramerInsert,
        &[(1, MetadataTreatment::Forward)],
        true,
    );
    let mut guard = FrameMetadataAdmission::new(context(identity()), p, 80).unwrap();
    let source = bundle().encode().unwrap();
    assert_eq!(
        unsafe { guard.prepare(&frame(&source, identity()), identity(), 1) },
        Err(MetadataError::TooLarge)
    );
    assert!(guard.pending(1).unwrap().is_none());
}

#[test]
fn additive_tails_require_version_and_size_and_preserve_c_layout() {
    let header = AbiHeader {
        size: CAPTURE_API_V1_10_SIZE,
        major: 1,
        minor: CAPTURE_METADATA_INTERFACE_MINOR,
    };
    assert!(capture_metadata_available(header));
    assert!(!capture_metadata_available(AbiHeader {
        size: header.size - 1,
        ..header
    }));
    assert!(!capture_metadata_available(AbiHeader {
        minor: header.minor - 1,
        ..header
    }));
    #[cfg(target_pointer_width = "64")]
    {
        assert_eq!(size_of::<FrameResourceMetadataFrameV1>(), 272);
        assert_eq!(
            std::mem::offset_of!(CaptureApiV1_10, read_resource_frame_with_metadata),
            size_of::<CaptureApiV1_9>()
        );
        assert_eq!(
            std::mem::offset_of!(FrameResourceConsumerApiV1_1, submit_with_metadata),
            size_of::<FrameResourceConsumerApi>()
        );
        assert_eq!(
            std::mem::offset_of!(FrameResourceProcessorApiV1_2, process_with_metadata),
            size_of::<FrameResourceProcessorApi>()
        );
    }
}

struct ArtifactSink {
    expected: FrameMetadataIdentity,
    input: FrameMetadataBundle,
    policy: MetadataPolicy,
    generation: u64,
    admission: FrameMetadataAdmission,
    busy: bool,
    transfer: u32,
    pixels: Vec<u8>,
}
unsafe extern "C" fn artifact_sink(
    context: *mut c_void,
    frame: *const FrameResourceMetadataFrameV1,
) -> Status {
    if context.is_null() || frame.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    let state = unsafe { &mut *context.cast::<ArtifactSink>() };
    if state.busy {
        return MEDIA_BUSY;
    }
    let frame = unsafe { &*frame };
    let output = match unsafe { copy_frame_metadata(frame, state.expected, &state.policy) } {
        Ok((bundle, _)) => bundle,
        Err(_) => return STATUS_INVALID_ARGUMENT,
    };
    if verify_processor_transition(&state.input, &output, state.expected, &state.policy).is_err() {
        return STATUS_INVALID_ARGUMENT;
    }
    let descriptor = frame.frame.resource;
    if descriptor.kind != FRAME_RESOURCE_CPU
        || descriptor.version != 1
        || descriptor.size as usize != size_of::<CpuFrame>()
        || descriptor.data.is_null()
    {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the fixture provides a live CPU descriptor and pixel storage for this callback.
    let cpu = unsafe { &*descriptor.data.cast::<CpuFrame>() };
    let pixels = match unsafe { cpu.bytes() } {
        Ok(pixels) => pixels,
        Err(_) => return STATUS_INVALID_ARGUMENT,
    };
    state.pixels = pixels.to_vec();
    state.transfer = frame.frame.color.transfer;
    match unsafe {
        state
            .admission
            .prepare(frame, state.expected, state.generation)
    } {
        Ok(MetadataAdmissionResult::Prepared | MetadataAdmissionResult::Retry) => STATUS_OK,
        Err(_) => STATUS_INVALID_ARGUMENT,
    }
}

#[test]
#[ignore = "requires separately built metadata-capable processor provider artifacts"]
fn unchanged_metadata_consumer_propagates_two_provider_artifacts() {
    for (variable, transfer) in [
        ("PROCESSOR_FIXTURE_PQ", FRAME_TRANSFER_PQ),
        ("PROCESSOR_FIXTURE_SRGB", FRAME_TRANSFER_SRGB),
    ] {
        let path = std::env::var(variable).expect("processor fixture path");
        // SAFETY: both paths name our test libraries. Symbols have the declared C
        // signatures, and bindings stay loaded until cancellation and destroy.
        unsafe {
            let library = libloading::Library::new(path).unwrap();
            let create: libloading::Symbol<
                unsafe extern "C" fn() -> *mut FrameResourceProcessorApi,
            > = library.get(b"fixture_processor_create\0").unwrap();
            let destroy: libloading::Symbol<unsafe extern "C" fn(*mut FrameResourceProcessorApi)> =
                library.get(b"fixture_processor_destroy\0").unwrap();
            let capabilities: libloading::Symbol<
                unsafe extern "C" fn(
                    *mut FrameResourceProcessorApi,
                ) -> *const FrameMetadataCapabilitiesApiV1,
            > = library.get(b"fixture_processor_capabilities\0").unwrap();
            let table = create();
            assert!(!table.is_null());
            let claims =
                enumerate_capabilities(&*capabilities(table), identity().session_id).unwrap();
            assert_eq!(claims.generation, 77);
            assert_eq!(claims.claims.len(), 2);
            let prefix = &*table;
            assert!(prefix.base.base.header.minor >= FRAME_RESOURCE_PROCESSOR_METADATA_MINOR);
            assert!(prefix.base.base.header.size >= FRAME_RESOURCE_PROCESSOR_API_V1_2_SIZE);
            let api = &*table.cast::<FrameResourceProcessorApiV1_2>();
            let offer = CaptureResourceOffer {
                id: 5,
                ownership: FRAME_OWNERSHIP_RETAINABLE,
                accessibility: FRAME_ACCESS_GPU,
                reserved: 0,
                resource: descriptor(FRAME_RESOURCE_VENDOR_BASE),
                acquire_sync: descriptor(FRAME_SYNC_NONE),
                completion_sync: descriptor(FRAME_SYNC_NONE),
            };
            let ctx = api.base.base.base.context;
            assert_eq!(
                (api.base.base.configure)(ctx, 1, 4, 8, 9, &offer),
                STATUS_OK
            );
            assert_eq!((api.base.select_output)(ctx, 1, 9, 5), STATUS_OK);
            let mut source = bundle().encode().unwrap();
            let expected_bytes = source.clone();
            let mut pixels = [17, 34, 51, 255];
            let cpu = CpuFrame {
                width: 1,
                height: 1,
                format: PIXEL_BGRA8,
                stride: 4,
                data: pixels.as_ptr(),
                len: pixels.len(),
                timestamp_us: identity().timestamp_us,
                sequence: identity().submission_id,
            };
            let mut input = frame(&source, identity());
            input.frame.resource = FrameDescriptor {
                kind: FRAME_RESOURCE_CPU,
                version: 1,
                size: size_of::<CpuFrame>() as u32,
                reserved: 0,
                namespace: [0; 16],
                data: (&cpu as *const CpuFrame).cast(),
            };
            let policy = claims
                .policy(CarriageOwner::FramerInsert, MetadataStage::Processor, false)
                .unwrap();
            let mut state = ArtifactSink {
                expected: identity(),
                input: bundle(),
                policy: policy.clone(),
                generation: claims.generation,
                admission: FrameMetadataAdmission::new(
                    context(identity()),
                    policy,
                    2 * FRAME_METADATA_MAX_BUNDLE_BYTES,
                )
                .unwrap(),
                busy: true,
                transfer: 0,
                pixels: Vec::new(),
            };
            let sink = FrameResourceMetadataSinkV1 {
                context: (&mut state as *mut ArtifactSink).cast(),
                write: artifact_sink,
            };
            assert_eq!(
                (api.process_with_metadata)(ctx, 1, &input, sink),
                MEDIA_BUSY
            );
            assert!(state.admission.pending(77).unwrap().is_none());
            state.busy = false;
            assert_eq!((api.process_with_metadata)(ctx, 1, &input, sink), STATUS_OK);
            assert_eq!(state.transfer, transfer);
            source.fill(0);
            pixels.fill(0);
            assert_eq!(state.pixels, [17, 34, 51, 255]);
            assert_eq!(
                state.admission.pending(77).unwrap().unwrap().bytes(),
                expected_bytes
            );
            assert_eq!(
                FrameMetadataBundle::decode(state.admission.pending(77).unwrap().unwrap().bytes())
                    .unwrap()
                    .identity,
                identity()
            );
            state.admission.retire(identity().submission_id).unwrap();
            (api.base.base.cancel)(ctx, 1);
            destroy(table);
        }
    }
}

struct CapabilityFixture {
    mode: u32,
}
unsafe extern "C" fn capability_enumerate(
    context: *mut c_void,
    _: u64,
    sink: FrameMetadataCapabilitySinkV1,
    generation: *mut u64,
) -> Status {
    let state = unsafe { &mut *context.cast::<CapabilityFixture>() };
    if state.mode == 3 {
        return MEDIA_BUSY;
    }
    unsafe {
        *generation = if state.mode == 4 { 0 } else { 7 };
    }
    let capability = FrameMetadataCapabilityV1 {
        schema: FrameMetadataSchemaId {
            namespace_id: 1,
            value: 1,
            schema: 1,
        },
        operations: if state.mode == 2 {
            0x8000
        } else {
            FRAME_METADATA_CAP_FORWARD
        },
        reserved: 0,
    };
    let first = unsafe { (sink.write)(sink.context, &capability) };
    if state.mode == 1 {
        let _ = unsafe { (sink.write)(sink.context, &capability) };
        return STATUS_OK;
    }
    if state.mode == 2 {
        return STATUS_OK;
    }
    first
}
#[test]
fn capability_claims_are_bounded_versioned_and_preserve_backpressure() {
    let mut fixture = CapabilityFixture { mode: 0 };
    let api = FrameMetadataCapabilitiesApiV1 {
        base: NativeInterface {
            header: AbiHeader {
                size: FRAME_METADATA_CAPABILITIES_API_V1_SIZE,
                major: 1,
                minor: 0,
            },
            context: (&mut fixture as *mut CapabilityFixture).cast(),
            release: None,
        },
        enumerate: capability_enumerate,
    };
    let invalid_api = FrameMetadataCapabilitiesApiV1 {
        base: NativeInterface {
            header: AbiHeader {
                size: FRAME_METADATA_CAPABILITIES_API_V1_SIZE - 1,
                major: 1,
                minor: 0,
            },
            ..api.base
        },
        enumerate: capability_enumerate,
    };
    assert_eq!(
        unsafe { enumerate_capabilities(&invalid_api, identity().session_id) },
        Err(MetadataError::Invalid)
    );
    let snapshot = unsafe { enumerate_capabilities(&api, identity().session_id) }.unwrap();
    assert_eq!(snapshot.generation, 7);
    assert_eq!(snapshot.session_id, identity().session_id);
    assert_eq!(snapshot.claims.len(), 1);
    let mut foreign_snapshot = snapshot.clone();
    foreign_snapshot.session_id += 1;
    let foreign_policy = foreign_snapshot
        .policy(CarriageOwner::FramerInsert, MetadataStage::Processor, true)
        .unwrap();
    assert_eq!(
        foreign_policy.validate(&bundle()),
        Err(MetadataError::Foreign)
    );
    let forward = snapshot
        .policy(CarriageOwner::FramerInsert, MetadataStage::Processor, true)
        .unwrap();
    assert_eq!(
        forward.validate(&bundle()),
        Ok(vec![
            MetadataDisposition::Forward,
            MetadataDisposition::DropOptional
        ])
    );
    let apply = snapshot
        .policy(CarriageOwner::FramerInsert, MetadataStage::Framer, true)
        .unwrap();
    assert_eq!(apply.validate(&bundle()), Err(MetadataError::Unsupported));
    for (mode, expected) in [
        (1, MetadataError::Invalid),
        (2, MetadataError::Invalid),
        (3, MetadataError::Provider(MEDIA_BUSY)),
        (4, MetadataError::Invalid),
    ] {
        fixture.mode = mode;
        std::hint::black_box(&fixture);
        assert_eq!(
            unsafe { enumerate_capabilities(&api, identity().session_id) },
            Err(expected)
        );
    }
    #[cfg(target_pointer_width = "64")]
    {
        assert_eq!(size_of::<FrameMetadataSchemaId>(), 24);
        assert_eq!(size_of::<FrameMetadataCapabilityV1>(), 32);
        assert_eq!(size_of::<FrameMetadataCapabilitiesApiV1>(), 40);
    }
}

#[test]
fn processor_transition_rejects_lost_or_changed_required_payload() {
    let input = bundle();
    let forwarding = policy(
        MetadataStage::Processor,
        CarriageOwner::FramerInsert,
        &[
            (1, MetadataTreatment::Forward),
            (2, MetadataTreatment::Forward),
        ],
        false,
    );
    assert_eq!(
        verify_processor_transition(&input, &input, identity(), &forwarding),
        Ok(())
    );
    let mut missing = input.clone();
    missing.items.remove(0);
    assert_eq!(
        verify_processor_transition(&input, &missing, identity(), &forwarding),
        Err(MetadataError::Conflict)
    );
    let mut changed = input.clone();
    changed.items[0].payload.push(0xff);
    assert_eq!(
        verify_processor_transition(&input, &changed, identity(), &forwarding),
        Err(MetadataError::Conflict)
    );
    let mut downgraded = input.clone();
    downgraded.items[0].required = false;
    assert_eq!(
        verify_processor_transition(&input, &downgraded, identity(), &forwarding),
        Err(MetadataError::Conflict)
    );
    let mut foreign = input.clone();
    foreign.identity.source_generation += 1;
    assert_eq!(
        verify_processor_transition(&input, &foreign, identity(), &forwarding),
        Err(MetadataError::Foreign)
    );
    let dropping = policy(
        MetadataStage::Processor,
        CarriageOwner::FramerInsert,
        &[(1, MetadataTreatment::Forward)],
        true,
    );
    let mut output = input.clone();
    output.items.pop();
    assert_eq!(
        verify_processor_transition(&input, &output, identity(), &dropping),
        Ok(())
    );
}
