//! M1 gate composition over the completed event-path checks.

use std::future::Future;

pub type M1Args = crate::causation_e2e::CausationE2eArgs;

async fn run_check_9<F, Fut>(mut check: F) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    check().await
}

/// Run the currently implemented portion of M1.
///
/// Check 10 (`wamn-0h0g.11.10`) is deliberately not claimed here.
pub async fn run(args: M1Args) -> anyhow::Result<()> {
    println!("# wamn-gates M1 — check 9 only; check 10 pending wamn-0h0g.11.10");
    let mut args = Some(args);
    run_check_9(|| {
        crate::causation_e2e::run(
            args.take()
                .expect("M1 check 9 must be invoked exactly once"),
        )
    })
    .await?;
    println!("M1 partial PASS — check 9 passed; check 10 remains pending");
    Ok(())
}

/// Idempotently clean the exact resources owned by this M1 Job identity.
pub async fn cleanup(args: M1Args) -> anyhow::Result<()> {
    crate::causation_e2e::cleanup_only(args).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct Check9Failure;

    impl fmt::Display for Check9Failure {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("check 9 sentinel failure")
        }
    }

    impl std::error::Error for Check9Failure {}

    #[tokio::test]
    async fn composition_invokes_check_9_exactly_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        run_check_9(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn composition_propagates_check_9_error_unchanged() {
        let error = run_check_9(|| async { Err(anyhow::Error::new(Check9Failure)) })
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<Check9Failure>().is_some());
    }
}
