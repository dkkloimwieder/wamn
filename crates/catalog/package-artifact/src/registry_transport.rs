//! Classify upstream OCI transport failures for every artifact reader.
//!
//! A mismatch means that the registry contradicted the named artifact.
//! Unavailable transport remains a separate outcome.

use oci_client::errors::OciDistributionError;

/// Delete a digest through the shared HTTP transport and registry authorization owner.
pub(crate) async fn delete_manifest(
    http: &crate::http_client::HttpClient,
    insecure: bool,
    reference: &oci_client::Reference,
    auth: &oci_client::secrets::RegistryAuth,
    digest: &str,
    timeout: std::time::Duration,
) -> anyhow::Result<()> {
    use anyhow::{Context as _, ensure};
    use bytes::Bytes;
    use hyper::{Method, Request, StatusCode};

    let scheme = if insecure { "http" } else { "https" };
    let uri = format!(
        "{scheme}://{}/v2/{}/manifests/{digest}",
        reference.resolve_registry(),
        reference.repository()
    );
    tokio::time::timeout(timeout, async {
        let request = authorized(Request::delete(&uri), auth).body(Bytes::new())?;
        let mut response = http.send(request, timeout, 65_536).await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            let challenge = response
                .headers()
                .get(hyper::header::WWW_AUTHENTICATE)
                .context("registry omitted delete authorization")?
                .to_str()?;
            let challenge = http_auth::parser::ChallengeParser::new(challenge)
                .filter_map(Result::ok)
                .find(|challenge| challenge.scheme.eq_ignore_ascii_case("Bearer"))
                .context("registry did not offer Bearer authorization")?;
            let mut realm = None;
            let mut service = None;
            for (name, value) in &challenge.params {
                if name.eq_ignore_ascii_case("realm") {
                    realm = Some(value.to_unescaped());
                }
                if name.eq_ignore_ascii_case("service") {
                    service = Some(value.to_unescaped());
                }
            }
            let mut realm =
                url::Url::parse(&realm.context("registry authorization has no realm")?)?;
            ensure!(
                realm.scheme() == "https" || insecure,
                "registry authorization requires HTTPS"
            );
            realm.query_pairs_mut().append_pair(
                "scope",
                &format!("repository:{}:delete", reference.repository()),
            );
            if let Some(service) = service {
                realm.query_pairs_mut().append_pair("service", &service);
            }
            let request = authorized(
                Request::builder().method(Method::GET).uri(realm.as_str()),
                auth,
            )
            .body(Bytes::new())?;
            let token = http.send(request, timeout, 65_536).await?;
            ensure!(
                token.status().is_success(),
                "registry delete authorization refused"
            );
            let token: serde_json::Value = serde_json::from_slice(token.body())?;
            let token = token
                .get("token")
                .or_else(|| token.get("access_token"))
                .and_then(serde_json::Value::as_str)
                .context("registry authorization returned no token")?;
            let request = Request::delete(&uri)
                .header(hyper::header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Bytes::new())?;
            response = http.send(request, timeout, 65_536).await?;
        }
        ensure!(
            matches!(
                response.status(),
                StatusCode::ACCEPTED | StatusCode::NOT_FOUND
            ),
            "registry refused manifest deletion: {}",
            response.status()
        );
        Ok(())
    })
    .await
    .context("registry manifest deletion deadline exceeded")?
}

fn authorized(
    request: hyper::http::request::Builder,
    auth: &oci_client::secrets::RegistryAuth,
) -> hyper::http::request::Builder {
    use base64::Engine as _;
    use oci_client::secrets::RegistryAuth;
    let header = match auth {
        RegistryAuth::Anonymous => return request,
        RegistryAuth::Basic(username, password) => format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
        ),
        RegistryAuth::Bearer(token) => format!("Bearer {token}"),
    };
    request.header(hyper::header::AUTHORIZATION, header)
}

/// True when this transport failure is the registry contradicting the name.
///
/// The list binds this crate's refusal taxonomy to an `oci-client` version, and
/// that cost is accepted deliberately (`wamn-0h0g.19.17`): the distinction is
/// only observable here, because `pull_blob` builds its digester from the layer
/// descriptor and refuses a body that does not finalize to it — so a lying
/// registry never reaches a caller's own comparison, it arrives as
/// [`OciDistributionError::DigestError`] or not at all.
pub fn transport_is_mismatched(error: &OciDistributionError) -> bool {
    matches!(
        error,
        OciDistributionError::ConfigConversionError(_)
            | OciDistributionError::DigestError(_)
            | OciDistributionError::ImageIndexParsingNoPlatformResolverError
            | OciDistributionError::IncompatibleLayerMediaTypeError(_)
            | OciDistributionError::JsonError(_)
            | OciDistributionError::ManifestEncodingError(_)
            | OciDistributionError::ManifestParsingError(_)
            | OciDistributionError::PullNoLayersError
            | OciDistributionError::RegistryNoDigestError
            | OciDistributionError::SpecViolationError(_)
            | OciDistributionError::UnsupportedMediaTypeError(_)
            | OciDistributionError::UnsupportedSchemaVersionError(_)
            | OciDistributionError::VersionedParsingError(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_client::errors::DigestError;
    use wamn_digest::bytes_sha256;

    #[test]
    fn transport_integrity_errors_are_not_classified_as_unavailable() {
        let digest_error = OciDistributionError::DigestError(DigestError::VerificationError {
            expected: bytes_sha256(b"expected"),
            actual: bytes_sha256(b"actual"),
        });
        assert!(transport_is_mismatched(&digest_error));
        assert!(!transport_is_mismatched(
            &OciDistributionError::ImageManifestNotFoundError("missing".to_owned())
        ));
    }
}
