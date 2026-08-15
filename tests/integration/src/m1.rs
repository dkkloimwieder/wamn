//! M1 gate composition over the completed event-path checks.

use std::future::Future;

pub type M1Args = crate::causation_e2e::CausationE2eArgs;

async fn run_checks_9_and_10<F, Fut>(mut check: F) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    check().await
}

/// Run the complete M1 event-path proof.
pub async fn run(args: M1Args) -> anyhow::Result<()> {
    println!("# wamn-gates M1 — checks 9 and 10");
    let mut args = Some(args);
    run_checks_9_and_10(|| {
        crate::causation_e2e::run(
            args.take()
                .expect("M1 checks 9 and 10 must share one fixture invocation"),
        )
    })
    .await?;
    println!("M1 PASS — checks 9 and 10 passed");
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
    struct M1Failure;

    impl fmt::Display for M1Failure {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("M1 sentinel failure")
        }
    }

    impl std::error::Error for M1Failure {}

    #[tokio::test]
    async fn composition_invokes_the_shared_check_9_fixture_exactly_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        run_checks_9_and_10(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn composition_propagates_m1_error_unchanged() {
        let error = run_checks_9_and_10(|| async { Err(anyhow::Error::new(M1Failure)) })
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<M1Failure>().is_some());
    }
}
