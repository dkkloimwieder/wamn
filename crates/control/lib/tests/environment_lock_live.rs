//! Live test of the lifecycle lock (docs/plan/platform-deploy.md R19,
//! `wamn-snz0.1`): a second verb on the same coordinate refuses at once with
//! "deployment in progress", another coordinate is not blocked, and closing
//! the holder's connection releases the lock.

use wamn_control::environment::lock::{DEPLOYMENT_IN_PROGRESS, LifecycleLock};
use wamn_control_registry::Triple;

#[tokio::test]
async fn a_second_verb_refuses_until_the_holder_is_gone() {
    let database = wamn_test_postgres::database();
    let url = database.url();
    let prod = Triple::new("acme", "wms", "prod");

    let held = LifecycleLock::acquire(url, &prod)
        .await
        .expect("take the lock");
    let refused = LifecycleLock::acquire(url, &prod)
        .await
        .expect_err("a second verb on the same coordinate refuses");
    assert!(
        refused.to_string().starts_with(DEPLOYMENT_IN_PROGRESS),
        "{refused:#}"
    );

    let other = LifecycleLock::acquire(url, &Triple::new("acme", "wms", "dev"))
        .await
        .expect("another coordinate is not blocked");
    drop(other);

    drop(held);
    // The server ends the session when it reads the closed socket.
    let mut retaken = None;
    for _ in 0..50 {
        if let Ok(lock) = LifecycleLock::acquire(url, &prod).await {
            retaken = Some(lock);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        retaken.is_some(),
        "closing the holder's connection releases the lock"
    );
}
