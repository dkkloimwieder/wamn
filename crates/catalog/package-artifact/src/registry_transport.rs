//! Classify upstream OCI transport failures for every artifact reader.
//!
//! A mismatch means that the registry contradicted the named artifact.
//! Unavailable transport remains a separate outcome.

use oci_client::errors::OciDistributionError;

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
