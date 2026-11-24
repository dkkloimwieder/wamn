//! Sessions that bind the caller's authority for native administration.

use wamn_identity_protocol::PrincipalId;
use wamn_package::canonical_name::CanonicalName;
use wamn_registry::ProjectId;

/// Open a session with an active administration transaction.
pub trait AdministrationTransactions {
    type Session: AdministrationSession;

    fn begin_administration(
        &self,
        project: &ProjectId,
        principal: &PrincipalId,
        operation: &CanonicalName,
    ) -> impl Future<Output = anyhow::Result<Self::Session>> + Send;
}

/// An owned session that can retain its locks after the transaction ends.
pub trait AdministrationSession: Send {
    type Connection;

    fn connection(&mut self) -> &mut Self::Connection;

    /// End the transaction and return the clean session to its pool.
    fn finish(
        self,
        outcome: AdministrationOutcome,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;

    /// Return the session after the caller has ended its transaction and released its locks.
    fn repool(self);
}

/// How to end an administration transaction.
#[derive(Debug, Clone, Copy)]
pub enum AdministrationOutcome {
    Commit,
    Rollback,
}

/// Run the callback with the caller's authority and settle its transaction.
///
/// # Errors
/// Returns a session error if opening or settling the transaction fails.
pub async fn administration_transaction<S, T, E>(
    store: &S,
    project: &ProjectId,
    principal: &PrincipalId,
    operation: &CanonicalName,
    work: impl AsyncFnOnce(&mut <S::Session as AdministrationSession>::Connection) -> Result<T, E>,
) -> anyhow::Result<Result<T, E>>
where
    S: AdministrationTransactions,
{
    let mut session = store
        .begin_administration(project, principal, operation)
        .await?;
    let result = work(session.connection()).await;
    let outcome = if result.is_ok() {
        AdministrationOutcome::Commit
    } else {
        AdministrationOutcome::Rollback
    };
    session.finish(outcome).await?;
    Ok(result)
}
