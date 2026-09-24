//! Strict HTTP/1.1 framing and browser boundary validation.
use crate::routes::{self, Route};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const MAX_HEADERS: usize = 8192;
pub const MAX_BODY: usize = 12_000;
pub const MAX_INPUT: usize = MAX_HEADERS + MAX_BODY;
pub const MAX_RESPONSE: usize = 24_000;

pub enum RequestTarget {
    Api(Route),
    Asset(String),
    /// A deliberately tiny loopback-only readiness surface. It has no
    /// credentials, identifiers, or mutable operations and exists solely for
    /// the installer to establish that the selected interactive worker is
    /// consuming its imported control-plane state.
    OperationalHealth,
}

pub struct Request {
    pub target: RequestTarget,
    pub payload: Value,
    pub bearer: Option<String>,
    pub origin: Option<String>,
    pub preflight: bool,
}

#[derive(Debug)]
pub struct Failure {
    pub status: u16,
    pub code: &'static str,
}

impl Failure {
    pub fn new(status: u16, code: &'static str) -> Self {
        Self { status, code }
    }
}

pub fn parse(
    bytes: &[u8],
    authority: &str,
    origins: &[String],
) -> Result<Option<Request>, Failure> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut request = httparse::Request::new(&mut headers);
    let header_end = match request
        .parse(bytes)
        .map_err(|_| Failure::new(400, "invalid_http"))?
    {
        httparse::Status::Partial => {
            if bytes.len() > MAX_HEADERS {
                return Err(Failure::new(431, "headers_too_large"));
            }
            return Ok(None);
        }
        httparse::Status::Complete(length) if length <= MAX_HEADERS => length,
        _ => return Err(Failure::new(431, "headers_too_large")),
    };
    let fields = Fields::read(request.headers)?;
    if request.version != Some(1) || fields.host != Some(authority) {
        return Err(Failure::new(400, "invalid_host_or_version"));
    }
    validate_origin(&fields, authority, origins)?;
    if request.method == Some("GET") {
        return asset_request(
            request.path.unwrap_or_default(),
            &fields,
            header_end,
            bytes.len(),
        )
        .map(Some);
    }
    let route = routes::resolve(request.path.unwrap_or_default())
        .ok_or(Failure::new(404, "route_not_found"))?;
    let preflight = validate_browser_request(&fields, request.method)?;
    let length = body_length(&fields, preflight)?;
    let expected = header_end + length;
    if bytes.len() < expected {
        return Ok(None);
    }
    if bytes.len() != expected {
        return Err(Failure::new(400, "pipelining_forbidden"));
    }
    build_request(route, &fields, preflight, &bytes[header_end..]).map(Some)
}

fn validate_origin(
    fields: &Fields<'_>,
    authority: &str,
    origins: &[String],
) -> Result<(), Failure> {
    let same_origin = format!("http://{authority}");
    if fields.origin.is_some_and(|origin| {
        origin != same_origin && !origins.iter().any(|allowed| allowed == origin)
    }) || (fields.fetch_site == Some("cross-site") && fields.origin.is_none())
    {
        return Err(Failure::new(403, "origin_denied"));
    }
    Ok(())
}

fn asset_request(
    path: &str,
    fields: &Fields<'_>,
    header_end: usize,
    input_length: usize,
) -> Result<Request, Failure> {
    if path == "/healthz" {
        if fields.content_length.is_some_and(|length| length != "0") || input_length != header_end {
            return Err(Failure::new(400, "asset_body_forbidden"));
        }
        return Ok(Request {
            target: RequestTarget::OperationalHealth,
            payload: Value::Null,
            bearer: None,
            origin: fields.origin.map(str::to_owned),
            preflight: false,
        });
    }
    if path.starts_with("/v1/") {
        return Err(Failure::new(405, "method_not_allowed"));
    }
    if !path.starts_with('/')
        || path.len() > 512
        || path.contains(['?', '#', '\\', '%'])
        || path.split('/').any(|part| part == "." || part == "..")
    {
        return Err(Failure::new(404, "asset_not_found"));
    }
    if fields.content_length.is_some_and(|length| length != "0") || input_length != header_end {
        return Err(Failure::new(400, "asset_body_forbidden"));
    }
    Ok(Request {
        target: RequestTarget::Asset(path.into()),
        payload: Value::Null,
        bearer: None,
        origin: fields.origin.map(str::to_owned),
        preflight: false,
    })
}

