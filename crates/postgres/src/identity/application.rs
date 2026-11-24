//! Project database identities and authority projection
//! (docs/architecture/overview.md#organization-membership-and-administration).
//!
//! System writers commit authority before projection. Projection reads current
//! authority under the org membership lock and commits application data before
//! releasing that lock.

pub mod service;

mod user;
pub use user::{disable_connection_services, remove_user, write_user};

use crate::administration::{project_administration, read_project_authority};
use wamn_authentication::administration::{ApplicationTarget, ApplicationUser};
use wamn_identity_protocol::PrincipalId;

use wamn_authentication::identity_error::IdentityError;

/// Rebuild one principal's application authority from a locked system transaction.
/// The caller locks membership before opening the application transaction and
/// commits the application transaction before ending the system transaction.
pub async fn project_authority(
    system: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    target: &mut sqlx::PgConnection,
    coordinate: ApplicationTarget<'_>,
    principal: &PrincipalId,
    issuer: &str,
) -> Result<(), IdentityError> {
    crate::identity::org::lock_membership(system, principal, coordinate.org).await?;
    let Some(authority) = read_project_authority(system, coordinate, principal).await? else {
        return remove_user(target, principal).await;
    };
    let user = ApplicationUser {
        principal_id: principal,
        email: &authority.email,
        display_name: &authority.display_name,
    };
    write_user(target, issuer, user).await?;
    project_administration(target, principal, authority.admin).await
}
