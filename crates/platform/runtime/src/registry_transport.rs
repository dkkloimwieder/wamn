//! Which upstream transport failures mean the registry contradicted the name.
//!
//! Both OCI sources in this crate pull by digest, so both must tell the same
//! two things apart: a registry that could not answer, and a registry that
//! answered with something the digest does not address. The second is the
//! signal that makes pull-by-digest operationally worth anything, and it is a
//! different page for whoever is holding it.
//!
//! One definition, above both sources rather than owned by either, because two
//! copies of this match are two appliers of one rule: they drift, and the drift
//! is silent — a variant added to one list and not the other reclassifies a
//! lying registry as a missing one on exactly one of the two paths.

use oci_client::errors::OciDistributionError;

/// True when this transport failure is the registry contradicting the name.
///
/// The list binds this crate's refusal taxonomy to an `oci-client` version, and
/// that cost is accepted deliberately (`wamn-0h0g.19.17`): the distinction is
/// only observable here, because `pull_blob` builds its digester from the layer
/// descriptor and refuses a body that does not finalize to it — so a lying
/// registry never reaches a caller's own comparison, it arrives as
/// [`OciDistributionError::DigestError`] or not at all.
pub(crate) fn transport_is_mismatched(error: &OciDistributionError) -> bool {
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