fn validate_browser_request(fields: &Fields<'_>, method: Option<&str>) -> Result<bool, Failure> {
    let preflight = method == Some("OPTIONS");
    if !preflight && method != Some("POST") {
        return Err(Failure::new(405, "method_not_allowed"));
    }
    if preflight {
        if fields.origin.is_none()
            || fields.request_method != Some("POST")
            || fields.request_headers.is_some_and(|headers| {
                headers.split(',').any(|name| {
                    !matches!(
                        name.trim().to_ascii_lowercase().as_str(),
                        "authorization" | "content-type"
                    )
                })
            })
        {
            return Err(Failure::new(403, "preflight_denied"));
        }
    } else if fields.content_type != Some("application/json") {
        return Err(Failure::new(415, "json_required"));
    }
    Ok(preflight)
}

fn body_length(fields: &Fields<'_>, preflight: bool) -> Result<usize, Failure> {
    let length = match fields.content_length {
        Some(length) if !length.is_empty() && length.bytes().all(|byte| byte.is_ascii_digit()) => {
            length
                .parse::<usize>()
                .map_err(|_| Failure::new(400, "invalid_content_length"))?
        }
        None if preflight => 0,
        _ => return Err(Failure::new(411, "content_length_required")),
    };
    if length > MAX_BODY {
        return Err(Failure::new(413, "body_too_large"));
    }
    if preflight && length != 0 {
        return Err(Failure::new(400, "preflight_body_forbidden"));
    }
    Ok(length)
}

fn build_request(
    route: Route,
    fields: &Fields<'_>,
    preflight: bool,
    body: &[u8],
) -> Result<Request, Failure> {
    let bearer = if !preflight && route.scope.is_some() {
        let bearer = fields
            .authorization
            .and_then(|value| value.strip_prefix("Bearer "))
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 128
                    && value.bytes().all(|byte| byte.is_ascii_graphic())
            })
            .ok_or(Failure::new(401, "unauthorized"))?;
        Some(bearer.to_owned())
    } else {
        None
    };
    let payload = if preflight {
        Value::Null
    } else {
        routes::payload(route, body).map_err(|_| Failure::new(400, "invalid_json_body"))?
    };
    Ok(Request {
        target: RequestTarget::Api(route),
        payload,
        bearer,
        origin: fields.origin.map(str::to_owned),
        preflight,
    })
}

#[derive(Default)]
struct Fields<'a> {
    host: Option<&'a str>,
    content_length: Option<&'a str>,
    content_type: Option<&'a str>,
    authorization: Option<&'a str>,
    origin: Option<&'a str>,
    fetch_site: Option<&'a str>,
    request_method: Option<&'a str>,
    request_headers: Option<&'a str>,
}

impl<'a> Fields<'a> {
    fn read(headers: &'a [httparse::Header<'a>]) -> Result<Self, Failure> {
        let mut fields = Self::default();
        let mut names = BTreeSet::new();
        for header in headers {
            let name = header.name.to_ascii_lowercase();
            if !names.insert(name.clone()) {
                return Err(Failure::new(400, "duplicate_header"));
            }
            let value = std::str::from_utf8(header.value)
                .map_err(|_| Failure::new(400, "invalid_header"))?;
            match name.as_str() {
                "host" => fields.host = Some(value),
                "content-length" => fields.content_length = Some(value),
                "content-type" => fields.content_type = Some(value),
                "authorization" => fields.authorization = Some(value),
                "origin" => fields.origin = Some(value),
                "sec-fetch-site" => fields.fetch_site = Some(value),
                "access-control-request-method" => fields.request_method = Some(value),
                "access-control-request-headers" => fields.request_headers = Some(value),
                "transfer-encoding" | "expect" | "upgrade" => {
                    return Err(Failure::new(400, "unsupported_framing"));
                }
                _ => {}
            }
        }
        Ok(fields)
    }
}

pub fn valid_origin(origin: &str) -> bool {
    let authority = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"));
    origin.len() <= 256
        && authority.is_some_and(|value| {
            !value.is_empty()
                && value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'[' | b']' | b'-')
                })
        })
}

