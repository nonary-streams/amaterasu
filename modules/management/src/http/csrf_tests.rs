use super::*;

const HOST: &str = "127.0.0.1:47990";
const EXCEPTION: &str = "https://trusted.example:47990";

fn request(path: &str, origin: Option<&str>, headers: &str) -> Vec<u8> {
    let origin = origin.map_or(String::new(), |origin| format!("Origin: {origin}\r\n"));
    format!("POST {path} HTTP/1.1\r\nHost: {HOST}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n{origin}{headers}\r\n{{}}").into_bytes()
}

#[test]
fn exception_only_relaxes_csrf_and_keeps_cors_transport_and_credentials() {
    let exceptions: CsrfOrigins = serde_json::from_value(json!([EXCEPTION])).unwrap();
    let token = format!("ama_{}", "a".repeat(64));
    let cookie = format!("Cookie: __Secure-amaterasu-access={token}\r\n");
    let bytes = request("/v1/catalog/list", Some(EXCEPTION), &cookie);
    let cors = vec![EXCEPTION.to_owned()];
    let parsed = parse_policy_transport(&bytes, HOST, &cors, Transport::Tls, &exceptions)
        .unwrap()
        .unwrap();
    assert_eq!(parsed.bearer.as_deref(), Some(token.as_str()));
    assert!(parsed.csrf_exception);
    assert!(matches!(
        parsed.target,
        RequestTarget::Api(Route {
            scope: Some("catalog.read"),
            ..
        })
    ));
    assert_eq!(
        parse_policy_transport(&bytes, HOST, &[], Transport::Tls, &exceptions)
            .err()
            .unwrap()
            .code,
        "origin_denied"
    );
    assert_eq!(
        parse_policy_transport(&bytes, HOST, &cors, Transport::Plaintext, &exceptions)
            .err()
            .unwrap()
            .status,
        401
    );
    assert_eq!(
        parse_policy_transport(
            &request("/v1/catalog/list", Some(EXCEPTION), ""),
            HOST,
            &cors,
            Transport::Tls,
            &exceptions
        )
        .err()
        .unwrap()
        .status,
        401
    );
    assert_eq!(
        parse_policy_transport(
            &request(
                "/v1/catalog/list",
                Some(EXCEPTION),
                &(cookie + "Authorization: junk\r\n")
            ),
            HOST,
            &cors,
            Transport::Tls,
            &exceptions
        )
        .err()
        .unwrap()
        .status,
        401
    );
}

#[test]
fn exception_never_matches_missing_origin_referer_prefix_path_or_other_port() {
    let exceptions: CsrfOrigins = serde_json::from_value(json!([EXCEPTION])).unwrap();
    let token = format!("ama_{}", "a".repeat(64));
    for origin in [
        None,
        Some("https://trusted.example"),
        Some("https://trusted.example:47991"),
        Some("https://trusted.example:47990/path"),
        Some("https://trusted.example:47990.evil"),
    ] {
        let bytes = request(
            "/v1/auth/browser/session",
            origin,
            &format!("Cookie: __Secure-amaterasu-access={token}\r\nReferer: {EXCEPTION}/page\r\n"),
        );
        let cors = origin.map_or(Vec::new(), |origin| vec![origin.to_owned()]);
        assert_eq!(
            parse_policy_transport(&bytes, HOST, &cors, Transport::Tls, &exceptions)
                .err()
                .unwrap()
                .code,
            "csrf_denied"
        );
    }
    let bytes = request(
        "/v1/auth/browser/session",
        Some(EXCEPTION),
        &format!("Cookie: __Secure-amaterasu-access={token}\r\n"),
    );
    assert!(
        parse_policy_transport(
            &bytes,
            HOST,
            &[EXCEPTION.to_owned()],
            Transport::Tls,
            &exceptions
        )
        .unwrap()
        .unwrap()
        .csrf_exception
    );
    assert_eq!(
        parse_policy_transport(
            &bytes,
            HOST,
            &[EXCEPTION.to_owned()],
            Transport::Tls,
            &CsrfOrigins::default()
        )
        .err()
        .unwrap()
        .code,
        "csrf_denied"
    );
}

#[test]
fn ordinary_same_origin_header_path_remains_active_without_exceptions() {
    let token = format!("ama_{}", "a".repeat(64));
    let bytes = request(
        "/v1/catalog/list",
        Some(&format!("https://{HOST}")),
        &format!("Cookie: __Secure-amaterasu-access={token}\r\nX-Amaterasu-CSRF: 1\r\n"),
    );
    let parsed = parse_policy_transport(&bytes, HOST, &[], Transport::Tls, &CsrfOrigins::default())
        .unwrap()
        .unwrap();
    assert!(!parsed.csrf_exception);
}
