//! Disposable P2 guest: standard WASI HTTP only, with no WAMN connection import.

use wasi::http::types::{ErrorCode, Fields, Method, OutgoingRequest, Scheme};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(
        args.len(),
        7,
        "scheme authority path content-type authorization traceparent"
    );
    let fields = Fields::new();
    for (name, value) in [
        ("content-type", &args[4]),
        ("authorization", &args[5]),
        ("traceparent", &args[6]),
    ] {
        if !value.is_empty() {
            fields
                .set(&name.to_owned(), &[value.as_bytes().to_vec()])
                .unwrap();
        }
    }
    // A separate guest-owned Fields resource must never acquire host-injected
    // credentials. The consumed outgoing request itself is no longer readable.
    let visible = fields.clone();
    let before = visible.entries();
    let request = OutgoingRequest::new(fields);
    request.set_method(&Method::Post).unwrap();
    request
        .set_scheme(Some(&match args[1].as_str() {
            "http" => Scheme::Http,
            "https" => Scheme::Https,
            other => Scheme::Other(other.to_owned()),
        }))
        .unwrap();
    request.set_authority(Some(&args[2])).unwrap();
    request.set_path_with_query(Some(&args[3])).unwrap();
    let (status, error) = match wasi::http::outgoing_handler::handle(request, None) {
        Ok(pending) => {
            pending.subscribe().block();
            match pending
                .get()
                .expect("response ready")
                .expect("response not consumed")
            {
                Ok(response) => (i32::from(response.status()), None),
                Err(error) => (-1, Some(error)),
            }
        }
        Err(error) => (-1, Some(error)),
    };
    println!(
        "{}",
        serde_json::json!({
            "status": status,
            "request_denied": matches!(error, Some(ErrorCode::HttpRequestDenied)),
            "headers_unchanged": visible.entries() == before,
            "authorization_visible": !visible.get(&"authorization".to_owned()).is_empty(),
            "host_environment_visible": std::env::var_os("WAMN_CTC8_14_SECRET").is_some(),
        })
    );
}
