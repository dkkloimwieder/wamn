//! Authored package declarations, compiled operation contracts, and package identities.

mod attachment;
pub mod canonical_name;
pub mod client_package_name;
mod component_declaration;
pub mod feed;
pub mod local_operation;
pub mod manifest;
mod operation_type;
mod package;
pub mod package_schema;
pub mod package_version;
mod palette;
pub mod registered_operation;
mod route_identity;
pub mod row_operation;
pub mod schema;
mod signed_file_read;

pub use attachment::{
    AttachmentAuthPolicy, AttachmentTarget, AttachmentType, INVALID_ATTACHMENT_AUTH_POLICY_REFUSAL,
    NO_AUTHENTICATION_MODE, PAT_AUTHENTICATION_MODE, SESSION_AUTHENTICATION_MODE,
    ServingAttachment, parse_attachment_auth_policy,
};
pub use component_declaration::{
    ComponentConnection, ComponentConnectionAlias, ComponentConnectionType, ComponentDeclaration,
    ComponentOperationDeclaration, ComponentParameterDeclaration, ComponentPortDeclaration,
    ConnectionMode,
};
pub use operation_type::OperationType;
pub use package::{
    PackageCoordinate, PackageName, ParsePackageNameError, validate_canonical_operation_for_package,
};
pub use palette::{
    PALETTE_DESCRIPTOR_ANNOTATION, PALETTE_FILE, PALETTE_PACKAGE_ID, PALETTE_PACKAGE_VERSION,
    palette_package,
};
pub use route_identity::{
    OperationContract, OperationDescription, StatementDescription, StatementValue,
    operation_contract, operation_token, sealed_operation_reference, split_sealed_operation,
};
pub use row_operation::RowOperation;
pub use signed_file_read::{
    MAX_SIGNED_FILE_SECONDS, SIGNED_FILE_READS, SignedFileField, SignedFileRead,
    SignedFileReadError, SignedFileReads, SignedFileResultClass,
};

/// The interface exported by an application data change handler.
pub const DATA_CHANGE_EXPORT: &str = "wamn:handler/data-change@0.1.0";

/// An invalid package identity or operation reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageIdentityError {
    message: String,
}

impl std::fmt::Display for PackageIdentityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PackageIdentityError {}
