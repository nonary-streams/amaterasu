use super::*;

#[test]
fn authenticated_touch_and_pen_records_preserve_identity_and_reject_malformed_data() {
    let mut touch = vec![0u8; 36];
    touch[..4].copy_from_slice(&32u32.to_be_bytes());
    touch[4..8].copy_from_slice(&0x55000002u32.to_le_bytes());
    touch[8] = 1;
    touch[12..16].copy_from_slice(&0x12345678u32.to_le_bytes());
    for offset in [16, 20, 24, 28, 32] {
        touch[offset..offset + 4].copy_from_slice(&0.5f32.to_bits().to_le_bytes());
    }
    assert!(matches!(
        client(0x0206, &touch),
        Ok(Message::Input(Input::PenTouch(PenTouch {
            pointer_id: 0x12345678,
            device: PointerDevice::Touch,
            event: PointerEvent::Down,
            ..
        })))
    ));
    touch[8] = 0;
    assert_eq!(client(0x0206, &touch), Err(DecodeError::InvalidValue));
    touch[8] = 1;
    touch[16..20].copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
    assert_eq!(client(0x0206, &touch), Err(DecodeError::InvalidValue));
    touch.pop();
    assert_eq!(client(0x0206, &touch), Err(DecodeError::InvalidLength));

    let mut pen = vec![0u8; 36];
    pen[..4].copy_from_slice(&32u32.to_be_bytes());
    pen[4..8].copy_from_slice(&0x55000003u32.to_le_bytes());
    pen[8] = 4;
    pen[9] = 2;
    pen[10] = 5;
    pen[26] = 0xff;
    pen[24..26].copy_from_slice(&0xffffu16.to_le_bytes());
    assert!(matches!(
        client(0x0206, &pen),
        Ok(Message::Input(Input::PenTouch(PenTouch {
            pointer_id: 1,
            device: PointerDevice::Pen,
            event: PointerEvent::Cancel,
            tool: Some(2),
            buttons: Some(5),
            tilt: Some(0xff),
            rotation: 0xffff,
            ..
        })))
    ));
    pen[10] = 8;
    assert_eq!(client(0x0206, &pen), Err(DecodeError::InvalidValue));
}

fn controller_packet(magic: u32, body: &[u8]) -> Vec<u8> {
    let mut packet = vec![0u8; 8 + body.len()];
    let declared_size = (packet.len() - 4) as u32;
    packet[..4].copy_from_slice(&declared_size.to_be_bytes());
    packet[4..8].copy_from_slice(&magic.to_le_bytes());
    packet[8..].copy_from_slice(body);
    packet
}

#[test]
fn authenticated_controller_extension_packets_decode_all_native_fields() {
    let mut arrival = [0u8; 8];
    arrival[0] = 3;
    arrival[1] = 2;
    arrival[2..4].copy_from_slice(&0x0123u16.to_le_bytes());
    arrival[4..8].copy_from_slice(&0x89ab_cdefu32.to_le_bytes());
    assert_eq!(
        client(0x0206, &controller_packet(0x55000004, &arrival)),
        Ok(Message::Input(Input::ControllerArrival(
            ControllerArrival {
                number: 3,
                controller_type: 2,
                capabilities: 0x0123,
                supported_buttons: 0x89ab_cdef,
            }
        )))
    );

    let mut touch = [0u8; 20];
    touch[0] = 2;
    touch[1] = 6;
    touch[3] = 1;
    touch[4..8].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    touch[8..12].copy_from_slice(&0.25f32.to_bits().to_le_bytes());
    touch[12..16].copy_from_slice(&0.5f32.to_bits().to_le_bytes());
    touch[16..20].copy_from_slice(&0.75f32.to_bits().to_le_bytes());
    assert_eq!(
        client(0x0206, &controller_packet(0x55000005, &touch)),
        Ok(Message::Input(Input::ControllerTouch(ControllerTouch {
            number: 2,
            touchpad_index: 1,
            event: ControllerTouchEvent::HoverLeave,
            pointer_id: 0x1234_5678,
            x: 0.25f32.to_bits(),
            y: 0.5f32.to_bits(),
            pressure: 0.75f32.to_bits(),
        })))
    );

    let mut motion = [0u8; 16];
    motion[0] = 5;
    motion[1] = 2;
    motion[4..8].copy_from_slice(&(-1.25f32).to_bits().to_le_bytes());
    motion[8..12].copy_from_slice(&2.5f32.to_bits().to_le_bytes());
    motion[12..16].copy_from_slice(&3.75f32.to_bits().to_le_bytes());
    assert_eq!(
        client(0x0206, &controller_packet(0x55000006, &motion)),
        Ok(Message::Input(Input::ControllerMotion(ControllerMotion {
            number: 5,
            motion_type: ControllerMotionType::Gyro,
            x: (-1.25f32).to_bits(),
            y: 2.5f32.to_bits(),
            z: 3.75f32.to_bits(),
        })))
    );

    let battery = [7, 3, 100, 0];
    assert_eq!(
        client(0x0206, &controller_packet(0x55000007, &battery)),
        Ok(Message::Input(Input::ControllerBattery(
            ControllerBattery {
                number: 7,
                state: ControllerBatteryState::Charging,
                percentage: Some(100),
            }
        )))
    );
    let unknown_battery = [7, 0, 0xff, 0];
    assert_eq!(
        client(0x0206, &controller_packet(0x55000007, &unknown_battery)),
        Ok(Message::Input(Input::ControllerBattery(
            ControllerBattery {
                number: 7,
                state: ControllerBatteryState::Unknown,
                percentage: None,
            }
        )))
    );
}

