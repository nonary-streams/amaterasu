use super::*;

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
