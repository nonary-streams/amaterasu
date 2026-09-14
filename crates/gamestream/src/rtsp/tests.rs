use super::*;

const OPTIONS: &[u8] = b"OPTIONS rtsp://192.0.2.1:48010 RTSP/1.0\r\nCSeq: 1\r\nX-GS-ClientVersion: 14\r\nHost: 192.0.2.1\r\n\r\n";

fn decode(bytes: &[u8]) -> Result<Request, Error> {
    let mut decoder = Decoder::default();
    decoder.push(bytes)?;
    decoder.next_request()?.ok_or(Error::Truncated)
}

#[test]
fn client_shaped_requests_and_case_insensitive_headers() {
    let options = decode(OPTIONS).unwrap();
    assert_eq!(options.method, Method::Options);
    assert_eq!(options.cseq, 1);
    assert_eq!(options.header("X-GS-ClientVersion"), Some("14"));
    for (name, method, target) in [
        ("DESCRIBE", Method::Describe, "rtsp://192.0.2.1:48010"),
        ("SETUP", Method::Setup, "streamid=video/0/0"),
        ("ANNOUNCE", Method::Announce, "streamid=control/13/0"),
        ("PLAY", Method::Play, "/"),
        ("TEARDOWN", Method::Teardown, "/"),
    ] {
        let bytes = format!(
            "{name} {target} RTSP/1.0\r\ncSeQ: 2\r\nSession: opaque-session\r\nTransport: unicast;X-GS-ClientPort=50000-50001\r\n\r\n"
        );
        let request = decode(bytes.as_bytes()).unwrap();
        assert_eq!(request.method, method);
        assert_eq!(request.target, target);
    }
}

#[test]
fn every_fragment_boundary_and_coalesced_announce_preserve_exact_body() {
    let body = include_bytes!("../sdp/fixtures/moonlight-h264.sdp");
    let header = format!(
        "ANNOUNCE streamid=control/13/0 RTSP/1.0\r\nCSeq: 6\r\nContent-type: application/sdp\r\nContent-length: {}\r\nSession: test\r\n\r\n",
        body.len()
    );
    let mut bytes = header.into_bytes();
    bytes.extend_from_slice(body);
    for split in 0..bytes.len() {
        let mut decoder = Decoder::default();
        decoder.push(&bytes[..split]).unwrap();
        assert!(decoder.next_request().unwrap().is_none());
        decoder.push(&bytes[split..]).unwrap();
        decoder.push(OPTIONS).unwrap();
        let request = decoder.next_request().unwrap().unwrap();
        assert_eq!(request.body, body);
        assert_eq!(request.cseq, 6);
        assert!(crate::sdp::parse_announce(&request.body).is_ok());
        assert_eq!(
            decoder.next_request().unwrap().unwrap().method,
            Method::Options
        );
        assert!(decoder.next_request().unwrap().is_none());
        decoder.finish().unwrap();
    }
}

#[test]
fn bytewise_input_and_partial_eof_are_bounded() {
    let mut decoder = Decoder::default();
    for (index, byte) in OPTIONS.iter().enumerate() {
        decoder.push(&[*byte]).unwrap();
        assert_eq!(
            decoder.next_request().unwrap().is_some(),
            index == OPTIONS.len() - 1
        );
    }
    decoder
        .push(b"ANNOUNCE / RTSP/1.0\r\nCSeq: 1\r\nContent-Length: 4\r\n\r\nabc")
        .unwrap();
    assert!(decoder.next_request().unwrap().is_none());
    assert_eq!(decoder.finish(), Err(Error::Truncated));
    assert_eq!(decoder.push(OPTIONS), Err(Error::Poisoned));
    assert_eq!(decoder.buffered_bytes(), 0);
}

#[test]
fn ambiguous_framing_and_invalid_headers_are_rejected() {
    for headers in [
        "CSeq: 1\r\ncseq: 1",
        "CSeq: -1",
        "CSeq: +1",
        "CSeq: 4294967296",
        "CSeq: 1,2",
        "CSeq: 1\r\nContent-Length: 0\r\ncontent-length: 0",
        "CSeq: 1\r\nContent-Length: 1, 1",
        "CSeq: 1\r\nContent-Length: +1",
        "CSeq: 1\r\nTransfer-Encoding: chunked",
        "CSeq: 1\r\nContent-Encoding: gzip",
        "CSeq: 1\r\nTrailer: CSeq",
        "CSeq : 1",
        " CSeq: 1",
        "CSeq: 1\r\n folded",
        "CSeq: 1\r\nHost: a\r\nhost: b",
        "CSeq: 1\nHost: a",
        "CSeq: 1\r\nX: a\0b",
        "Host: a",
    ] {
        let bytes = format!("OPTIONS / RTSP/1.0\r\n{headers}\r\n\r\n");
        assert!(decode(bytes.as_bytes()).is_err(), "accepted {headers:?}");
    }
    for start in [
        "OPTIONS / RTSP/2.0",
        "OPTIONS  / RTSP/1.0",
        "OPTIONS / RTSP/1.0 extra",
        "GET / RTSP/1.0",
        "OPTIONS /\t RTSP/1.0",
    ] {
        assert!(decode(format!("{start}\r\nCSeq: 1\r\n\r\n").as_bytes()).is_err());
    }
}

#[test]
fn body_header_and_accumulation_limits_fail_before_unbounded_allocation() {
    let bytes = format!(
        "ANNOUNCE / RTSP/1.0\r\nCSeq: 1\r\nContent-Length: {}\r\n\r\n",
        MAX_BODY_BYTES + 1
    );
    assert_eq!(decode(bytes.as_bytes()), Err(Error::LimitExceeded));
    let mut decoder = Decoder::default();
    assert_eq!(
        decoder.push(&vec![b'x'; MAX_BUFFER_BYTES + 1]),
        Err(Error::LimitExceeded)
    );
    assert_eq!(decoder.buffered_bytes(), 0);
    let mut decoder = Decoder::default();
    decoder.push(&vec![b'x'; MAX_HEADER_BYTES]).unwrap();
    assert_eq!(decoder.next_request(), Err(Error::LimitExceeded));
    let headers = (0..MAX_HEADERS)
        .map(|i| format!("X-{i}: value\r\n"))
        .collect::<String>();
    assert_eq!(
        decode(format!("OPTIONS / RTSP/1.0\r\nCSeq: 1\r\n{headers}\r\n").as_bytes()),
        Err(Error::LimitExceeded)
    );
}

#[test]
fn serializer_controls_framing_and_rejects_injection() {
    let response =
        serialize_response(200, 42, &[("Content-Type", "application/sdp")], b"v=0\r\n").unwrap();
    assert_eq!(response, b"RTSP/1.0 200 OK\r\nCSeq: 42\r\nContent-Length: 5\r\nContent-Type: application/sdp\r\n\r\nv=0\r\n");
    for headers in [
        vec![("Cseq", "1")],
        vec![("Content-Length", "0")],
        vec![("Session", "a\r\nX: b")],
        vec![("Bad Name", "x")],
        vec![("Session", "a"), ("session", "b")],
    ] {
        assert!(serialize_response(200, 1, &headers, b"").is_err());
    }
    assert!(serialize_response(200, 1, &[], &vec![0; MAX_BODY_BYTES + 1]).is_err());
    assert!(serialize_response(999, 1, &[], b"").is_err());
}