#[test]
fn controller_extension_packets_reject_bad_lengths_reserved_fields_and_values() {
    let valid_arrival = [0, 2, 0, 0, 0, 0, 0, 0];
    let mut invalid_arrival = valid_arrival;
    invalid_arrival[0] = 16;
    assert_eq!(
        client(0x0206, &controller_packet(0x55000004, &invalid_arrival)),
        Err(DecodeError::InvalidValue)
    );

    let mut invalid_touch = [0u8; 20];
    invalid_touch[2] = 1;
    assert_eq!(
        client(0x0206, &controller_packet(0x55000005, &invalid_touch)),
        Err(DecodeError::InvalidValue)
    );
    invalid_touch[2] = 0;
    invalid_touch[3] = 2;
    assert_eq!(
        client(0x0206, &controller_packet(0x55000005, &invalid_touch)),
        Err(DecodeError::InvalidValue)
    );
    invalid_touch[3] = 0;
    invalid_touch[1] = 8;
    assert_eq!(
        client(0x0206, &controller_packet(0x55000005, &invalid_touch)),
        Err(DecodeError::InvalidValue)
    );
    invalid_touch[1] = 1;
    invalid_touch[8..12].copy_from_slice(&f32::INFINITY.to_bits().to_le_bytes());
    assert_eq!(
        client(0x0206, &controller_packet(0x55000005, &invalid_touch)),
        Err(DecodeError::InvalidValue)
    );

    let mut invalid_motion = [0u8; 16];
    invalid_motion[0] = 1;
    invalid_motion[1] = 3;
    assert_eq!(
        client(0x0206, &controller_packet(0x55000006, &invalid_motion)),
        Err(DecodeError::InvalidValue)
    );
    invalid_motion[1] = 1;
    invalid_motion[2] = 1;
    assert_eq!(
        client(0x0206, &controller_packet(0x55000006, &invalid_motion)),
        Err(DecodeError::InvalidValue)
    );
    invalid_motion[2] = 0;
    invalid_motion[4..8].copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
    assert_eq!(
        client(0x0206, &controller_packet(0x55000006, &invalid_motion)),
        Err(DecodeError::InvalidValue)
    );

    for invalid_battery in [[0, 6, 0, 0], [0, 0, 101, 0], [0, 0, 0, 1], [16, 0, 0, 0]] {
        assert_eq!(
            client(0x0206, &controller_packet(0x55000007, &invalid_battery)),
            Err(DecodeError::InvalidValue)
        );
    }

    for (magic, body) in [
        (0x55000004, &valid_arrival[..]),
        (0x55000005, &invalid_touch[..]),
        (0x55000006, &invalid_motion[..]),
        (0x55000007, &[0, 0, 0, 0][..]),
    ] {
        let packet = controller_packet(magic, body);
        assert_eq!(
            client(0x0206, &packet[..packet.len() - 1]),
            Err(DecodeError::InvalidLength)
        );
        let mut trailing = packet;
        trailing.push(0);
        assert_eq!(client(0x0206, &trailing), Err(DecodeError::InvalidLength));
    }
}

fn fixture(name: &str) -> (u16, Vec<u8>) {
    let row = include_str!("vectors.txt")
        .lines()
        .find(|line| line.split_whitespace().next() == Some(name))
        .unwrap();
    let mut fields = row.split_whitespace();
    fields.next();
    let kind = u16::from_str_radix(fields.next().unwrap(), 16).unwrap();
    let hex = fields.next().unwrap();
    let bytes = hex
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    (kind, bytes)
}

fn client(kind: u16, bytes: &[u8]) -> Result<Message<'_>, DecodeError> {
    decode(
        Profile::SunshineModern,
        Direction::ClientToHost,
        kind,
        bytes,
    )
}

