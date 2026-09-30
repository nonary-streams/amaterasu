//! Independent security checks for the explicit cookie CSRF exception boundary.
use crate::csrf_policy::CsrfOrigins;
use crate::http::{self, BrowserAuth, RequestTarget, Transport};
use serde_json::json;

const HOST: &str = "127.0.0.1:1234";
const TRUSTED: &str = "https://trusted.example:47990";

fn origins() -> CsrfOrigins {
    serde_json::from_value(json!([TRUSTED])).unwrap()
}

fn token() -> String {
    format!("ama_{}", "a".repeat(64))
}

fn request(path: &str, extra: &str, body: &str) -> Vec<u8> {
    format!(
        "POST {path} HTTP/1.1\r\nHost: {HOST}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn parsed(
    bytes: &[u8],
    cors: &[String],
    transport: Transport,
) -> Result<http::Request, http::Failure> {
    http::parse_policy_transport(bytes, HOST, cors, transport, &origins())
        .map(|request| request.expect("a complete bounded request was supplied"))
}

#[test]
fn explicit_exception_cannot_supply_missing_or_malformed_credentials() {
    let cors = vec![TRUSTED.to_owned()];
    let access = token();
    for credentials in [
        String::new(),
        "Cookie: __Secure-amaterasu-access=invalid\r\n".to_owned(),
        format!(
            "Cookie: __Secure-amaterasu-access={access}; __Secure-amaterasu-access={access}\r\n"
        ),
        format!("Cookie: __Secure-amaterasu-access={access}\r\nAuthorization: invalid\r\n"),
    ] {
        let bytes = request(
            "/v1/management_settings/get",
            &format!("Origin: {TRUSTED}\r\n{credentials}"),
            "{}",
        );
        assert_eq!(
            parsed(&bytes, &cors, Transport::Tls).err().unwrap().status,
            401
        );
    }
    let cookie = request(
        "/v1/management_settings/get",
        &format!("Origin: {TRUSTED}\r\nCookie: __Secure-amaterasu-access={access}\r\n"),
        "{}",
    );
    let cookie = parsed(&cookie, &cors, Transport::Tls).unwrap();
    assert_eq!(cookie.bearer.as_deref(), Some(access.as_str()));
    assert!(cookie.csrf_exception);
    assert!(matches!(
        cookie.target,
        RequestTarget::Api(route) if route.scope == Some("configuration.read")
    ));

    let bearer = request(
        "/v1/management_settings/get",
        &format!(
            "Origin: {TRUSTED}\r\nAuthorization: Bearer explicit-token\r\nCookie: __Secure-amaterasu-access=invalid\r\n"
        ),
        "{}",
    );
    let bearer = parsed(&bearer, &cors, Transport::Tls).unwrap();
    assert_eq!(bearer.bearer.as_deref(), Some("explicit-token"));
    assert!(!bearer.csrf_exception);
}

#[test]
fn exception_requires_the_exact_origin_without_referer_or_query_fallbacks() {
    let access = token();
    for origin in [
        "https://trusted.example",
        "https://trusted.example:47991",
        "https://trusted.example:47990/path",
        "https://trusted.example:47990.evil",
        "https://trusted.example.evil:47990",
        "https://TRUSTED.example:47990",
    ] {
        let bytes = request(
            "/v1/management_settings/get",
            &format!("Origin: {origin}\r\nCookie: __Secure-amaterasu-access={access}\r\n"),
            "{}",
        );
        let failure = parsed(&bytes, &[origin.to_owned()], Transport::Tls)
            .err()
            .unwrap();
        assert_eq!(failure.code, "csrf_denied", "{origin}");
    }
    for extra in [
        format!("Referer: {TRUSTED}/settings\r\n"),
        "X-Amaterasu-CSRF: 1\r\n".to_owned(),
        format!("Referer: {TRUSTED}/settings\r\nX-CSRF-Token: minted-token\r\n"),
    ] {
        let bytes = request(
            "/v1/management_settings/get",
            &format!("{extra}Cookie: __Secure-amaterasu-access={access}\r\n"),
            "{}",
        );
        assert_eq!(
            parsed(&bytes, &[], Transport::Tls).err().unwrap().code,
            "csrf_denied"
        );
    }
    let query = request(
        "/v1/management_settings/get?csrf_token=minted-token",
        &format!("Referer: {TRUSTED}/settings\r\nCookie: __Secure-amaterasu-access={access}\r\n"),
        "{}",
    );
    assert_eq!(
        parsed(&query, &[], Transport::Tls).err().unwrap().code,
        "route_not_found"
    );
}

#[test]
fn exception_does_not_relax_host_cors_transport_or_duplicate_headers() {
    let access = token();
    let extra = format!("Origin: {TRUSTED}\r\nCookie: __Secure-amaterasu-access={access}\r\n");
    let valid = request("/v1/management_settings/get", &extra, "{}");
    assert_eq!(
        parsed(&valid, &[], Transport::Tls).err().unwrap().code,
        "origin_denied"
    );
    assert_eq!(
        parsed(&valid, &[TRUSTED.to_owned()], Transport::Plaintext)
            .err()
            .unwrap()
            .code,
        "unauthorized"
    );
    let wrong_host = String::from_utf8(valid.clone())
        .unwrap()
        .replace(HOST, "attacker.invalid");
    assert_eq!(
        parsed(wrong_host.as_bytes(), &[TRUSTED.to_owned()], Transport::Tls)
            .err()
            .unwrap()
            .code,
        "invalid_host_or_version"
    );
    for duplicated in [
        format!("origin: {TRUSTED}\r\n"),
        format!("COOKIE: __Secure-amaterasu-access={access}\r\n"),
        format!("host: {HOST}\r\n"),
        "X-Amaterasu-CSRF: 1\r\nx-amaterasu-csrf: 0\r\n".to_owned(),
    ] {
        let bytes = request(
            "/v1/management_settings/get",
            &format!("{extra}{duplicated}"),
            "{}",
        );
        assert_eq!(
            parsed(&bytes, &[TRUSTED.to_owned()], Transport::Tls)
                .err()
                .unwrap()
                .code,
            "duplicate_header"
        );
    }
}

#[test]
fn browser_authentication_keeps_tls_and_refresh_credentials_mandatory() {
    let cors = vec![TRUSTED.to_owned()];
    let access = token();
    let extra = format!(
        "Origin: {TRUSTED}\r\nCookie: __Secure-amaterasu-refresh={access}; __Secure-amaterasu-access={access}\r\n"
    );
    for path in ["refresh", "session", "logout"] {
        let bytes = request(&format!("/v1/auth/browser/{path}"), &extra, "{}");
        assert_eq!(
            parsed(&bytes, &cors, Transport::Plaintext)
                .err()
                .unwrap()
                .code,
            "route_not_found"
        );
    }
    for path in ["refresh", "session"] {
        let bytes = request(
            &format!("/v1/auth/browser/{path}"),
            &format!("Origin: {TRUSTED}\r\n"),
            "{}",
        );
        assert_eq!(
            parsed(&bytes, &cors, Transport::Tls).err().unwrap().code,
            "unauthorized"
        );
    }
    let refresh = request("/v1/auth/browser/refresh", &extra, "{}");
    let refresh = parsed(&refresh, &cors, Transport::Tls).unwrap();
    assert!(matches!(
        refresh.target,
        RequestTarget::BrowserAuth(BrowserAuth::Refresh)
    ));
    assert_eq!(
        refresh.payload,
        json!({"action":"refresh", "refresh_token":access})
    );
}

#[test]
fn exception_does_not_expand_preflight_or_credentialed_cors_responses() {
    let cors = vec![TRUSTED.to_owned()];
    for path in ["/v1/auth/browser/refresh", "/v1/management_settings/get"] {
        let bytes = format!(
            "OPTIONS {path} HTTP/1.1\r\nHost: {HOST}\r\nOrigin: {TRUSTED}\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: x-amaterasu-csrf,content-type\r\n\r\n"
        );
        assert_eq!(
            parsed(bytes.as_bytes(), &cors, Transport::Tls)
                .err()
                .unwrap()
                .code,
            "preflight_denied"
        );
    }
    let browser = format!(
        "OPTIONS /v1/auth/browser/refresh HTTP/1.1\r\nHost: {HOST}\r\nOrigin: {TRUSTED}\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: content-type\r\n\r\n"
    );
    assert_eq!(
        parsed(browser.as_bytes(), &cors, Transport::Tls)
            .err()
            .unwrap()
            .code,
        "preflight_denied"
    );
    let response =
        String::from_utf8(http::response(200, &json!({"ok":true}), Some(TRUSTED))).unwrap();
    assert!(
        !response
            .to_ascii_lowercase()
            .contains("access-control-allow-credentials")
    );
}

#[test]
fn csrf_policy_route_rejects_type_confusion_and_authority_fields() {
    let route = crate::routes::resolve("/v1/management_settings/update").unwrap();
    for payload in [
        json!({"expected_revision":0,"csrf_allowed_origins":true}),
        json!({"expected_revision":0,"csrf_allowed_origins":[7]}),
        json!({"expected_revision":0,"csrf_allowed_origins":[null]}),
        json!({"expected_revision":0,"csrf_allowed_origins":[{"origin":TRUSTED}]}),
        json!({"expected_revision":0,"csrf_allowed_origins":vec![TRUSTED;17]}),
        json!({"expected_revision":0,"csrf_allowed_origins":[TRUSTED],"caller":"amaterasu.management"}),
        json!({"expected_revision":0,"csrf_allowed_origins":[TRUSTED],"scopes":["configuration.write"]}),
        json!({"expected_revision":0,"values":{"csrf_allowed_origins":[TRUSTED]}}),
        json!({"expected_revision":-1,"csrf_allowed_origins":[TRUSTED]}),
        json!({"expected_revision":0.5,"csrf_allowed_origins":[TRUSTED]}),
    ] {
        let bytes = serde_json::to_vec(&payload).unwrap();
        assert!(crate::routes::payload(route, &bytes).is_err(), "{payload}");
    }
}
