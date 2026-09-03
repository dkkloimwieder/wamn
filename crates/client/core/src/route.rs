//! Route construction from supplied route metadata.
//!
//! **No deployment host, base URL or path is ever baked into generated code**
//! (spec ruling 3). A generated client names an OPERATION; the route it
//! resolves to is metadata this layer is given, because the same operation
//! sits at a different path in a different deployment and generated code that
//! knew the path would be wrong the first time it moved.
//!
//! Parameter segments follow the platform's authored form — `{name}` for one
//! segment, `{*name}` for a trailing capture — the same shape
//! `canonical_http_route_template` collapses when it checks for collisions.

use std::collections::BTreeMap;

/// Why a route could not be constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    /// The template names a parameter the caller did not supply.
    MissingParameter {
        /// The unsupplied parameter.
        name: String,
    },
    /// The caller supplied a parameter the template does not name. Refused
    /// rather than ignored: a silently dropped argument is a request that did
    /// not do what its caller asked.
    UnknownParameter {
        /// The parameter with nowhere to go.
        name: String,
    },
    /// A supplied value would change the route's shape rather than fill a
    /// segment.
    UnsafeValue {
        /// The offending parameter.
        name: String,
    },
}

impl RouteError {
    /// Stable wire code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingParameter { .. } => "route_missing_parameter",
            Self::UnknownParameter { .. } => "route_unknown_parameter",
            Self::UnsafeValue { .. } => "route_unsafe_value",
        }
    }
}

impl core::fmt::Display for RouteError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingParameter { name } => {
                write!(formatter, "route template needs a value for {name:?}")
            }
            Self::UnknownParameter { name } => write!(
                formatter,
                "route template has no segment for {name:?}; a dropped argument would send a \
                 request the caller did not ask for"
            ),
            Self::UnsafeValue { name } => write!(
                formatter,
                "value for {name:?} would change the route's shape rather than fill a segment"
            ),
        }
    }
}

impl std::error::Error for RouteError {}

/// One published route, as the deployment describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteMetadata {
    /// HTTP method, e.g. `POST`.
    pub method: String,
    /// Authored path template, e.g. `/purchase_order/{id}`.
    pub template: String,
    /// Host header the deployment routes on, when it routes by host.
    pub host: Option<String>,
}

impl RouteMetadata {
    /// Build the concrete path for one call.
    ///
    /// # Errors
    ///
    /// [`RouteError`] naming the parameter at fault.
    pub fn path(&self, parameters: &BTreeMap<String, String>) -> Result<String, RouteError> {
        let mut used = 0usize;
        let mut segments = Vec::new();
        for segment in self.template.split('/') {
            let name = segment
                .strip_prefix("{*")
                .or_else(|| segment.strip_prefix('{'))
                .and_then(|rest| rest.strip_suffix('}'));
            let Some(name) = name else {
                segments.push(segment.to_owned());
                continue;
            };
            let value = parameters
                .get(name)
                .ok_or_else(|| RouteError::MissingParameter {
                    name: name.to_owned(),
                })?;
            // A value carrying `/` or `?` or `#` would add segments or a query
            // to a path the caller believed was one value. Trailing captures
            // are the one place a `/` is legitimate.
            let trailing = segment.starts_with("{*");
            if value.contains(['?', '#']) || (!trailing && value.contains('/')) {
                return Err(RouteError::UnsafeValue {
                    name: name.to_owned(),
                });
            }
            used += 1;
            segments.push(value.clone());
        }
        if used != parameters.len() {
            let named: Vec<&str> = self
                .template
                .split('/')
                .filter_map(|segment| {
                    segment
                        .strip_prefix("{*")
                        .or_else(|| segment.strip_prefix('{'))
                        .and_then(|rest| rest.strip_suffix('}'))
                })
                .collect();
            let unknown = parameters
                .keys()
                .find(|key| !named.contains(&key.as_str()))
                .cloned()
                .unwrap_or_default();
            return Err(RouteError::UnknownParameter { name: unknown });
        }
        Ok(segments.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(template: &str) -> RouteMetadata {
        RouteMetadata {
            method: "POST".to_owned(),
            template: template.to_owned(),
            host: Some("receiving.localhost".to_owned()),
        }
    }

    fn parameters(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn a_parameterless_route_is_its_template() {
        assert_eq!(
            route("/purchase_order/query")
                .path(&BTreeMap::new())
                .expect("no parameters needed"),
            "/purchase_order/query"
        );
    }

    #[test]
    fn a_parameter_fills_its_segment() {
        assert_eq!(
            route("/purchase_order/{id}")
                .path(&parameters(&[("id", "3f8e")]))
                .expect("id fills"),
            "/purchase_order/3f8e"
        );
    }

    #[test]
    fn a_missing_parameter_is_named() {
        assert_eq!(
            route("/purchase_order/{id}").path(&BTreeMap::new()),
            Err(RouteError::MissingParameter {
                name: "id".to_owned()
            })
        );
    }

    /// A dropped argument is a request that did not do what its caller asked,
    /// so an extra parameter refuses rather than being ignored.
    #[test]
    fn an_unknown_parameter_refuses_rather_than_being_dropped() {
        assert_eq!(
            route("/purchase_order/{id}").path(&parameters(&[("id", "3f8e"), ("limit", "10")])),
            Err(RouteError::UnknownParameter {
                name: "limit".to_owned()
            })
        );
    }

    /// A value must fill a segment, never add one. Otherwise a caller-supplied
    /// id reaches a different operation than the one it named.
    #[test]
    fn a_value_cannot_change_the_routes_shape() {
        for hostile in ["../receipt/get", "a/b", "x?y", "x#y"] {
            assert_eq!(
                route("/purchase_order/{id}")
                    .path(&parameters(&[("id", hostile)]))
                    .map_err(|error| error.code()),
                Err("route_unsafe_value"),
                "{hostile:?} must not reshape the route"
            );
        }
    }

    /// A trailing capture is the one place a `/` is legitimate.
    #[test]
    fn a_trailing_capture_admits_separators() {
        assert_eq!(
            route("/files/{*path}")
                .path(&parameters(&[("path", "a/b/c.txt")]))
                .expect("trailing capture takes a path"),
            "/files/a/b/c.txt"
        );
    }
}