#[test]
fn primary_shaped_control_vectors_decode_with_mixed_byte_orders() {
    let (_, ping) = fixture("ping");
    assert_eq!(
        client(0x200, &ping),
        Ok(Message::Ping {
            timestamp: 0x12345678,
            opaque_tail: [0xab, 0xcd]
        })
    );
    let (_, references) = fixture("references");
    assert_eq!(
        client(0x301, &references),
        Ok(Message::InvalidateReferences {
            first: 0x12345678,
            last: 0x12345679
        })
    );
    let (_, fec) = fixture("fec");
    let Message::FecFeedback(value) = client(0x5502, &fec).unwrap() else {
        panic!()
    };
    assert_eq!(
        (
            value.frame,
            value.total_data,
            value.received_parity,
            value.block_index
        ),
        (0x12345678, 100, 19, 1)
    );
    assert_eq!(client(0x302, &[0, 0]), Ok(Message::RequestIdr));
    assert_eq!(client(0x307, &[0]), Ok(Message::Start));
}

#[test]
fn every_supported_fixture_rejects_truncation_and_trailing_bytes() {
    for name in [
        "ping",
        "idr",
        "start",
        "references",
        "ltr",
        "fec",
        "keyboard",
        "relative",
        "absolute",
        "button",
        "scroll",
        "horizontal",
        "controller",
        "haptics",
    ] {
        let (kind, bytes) = fixture(name);
        assert!(client(kind, &bytes).is_ok(), "{name}");
        for length in 0..bytes.len() {
            assert!(client(kind, &bytes[..length]).is_err(), "{name}:{length}");
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert_eq!(
            client(kind, &trailing),
            Err(DecodeError::InvalidLength),
            "{name}"
        );
    }
}

#[test]
fn direction_and_legacy_never_turn_into_active_client_actions() {
    let (_, bytes) = fixture("termination");
    assert_eq!(client(0x109, &bytes), Err(DecodeError::WrongDirection));
    assert_eq!(
        decode(
            Profile::SunshineModern,
            Direction::HostToClient,
            0x109,
            &bytes
        ),
        Ok(Message::Termination { reason: 0x800e9403 })
    );
    assert_eq!(
        decode(Profile::Legacy, Direction::ClientToHost, 0x302, &[0, 0]),
        Err(DecodeError::UnsupportedProfile)
    );
    assert!(matches!(
        decode(
            Profile::SunshineModern,
            Direction::HostToClient,
            0x5502,
            &[]
        ),
        Ok(Message::Unsupported { .. })
    ));
}

#[test]
fn keyboard_mouse_and_controller_fields_are_not_host_native_endian() {
    let (_, bytes) = fixture("keyboard");
    assert_eq!(
        client(0x206, &bytes),
        Ok(Message::Input(Input::Keyboard {
            pressed: true,
            flags: 1,
            key_code: 0x8041,
            modifiers: 5
        }))
    );
    let (_, bytes) = fixture("relative");
    assert_eq!(
        client(0x206, &bytes),
        Ok(Message::Input(Input::RelativeMouse { x: -300, y: 700 }))
    );
    let (_, bytes) = fixture("absolute");
    assert_eq!(
        client(0x206, &bytes),
        Ok(Message::Input(Input::AbsoluteMouse {
            x: 1000,
            y: 500,
            reference_width: 1920,
            reference_height: 1080
        }))
    );
    let (_, bytes) = fixture("controller");
    let Message::Input(Input::Controller(state)) = client(0x206, &bytes).unwrap() else {
        panic!()
    };
    assert_eq!(
        (state.number, state.buttons, state.left_x, state.left_y),
        (3, 0x55aa1234, -32768, 32767)
    );
}

#[test]
fn unknown_input_is_preserved_and_invalid_fields_are_rejected() {
    let bytes = [0, 0, 0, 5, 0xef, 0xbe, 0xad, 0xde, 7];
    assert_eq!(
        client(0x206, &bytes),
        Ok(Message::Input(Input::Unsupported {
            magic: 0xdeadbeef,
            payload: &[7]
        }))
    );
    assert_eq!(
        client(0xffff, &[7]),
        Ok(Message::Unsupported {
            kind: 0xffff,
            payload: &[7]
        })
    );
    for (name, offset) in [
        ("keyboard", 12),
        ("absolute", 12),
        ("scroll", 12),
        ("controller", 8),
        ("references", 4),
    ] {
        let (kind, mut bytes) = fixture(name);
        bytes[offset] ^= 1;
        assert_eq!(client(kind, &bytes), Err(DecodeError::InvalidValue));
    }
    assert_eq!(
        client(0xffff, &vec![0; 65512]),
        Err(DecodeError::InvalidLength)
    );
}

#[test]
fn text_requires_bounded_complete_utf8() {
    let (_, bytes) = fixture("text");
    assert_eq!(client(0x206, &bytes), Ok(Message::Input(Input::Text("😀"))));
    let mut broken = bytes.clone();
    broken[8] = 0xff;
    assert_eq!(client(0x206, &broken), Err(DecodeError::InvalidText));
    let mut zero = bytes;
    zero[8] = 0;
    assert!(client(0x206, &zero).is_err());
}
