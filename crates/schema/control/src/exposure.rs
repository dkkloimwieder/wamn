//! Pure route normalization for callable-flow exposure.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A normalized HTTP route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct HttpRoute {
    pub path: String,
    pub method: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposureError {
    pub code: &'static str,
    pub subject: String,
}

impl fmt::Display for ExposureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.subject)
    }
}

impl std::error::Error for ExposureError {}

/// Validate and normalize an authored route without adding deployment identity.
pub fn normalize_http_route(route: &HttpRoute, subject: &str) -> Result<HttpRoute, ExposureError> {
    let method = route.method.to_ascii_uppercase();
    if method.is_empty() || !method.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(error("invalid-http-method", subject));
    }
    let path =
        normalize_path(&route.path).ok_or_else(|| error("invalid-http-path-template", subject))?;
    Ok(HttpRoute { path, method })
}

fn normalize_path(path: &str) -> Option<String> {
    if !path.starts_with('/') || path.contains("//") {
        return None;
    }
    let mut catch_all = false;
    for (index, segment) in path.trim_end_matches('/').split('/').skip(1).enumerate() {
        if catch_all || segment.is_empty() {
            return None;
        }
        if segment.starts_with('{') {
            if !segment.ends_with('}') || segment.len() < 3 {
                return None;
            }
            catch_all = segment.starts_with("{*");
            if catch_all && index + 1 != path.trim_end_matches('/').split('/').skip(1).count() {
                return None;
            }
        } else if segment.contains('{') || segment.contains('}') {
            return None;
        }
    }
    let normalized = path.trim_end_matches('/');
    Some(
        if normalized.is_empty() {
            "/"
        } else {
            normalized
        }
        .to_string(),
    )
}

/// Collapse authored parameter names into the runtime route-collision key.
pub fn canonical_http_route_template(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            if segment.starts_with("{*") {
                "{*}"
            } else if segment.starts_with('{') {
                "{}"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn error(code: &'static str, subject: &str) -> ExposureError {
    ExposureError {
        code,
        subject: subject.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(path: &str, method: &str) -> HttpRoute {
        HttpRoute {
            path: path.into(),
            method: method.into(),
        }
    }

    #[test]
    fn normalization_uppercases_the_method_and_trims_the_trailing_slash() {
        let normalized = normalize_http_route(&route("/widgets/", "post"), "create").unwrap();
        assert_eq!(normalized.path, "/widgets");
        assert_eq!(normalized.method, "POST");
    }

    #[test]
    fn a_hostname_cannot_reach_an_authored_route() {
        let authored = serde_json::json!({
            "host": "package.example",
            "path": "/widgets",
            "method": "POST"
        });
        serde_json::from_value::<HttpRoute>(authored)
            .expect_err("package exposure cannot deserialize deployment hostname data");
    }

    #[test]
    fn a_malformed_method_or_path_is_refused_by_name() {
        assert_eq!(
            normalize_http_route(&route("/widgets", "po st"), "create")
                .unwrap_err()
                .code,
            "invalid-http-method"
        );
        assert_eq!(
            normalize_http_route(&route("widgets", "POST"), "create")
                .unwrap_err()
                .code,
            "invalid-http-path-template"
        );
    }

    #[test]
    fn the_collision_key_drops_parameter_names() {
        assert_eq!(canonical_http_route_template("/{id}"), "/{}");
        assert_eq!(
            canonical_http_route_template("/{widget}"),
            canonical_http_route_template("/{id}")
        );
        assert_eq!(
            canonical_http_route_template("/files/{*rest}"),
            "/files/{*}"
        );
    }
}