pub fn response(status: u16, payload: &Value, origin: Option<&str>) -> Vec<u8> {
    let body = serde_json::to_vec(payload).unwrap_or_default();
    if body.len() > MAX_RESPONSE {
        return response(
            502,
            &json!({"ok":false,"code":"response_too_large"}),
            origin,
        );
    }
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Bad Gateway",
    };
    let body = if status == 204 {
        &[][..]
    } else {
        body.as_slice()
    };
    let mut header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n",
        body.len()
    );
    if let Some(origin) = origin {
        header.push_str(&format!("Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\nAccess-Control-Allow-Methods: POST\r\nAccess-Control-Allow-Headers: authorization, content-type\r\n"));
    }
    header.push_str("\r\n");
    let mut bytes = header.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    const HOST: &str = "127.0.0.1:1234";

    fn wire(extra: &str, body: &str) -> Vec<u8> {
        format!("POST /v1/catalog/list HTTP/1.1\r\nHost: {HOST}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}\r\n{body}", body.len()).into_bytes()
    }

    #[test]
    fn pairing_and_display_remain_protected_post_routes() {
        for path in [
            "/v1/pairing/pending",
            "/v1/pairing/approve_pin",
            "/v1/display/status",
            "/v1/display/apply",
        ] {
            let request = format!(
                "POST {path} HTTP/1.1\r\nHost: {HOST}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nOrigin: http://{HOST}\r\n\r\n{{}}"
            );
            assert_eq!(
                parse(request.as_bytes(), HOST, &[]).err().unwrap().status,
                401
            );
        }
        let path = "/v1/pairing/approve_pin";
        let body = r#"{"pairing_id":"test-id","pin":"0123"}"#;
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: {HOST}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAuthorization: Bearer token\r\n\r\n{body}",
            body.len()
        );
        let parsed = parse(request.as_bytes(), HOST, &[]).unwrap().unwrap();
        assert!(matches!(
            parsed.target,
            RequestTarget::Api(Route {
                scope: Some("pairing.admin"),
                ..
            })
        ));
        assert_eq!(parsed.payload["action"], "approve_pin");
        assert_eq!(parsed.payload["pin"], "0123");
    }

    #[test]
    fn auth_refresh_and_logout_require_body_secrets_without_access_headers() {
        let token = format!("ama_{}", "a".repeat(64));
        for (path, body) in [
            (
                "/v1/auth/login",
                r#"{"username":"admin","password":"correct horse battery staple"}"#.to_owned(),
            ),
            (
                "/v1/auth/refresh",
                serde_json::json!({"refresh_token":token.clone()}).to_string(),
            ),
            (
                "/v1/auth/logout",
                serde_json::json!({"refresh_token":token.clone()}).to_string(),
            ),
        ] {
            let request = format!(
                "POST {path} HTTP/1.1\r\nHost: {HOST}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let parsed = parse(request.as_bytes(), HOST, &[]).unwrap().unwrap();
            assert!(parsed.bearer.is_none());
            assert_eq!(parsed.payload["action"], path.rsplit('/').next().unwrap());
            assert!(matches!(
                parsed.target,
                RequestTarget::Api(Route { scope: None, .. })
            ));
        }
    }

    #[test]
    fn static_gets_are_bodyless_public_targets_not_api_commands() {
        for path in ["/", "/app.js", "/styles.css", "/web/0123456789/app.js"] {
            let bytes = format!("GET {path} HTTP/1.1\r\nHost: {HOST}\r\n\r\n");
            let request = parse(bytes.as_bytes(), HOST, &[]).unwrap().unwrap();
            assert!(matches!(request.target, RequestTarget::Asset(ref actual) if actual == path));
            assert!(request.bearer.is_none());
            assert!(request.payload.is_null());
        }
        for (path, header, body, expected) in [
            ("/app.js", "Content-Length: 1\r\n", "x", 400),
            ("/app.js", "Content-Length: 0\r\n", "x", 400),
            ("/../private", "", "", 404),
            ("/%2e%2e/private", "", "", 404),
            ("/v1/catalog/list", "", "", 405),
        ] {
            let bytes = format!("GET {path} HTTP/1.1\r\nHost: {HOST}\r\n{header}\r\n{body}");
            assert_eq!(
                parse(bytes.as_bytes(), HOST, &[]).err().unwrap().status,
                expected
            );
        }
    }

    #[test]
    fn operational_health_is_a_fixed_bodyless_get_not_an_asset_or_api_route() {
        let request = format!("GET /healthz HTTP/1.1\r\nHost: {HOST}\r\n\r\n");
        let parsed = parse(request.as_bytes(), HOST, &[]).unwrap().unwrap();
        assert!(matches!(parsed.target, RequestTarget::OperationalHealth));
        assert!(parsed.bearer.is_none());
        assert!(parsed.payload.is_null());
        let query = format!("GET /healthz?x=1 HTTP/1.1\r\nHost: {HOST}\r\n\r\n");
        assert_eq!(
            parse(query.as_bytes(), HOST, &[]).err().unwrap().status,
            404
        );
        for path in ["/healthz/extra", "/healthz-extra"] {
            let request = format!("GET {path} HTTP/1.1\r\nHost: {HOST}\r\n\r\n");
            let parsed = parse(request.as_bytes(), HOST, &[]).unwrap().unwrap();
            assert!(matches!(parsed.target, RequestTarget::Asset(_)));
        }
    }

    #[test]
    fn exact_bound_origin_can_use_api_without_cross_origin_allowlist() {
        let bytes = wire(
            &format!("Authorization: Bearer secret\r\nOrigin: http://{HOST}\r\n"),
            "{}",
        );
        assert!(matches!(
            parse(&bytes, HOST, &[]).unwrap().unwrap().target,
            RequestTarget::Api(_)
        ));
        let wrong_port = wire(
            "Authorization: Bearer secret\r\nOrigin: http://127.0.0.1:9999\r\n",
            "{}",
        );
        assert_eq!(parse(&wrong_port, HOST, &[]).err().unwrap().status, 403);
        let missing_auth = wire(&format!("Origin: http://{HOST}\r\n"), "{}");
        assert_eq!(parse(&missing_auth, HOST, &[]).err().unwrap().status, 401);
    }

    #[test]
    fn partial_input_waits_then_constructs_only_route_action() {
        let bytes = wire("Authorization: Bearer ama_test\r\n", "{}");
        assert!(
            parse(&bytes[..bytes.len() - 1], HOST, &[])
                .unwrap()
                .is_none()
        );
        let request = parse(&bytes, HOST, &[]).unwrap().unwrap();
        assert_eq!(request.payload, json!({"action":"list"}));
        assert!(matches!(
            request.target,
            RequestTarget::Api(Route {
                scope: Some("catalog.read"),
                ..
            })
        ));
        assert!(
            parse(
                &wire(
                    "Authorization: Bearer ama_test\r\n",
                    "{\"action\":\"delete\"}"
                ),
                HOST,
                &[]
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_ambiguous_framing_and_missing_bearer() {
        for extra in [
            "Content-Length: 2\r\n",
            "Transfer-Encoding: chunked\r\n",
            "Expect: 100-continue\r\n",
            "Host: evil.test\r\n",
        ] {
            assert_eq!(
                parse(&wire(extra, "{}"), HOST, &[]).err().unwrap().status,
                400
            );
        }
        assert_eq!(parse(&wire("", "{}"), HOST, &[]).err().unwrap().status, 401);
        let mut pipelined = wire("Authorization: Bearer secret\r\n", "{}");
        pipelined.extend_from_slice(b"GET /");
        assert_eq!(parse(&pipelined, HOST, &[]).err().unwrap().status, 400);
    }

    #[test]
    fn rejects_dns_rebinding_and_unapproved_browser_origins() {
        let origins = vec!["http://127.0.0.1:8000".into()];
        let approved = wire(
            "Authorization: Bearer secret\r\nOrigin: http://127.0.0.1:8000\r\n",
            "{}",
        );
        assert!(parse(&approved, HOST, &origins).is_ok());
        assert_eq!(parse(&approved, HOST, &[]).err().unwrap().status, 403);
        let rebound = String::from_utf8(approved)
            .unwrap()
            .replace(HOST, "evil.test:1234");
        assert_eq!(
            parse(rebound.as_bytes(), HOST, &origins)
                .err()
                .unwrap()
                .status,
            400
        );
        assert!(!valid_origin("http://site/path"));
        assert!(!valid_origin("*"));
    }

    #[test]
    fn preflight_is_explicit_and_never_dispatches_product_payload() {
        let wire = format!(
            "OPTIONS /v1/catalog/create HTTP/1.1\r\nHost: {HOST}\r\nOrigin: http://localhost:8000\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: authorization, content-type\r\n\r\n"
        );
        let request = parse(wire.as_bytes(), HOST, &["http://localhost:8000".into()])
            .unwrap()
            .unwrap();
        assert!(request.preflight);
        assert!(request.payload.is_null());
        assert!(request.bearer.is_none());
    }

    #[test]
    fn bodies_headers_and_responses_have_hard_byte_limits() {
        let oversized = format!(
            "POST /v1/auth/login HTTP/1.1\r\nHost: {HOST}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert_eq!(
            parse(oversized.as_bytes(), HOST, &[]).err().unwrap().status,
            413
        );
        assert!(parse(&vec![b'x'; MAX_HEADERS + 1], HOST, &[]).is_err());
        let reply = response(200, &json!({"huge":"x".repeat(MAX_RESPONSE)}), None);
        assert!(reply.starts_with(b"HTTP/1.1 502"));
        assert!(reply.len() < 1024);
    }
}
