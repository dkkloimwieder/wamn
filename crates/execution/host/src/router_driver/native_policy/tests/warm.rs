//! Native reuse keeps authority per invocation and resources out of later calls.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, timeout};
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::dispatch::{GuestCall, GuestCallFuture};
use wash_runtime::wasmtime::component::{Accessor, Instance};

use super::{CLEANUP, Case, Fixture, invoke_native, run_isolated_test};

pub(super) async fn fixture(case: Case) -> Fixture {
    Fixture::build_with_reuse(case, None, false, None, true, None).await
}

pub(super) fn starts(fixture: &Fixture) -> usize {
    fixture
        .events
        .lock()
        .expect("events")
        .iter()
        .filter(|event| event.phase == 0)
        .count()
}

pub(super) async fn close(fixture: &Fixture) {
    fixture
        .workload
        .resolved
        .unbind_all_plugins()
        .await
        .expect("close native pools");
    fixture.assert_clean().await;
}

#[test]
fn native_warm_reuse_and_idle_reclamation() {
    run_isolated_test("warm::native_warm_reuse_and_idle_reclamation", async {
        let fixture = fixture(Case::Success).await;
        let target = fixture.target().await;
        for input in ["alice", "bob", "alice"] {
            let mut request = fixture.request(Instant::now() + CLEANUP);
            request.input = input.into();
            assert_eq!(
                invoke_native(&target, request)
                    .await
                    .expect("dispatch")
                    .expect("emission")
                    .payload,
                input
            );
            assert!(
                fixture
                    .policy
                    .invocations
                    .lock()
                    .expect("authority")
                    .is_empty()
            );
        }
        assert_eq!(
            starts(&fixture),
            1,
            "one initializer for three real handler calls proves instance reuse"
        );
        let events = fixture.events.lock().expect("events").clone();
        let scopes: std::collections::BTreeSet<_> = events
            .iter()
            .filter(|event| event.phase == 1)
            .map(|event| &event.scope)
            .collect();
        assert_eq!(
            scopes.len(),
            3,
            "each call owns a different authority scope"
        );
        timeout(Duration::from_secs(4), async {
            while fixture.engine.guest_memory().in_use() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("idle native pools return guest memory");
        invoke_native(&target, fixture.request(Instant::now() + CLEANUP))
            .await
            .expect("dispatch after idle")
            .expect("emission");
        assert_eq!(
            starts(&fixture),
            2,
            "idle reclamation requires a new instance"
        );
        close(&fixture).await;
    });
}

#[test]
fn native_warm_release_shutdown_refuses_retained_target() {
    run_isolated_test(
        "warm::native_warm_release_shutdown_refuses_retained_target",
        async {
            let fixture = fixture(Case::Success).await;
            let target = fixture.target().await;
            invoke_native(&target, fixture.request(Instant::now() + CLEANUP))
                .await
                .expect("live release dispatch")
                .expect("emission");
            fixture.policy.shutdown();
            let error = invoke_native(&target, fixture.request(Instant::now() + CLEANUP))
                .await
                .expect_err("retained target cannot revive retired release authority");
            assert!(format!("{error:#}").contains("native invocation component is not admitted"));
            assert_eq!(
                starts(&fixture),
                1,
                "the warm instance reached the admission refusal"
            );
            assert_eq!(
                fixture
                    .events
                    .lock()
                    .expect("events")
                    .iter()
                    .filter(|event| event.phase == 1)
                    .count(),
                1,
                "retired release never executes a second guest call"
            );
            close(&fixture).await;
        },
    );
}

#[test]
fn native_warm_failures_retire_instances() {
    run_isolated_test("warm::native_warm_failures_retire_instances", async {
        for case in [Case::Trap, Case::RunDeadline, Case::Cancellation] {
            let fixture = fixture(case).await;
            let target = fixture.target().await;
            let request = fixture.request(
                Instant::now()
                    + if matches!(case, Case::RunDeadline) {
                        Duration::from_millis(200)
                    } else {
                        Duration::from_secs(10)
                    },
            );
            if matches!(case, Case::Cancellation) {
                let task = tokio::spawn(async move { invoke_native(&target, request).await });
                timeout(CLEANUP, fixture.entered.notified())
                    .await
                    .expect("guest entered");
                task.abort();
                assert!(task.await.expect_err("aborted caller").is_cancelled());
            } else {
                assert!(invoke_native(&target, request).await.is_err());
            }
            assert!(
                fixture
                    .policy
                    .invocations
                    .lock()
                    .expect("authority")
                    .is_empty(),
                "caller cancellation revokes immediately"
            );
            // Native requires its ten-second grace plus continuous execution
            // credit before trapping a spinning abandoned store.
            timeout(Duration::from_secs(25), async {
                while fixture.engine.guest_memory().in_use() != 0 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("native abandoned stores retire within their grace");
            fixture.assert_clean().await;
            assert_eq!(
                starts(&fixture),
                1,
                "failed stores must not serve the next call"
            );
            super::prepare_native(&fixture.target().await, Instant::now() + CLEANUP)
                .await
                .expect("replacement store initializes after retirement");
            assert_eq!(starts(&fixture), 2, "retired instance is replaced");
            close(&fixture).await;
        }
    });
}

struct Hold {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl GuestCall for Hold {
    fn describe(&self) -> &'static str {
        "test pool saturation"
    }
    fn deadline(&self) -> Duration {
        Duration::from_secs(10)
    }
    fn call(
        self: Box<Self>,
        _accessor: &Accessor<SharedCtx>,
        _instance: Instance,
    ) -> GuestCallFuture<'_> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(None)
        })
    }
}

#[test]
fn native_warm_saturation_uses_fresh_overflow() {
    run_isolated_test("warm::native_warm_saturation_uses_fresh_overflow", async {
        let fixture = fixture(Case::Success).await;
        let target = fixture.target().await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let holder = Hold {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        };
        let task = tokio::spawn(async move { target.dispatch(holder).await });
        timeout(CLEANUP, entered.notified())
            .await
            .expect("warm slot occupied");
        let target = fixture.target().await;
        for _ in 0..2 {
            invoke_native(&target, fixture.request(Instant::now() + CLEANUP))
                .await
                .expect("overflow dispatch")
                .expect("emission");
        }
        assert_eq!(
            starts(&fixture),
            3,
            "one warm store and two separate overflow stores"
        );
        release.notify_one();
        task.await.expect("holder task").expect("holder completion");
        invoke_native(&target, fixture.request(Instant::now() + CLEANUP))
            .await
            .expect("warm dispatch")
            .expect("emission");
        assert_eq!(
            starts(&fixture),
            3,
            "the original warm instance remains available"
        );
        close(&fixture).await;
    });
}

struct RetainedResource(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for RetainedResource {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
struct LeaveResource(Arc<std::sync::atomic::AtomicUsize>);
impl GuestCall for LeaveResource {
    fn describe(&self) -> &'static str {
        "retain a host resource"
    }
    fn call(
        self: Box<Self>,
        accessor: &Accessor<SharedCtx>,
        _instance: Instance,
    ) -> GuestCallFuture<'_> {
        Box::pin(async move {
            accessor.with(|mut access| access.get().table.push(RetainedResource(self.0)))?;
            Ok(None)
        })
    }
}

#[test]
fn native_warm_retained_resources_force_retirement() {
    run_isolated_test(
        "warm::native_warm_retained_resources_force_retirement",
        async {
            let fixture = fixture(Case::Success).await;
            let target = fixture.target().await;
            let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            target
                .dispatch(LeaveResource(Arc::clone(&dropped)))
                .await
                .expect("resource created in real warm store");
            assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 0);
            invoke_native(&target, fixture.request(Instant::now() + CLEANUP))
                .await
                .expect("completed result survives retirement")
                .expect("emission");
            assert_eq!(
                dropped.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "host resource is destroyed before completion"
            );
            invoke_native(&target, fixture.request(Instant::now() + CLEANUP))
                .await
                .expect("next invocation")
                .expect("emission");
            assert_eq!(
                starts(&fixture),
                2,
                "a retained handle cannot alias a resource in the next instance"
            );
            close(&fixture).await;
        },
    );
}
