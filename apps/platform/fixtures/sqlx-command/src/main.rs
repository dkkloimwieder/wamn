//! Exercises SQLx queries and explicit transactions over `wamn:postgres`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use sqlx::Error;
use wamn_postgres_sqlx::{
    WamnConnection, WamnDatabaseError, WamnPgError, WamnPostgres, run_transaction,
};

const FORCED_ROLLBACK: &str = "sqlx-command-forced-rollback";

fn is_permission_denied(error: &Error) -> bool {
    let Error::Database(database) = error else {
        return false;
    };
    database
        .as_error()
        .downcast_ref::<WamnDatabaseError>()
        .is_some_and(|error| matches!(error.pg_error(), WamnPgError::PermissionDenied))
}

async fn exercise() {
    let mut connection = WamnConnection::new();

    let committed = run_transaction(&mut connection, |transaction| {
        Box::pin(async move {
            let row = sqlx::query_as::<WamnPostgres, (i32, String)>(
                "INSERT INTO effects (id, label, owner_name) \
                 VALUES ($1, $2, current_user) RETURNING id, label",
            )
            .bind(1_i32)
            .bind("committed")
            .fetch_one(&mut **transaction)
            .await?;
            Ok::<_, Error>(row)
        })
    })
    .await
    .expect("the committed transaction succeeds");
    assert_eq!(committed, (1, "committed".to_owned()));

    let attempts = Arc::new(AtomicUsize::new(0));
    let callback_attempts = Arc::clone(&attempts);
    let rolled_back = run_transaction(&mut connection, move |transaction| {
        Box::pin(async move {
            callback_attempts.fetch_add(1, Ordering::SeqCst);
            sqlx::query::<WamnPostgres>(
                "INSERT INTO effects (id, label, owner_name) VALUES ($1, $2, current_user)",
            )
            .bind(2_i32)
            .bind("rolled-back")
            .execute(&mut **transaction)
            .await?;
            Err::<(), _>(Error::Protocol(FORCED_ROLLBACK.to_owned()))
        })
    })
    .await;
    assert!(matches!(rolled_back, Err(Error::Protocol(message)) if message == FORCED_ROLLBACK));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "the runner must not retry"
    );

    let denied = run_transaction(&mut connection, |transaction| {
        Box::pin(async move {
            sqlx::query::<WamnPostgres>(
                "INSERT INTO effects (id, label, owner_name) VALUES ($1, $2, $3)",
            )
            .bind(3_i32)
            .bind("wrong-identity")
            .bind("not-the-current-user")
            .execute(&mut **transaction)
            .await?;
            Ok::<_, Error>(())
        })
    })
    .await
    .expect_err("RLS must reject a row owned by the wrong current_user");
    assert!(
        is_permission_denied(&denied),
        "RLS must surface the typed permission-denied case, got {denied:?}"
    );
}

fn main() {
    futures_executor::block_on(exercise());
}
