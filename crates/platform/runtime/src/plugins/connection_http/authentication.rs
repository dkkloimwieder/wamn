//! Apply the declared authentication scheme to one authorized HTTP request.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use super::{AuthorityDecision, ConnectionError, Request, outbound_headers, reserved_header};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Basic {
    username: String,
    password: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiKey {
    key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Aws {
    access_key_id: String,
    secret_access_key: String,
}

pub(super) fn headers(
    auth_type: &str,
    parameters: &Value,
    material: Option<&str>,
    request: &Request,
    decision: &mut AuthorityDecision,
) -> Result<HashMap<String, String>, ConnectionError> {
    let unavailable = || ConnectionError::CredentialUnavailable;
    if auth_type == "none" {
        return Ok(HashMap::new());
    }
    let material = material.ok_or_else(unavailable)?;
    match auth_type {
        "http_basic" => {
            use base64::Engine as _;
            let value: Basic = serde_json::from_str(material).map_err(|_| unavailable())?;
            if value.username.contains(':') {
                return Err(unavailable());
            }
            let value = base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{}", value.username, value.password));
            Ok(HashMap::from([(
                "authorization".into(),
                format!("Basic {value}"),
            )]))
        }
        "api_key" => {
            let value: ApiKey = serde_json::from_str(material).map_err(|_| unavailable())?;
            let name = parameters["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .ok_or_else(unavailable)?;
            if value.key.is_empty() {
                return Err(unavailable());
            }
            match parameters["in"].as_str() {
                Some("header") => {
                    let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                        .map_err(|_| unavailable())?;
                    if reserved_header(&name) {
                        return Err(unavailable());
                    }
                    reqwest::header::HeaderValue::from_str(&value.key)
                        .map_err(|_| unavailable())?;
                    Ok(HashMap::from([(name.as_str().into(), value.key)]))
                }
                Some("query") => {
                    let mut url =
                        url::Url::parse(&decision.logical_url).map_err(|_| unavailable())?;
                    let query: Vec<_> = url
                        .query_pairs()
                        .filter(|(key, _)| key != name)
                        .map(|(key, value)| (key.into_owned(), value.into_owned()))
                        .collect();
                    url.set_query(None);
                    url.query_pairs_mut()
                        .extend_pairs(query)
                        .append_pair(name, &value.key);
                    decision.logical_url = url.as_str().into();
                    Ok(HashMap::new())
                }
                _ => Err(unavailable()),
            }
        }
        "aws_sigv4" => {
            let value: Aws = serde_json::from_str(material).map_err(|_| unavailable())?;
            let region = parameters["region"]
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(unavailable)?;
            let service = parameters["service"]
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(unavailable)?;
            if value.access_key_id.is_empty() || value.secret_access_key.is_empty() {
                return Err(unavailable());
            }
            let credential = object_store::aws::AwsCredential {
                key_id: value.access_key_id,
                secret_key: value.secret_access_key,
                token: None,
            };
            let mut signed = hyper::Request::builder()
                .method(request.method.as_str())
                .uri(decision.logical_url.as_ref())
                .body(object_store::client::HttpRequestBody::from(
                    request.body.clone().unwrap_or_default(),
                ))
                .map_err(|_| unavailable())?;
            *signed.headers_mut() = outbound_headers(request, HashMap::new())?;
            signed.headers_mut().remove("authorization");
            object_store::aws::AwsAuthorizer::new(&credential, service, region)
                .try_authorize(&mut signed, None)
                .map_err(|_| unavailable())?;
            signed
                .headers()
                .iter()
                .filter(|(name, _)| name.as_str() != "host")
                .map(|(name, value)| {
                    Ok((
                        name.as_str().into(),
                        value.to_str().map_err(|_| unavailable())?.into(),
                    ))
                })
                .collect()
        }
        _ => Err(unavailable()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request() -> Request {
        Request {
            requirement: "upstream".into(),
            method: "POST".into(),
            path_and_query: "/notify".into(),
            headers: Vec::new(),
            body: Some(b"message".to_vec()),
            idempotency_key: None,
        }
    }
    fn decision() -> AuthorityDecision {
        use crate::connection_authority::{
            PinnedEndpoint, TlsPolicy, TransportDecision, parse_http_connection_authority,
        };
        let authority = parse_http_connection_authority(
            "https://example.com/",
            TlsPolicy::VerifyAuthority,
            None,
        )
        .unwrap();
        AuthorityDecision {
            logical_url: "https://example.com/notify?key=guest".into(),
            logical_authority: authority.authority().clone(),
            host_header: "example.com".into(),
            transport: TransportDecision::Direct {
                origin: PinnedEndpoint {
                    address: "93.184.216.34:443".parse().unwrap(),
                    tls_identity: None,
                },
            },
        }
    }
    #[test]
    fn authentication_reads_current_shapes_and_overrides_guest_values() {
        let request = request();
        let mut target = decision();
        assert!(
            headers("none", &json!({}), None, &request, &mut target)
                .unwrap()
                .is_empty()
        );
        let basic = headers(
            "http_basic",
            &json!({}),
            Some(r#"{"username":"user","password":"pass"}"#),
            &request,
            &mut target,
        )
        .unwrap();
        assert_eq!(basic["authorization"], "Basic dXNlcjpwYXNz");
        let key = headers(
            "api_key",
            &json!({"in":"header","name":"x-api-key"}),
            Some(r#"{"key":"private"}"#),
            &request,
            &mut target,
        )
        .unwrap();
        assert_eq!(key["x-api-key"], "private");
        headers(
            "api_key",
            &json!({"in":"query","name":"key"}),
            Some(r#"{"key":"a & b"}"#),
            &request,
            &mut target,
        )
        .unwrap();
        let url = url::Url::parse(&target.logical_url).unwrap();
        assert_eq!(
            url.query_pairs()
                .filter(|(name, _)| name == "key")
                .collect::<Vec<_>>(),
            [("key".into(), "a & b".into())]
        );
        assert_eq!(request.path_and_query, "/notify");
    }
    #[test]
    fn malformed_credentials_and_protected_headers_refuse() {
        for material in [
            "{}",
            r#"{"headers":{}}"#,
            r#"{"key":"ok","extra":true}"#,
            r#"{"key":""}"#,
            r#"{"key":1}"#,
            r#"{"key":"x\r\nheader"}"#,
        ] {
            assert!(
                headers(
                    "api_key",
                    &json!({"in":"header","name":"x-api-key"}),
                    Some(material),
                    &request(),
                    &mut decision()
                )
                .is_err(),
                "{material}"
            );
        }
        for name in [
            "host",
            "content-length",
            "idempotency-key",
            "x-http-method",
            "x-http-method-override",
            "bad header",
        ] {
            assert!(
                headers(
                    "api_key",
                    &json!({"in":"header","name":name}),
                    Some(r#"{"key":"private"}"#),
                    &request(),
                    &mut decision()
                )
                .is_err()
            );
        }
        for auth in [
            "http_basic",
            "api_key",
            "aws_sigv4",
            "oauth2_client_credentials",
            "custom",
        ] {
            assert!(headers(auth, &json!({}), None, &request(), &mut decision()).is_err());
        }
    }
    #[test]
    fn aws_signature_covers_the_body_and_declared_service() {
        let signed = headers(
            "aws_sigv4",
            &json!({"region":"us-east-1","service":"execute-api"}),
            Some(r#"{"access_key_id":"AKID","secret_access_key":"private"}"#),
            &request(),
            &mut decision(),
        )
        .unwrap();
        assert!(signed["authorization"].starts_with("AWS4-HMAC-SHA256 Credential=AKID/"));
        assert!(signed["authorization"].contains("/us-east-1/execute-api/aws4_request"));
        assert_eq!(
            signed["x-amz-content-sha256"],
            hex::encode(ring::digest::digest(&ring::digest::SHA256, b"message"))
        );
    }
}
