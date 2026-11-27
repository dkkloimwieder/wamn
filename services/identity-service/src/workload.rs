//! Workload identity from the Kubernetes TokenReview API.

use hyper::{HeaderMap, header};

pub(super) fn bearer(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let (scheme, token) = values.next()?.to_str().ok()?.split_once(' ')?;
    (values.next().is_none()
        && scheme.eq_ignore_ascii_case("Bearer")
        && !token.is_empty()
        && !token.bytes().any(|byte| byte.is_ascii_whitespace()))
    .then_some(token)
}
