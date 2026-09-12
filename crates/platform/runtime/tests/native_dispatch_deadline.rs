//! Native fresh dispatch keeps initialization and execution inside one caller deadline.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::time::{Instant, timeout_at};
use wamn_runtime::engine::{build_engine_with_host_memory, host_memory_budgets};
use wash_runtime::engine::Engine;
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::dispatch::{DispatchTarget, GuestCall, GuestCallFuture};
use wash_runtime::host::http::NullServer;
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin::PluginBindings;
use wash_runtime::types::{Component, Workload};
use wasmtime::component::{Accessor, Instance};

const PAGE: u64 = 65_536;
const CALL_BUDGET: Duration = Duration::from_millis(200);
const CLEANUP_BUDGET: Duration = Duration::from_secs(2);
const CHILD_MARKER: &str = "WAMN_NATIVE_DISPATCH_DEADLINE_CHILD";

#[derive(Clone, Copy)]
enum Case {
    RootStart,
    LinkedStart,
    Export,
    Cancellation,
    NativeDeadlineOnly,
}

// Each hostile guest runs in its own process. A broken epoch yield must fail
// the test instead of pinning a Tokio worker and the entire test runner.
fn isolated(name: &str, case: Case) {
    if std::env::var(CHILD_MARKER).as_deref() != Ok(name) {
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", name, "--nocapture"])
            .env(CHILD_MARKER, name)
            .output()
            .expect("start isolated deadline proof");
        if matches!(case, Case::NativeDeadlineOnly) {
            assert_eq!(output.status.code(), Some(124), "{output:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("native deadline elapsed while root initialization kept running"),
                "the negative control must reach the hostile initialization: {output:?}"
            );
            return;
        }
        assert!(
            output.status.success(),
            "{name} failed with {}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (done, finished) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(60)) == Err(mpsc::RecvTimeoutError::Timeout) {
            eprintln!("native dispatch proof exceeded its process watchdog");
            std::process::exit(124);
        }
    });
    // Native epoch yields wake the guest task again. Poll timers after each
    // scheduled task so that Tokio's default 61-task batch cannot defer the
    // enclosing deadline by about six seconds. This is an embedder setting,
    // not a change to the guest deadline or to the upstream runtime.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .event_interval(1)
        .enable_all()
        .build()
        .expect("isolated Tokio runtime");
    runtime.block_on(assert_deadline(case));
    drop(runtime);
    done.send(()).expect("stop process watchdog");
    watchdog.join().expect("join process watchdog");
}

#[test]
fn native_root_start_obeys_enclosing_deadline() {
    isolated(
        "native_root_start_obeys_enclosing_deadline",
        Case::RootStart,
    );
}

#[test]
fn native_linked_start_obeys_enclosing_deadline() {
    isolated(
        "native_linked_start_obeys_enclosing_deadline",
        Case::LinkedStart,
    );
}

#[test]
fn native_export_obeys_enclosing_deadline() {
    isolated("native_export_obeys_enclosing_deadline", Case::Export);
}

#[test]
fn native_dispatch_cancellation_releases_capacity() {
    isolated(
        "native_dispatch_cancellation_releases_capacity",
        Case::Cancellation,
    );
}

#[test]
fn native_call_deadline_alone_does_not_bound_root_start() {
    isolated(
        "native_call_deadline_alone_does_not_bound_root_start",
        Case::NativeDeadlineOnly,
    );
}

struct Run {
    deadline: Duration,
    entered: Arc<AtomicBool>,
    started: Option<oneshot::Sender<()>>,
    answer: oneshot::Sender<u32>,
}

impl GuestCall for Run {
    fn describe(&self) -> &'static str {
        "wamn:deadline/probe#run"
    }

    fn deadline(&self) -> Duration {
        self.deadline
    }

    fn call(
        self: Box<Self>,
        accessor: &Accessor<SharedCtx>,
        instance: Instance,
    ) -> GuestCallFuture<'_> {
        Box::pin(async move {
            let run = accessor
                .with(|mut store| instance.get_typed_func::<(), (u32,)>(&mut store, "run"))?;
            self.entered.store(true, Ordering::SeqCst);
            if let Some(started) = self.started {
                let _ = started.send(());
            }
            let (answer,) = run.call_concurrent(accessor, ()).await?;
            let _ = self.answer.send(answer);
            Ok(None)
        })
    }
}

fn component(name: &str, wat: &str) -> Component {
    Component {
        name: name.to_owned(),
        bytes: wat::parse_str(wat).expect("encode deadline fixture").into(),
        pool_size: 0,
        max_concurrency: 1,
        ..Component::default()
    }
}

fn scalar_component(start: &str, body: &str, linked_export: bool) -> String {
    let export = if linked_export {
        r#"(instance $exports (export "run" (func $run)))
            (export "wamn:deadline/callee@0.1.0" (instance $exports))"#
    } else {
        r#"(export "run" (func $run))"#
    };
    format!(
        r#"(component
            (core module $code
                (memory 1)
                {start}
                (func (export "run") (result i32) {body}))
            (core instance $code (instantiate $code))
            (func $run (result u32) (canon lift (core func $code "run")))
            {export})"#
    )
}

const LINKED_CALLER: &str = r#"(component
    (import "wamn:deadline/callee@0.1.0" (instance $callee
        (export "run" (func (result u32)))))
    (alias export $callee "run" (func $run))
    (core func $lowered (canon lower (func $run)))
    (core module $code
        (import "callee" "run" (func $run (result i32)))
        (memory 1)
        (func (export "run") (result i32) call $run))
    (core instance $code (instantiate $code
        (with "callee" (instance (export "run" (func $lowered))))))
    (func (export "run") (result u32) (canon lift (core func $code "run"))))"#;

async fn target(engine: &Engine, components: Vec<Component>) -> DispatchTarget {
    let unresolved = engine
        .initialize_workload(
            "deadline-proof",
            Workload {
                name: "deadline-proof".to_owned(),
                namespace: "deadline-proof".to_owned(),
                annotations: HashMap::new(),
                service: None,
                components,
                host_interfaces: Vec::new(),
                volumes: Vec::new(),
            },
        )
        .expect("initialize the public native workload");
    let workload = unresolved
        .resolve(
            None,
            &PluginBindings::new(),
            Arc::new(NullServer::default()),
            &Meters::new(MeterKind::Off),
        )
        .await
        .expect("resolve the public native workload");
    let components = workload.components();
    let root_id = components
        .read()
        .await
        .iter()
        .find(|(_, component)| component.name() == "root")
        .map(|(id, _)| Arc::clone(id))
        .expect("the workload contains root");
    workload
        .dispatch_target(&root_id, "deadline-proof")
        .await
        .expect("resolve a public native dispatch target")
}

async fn assert_deadline(case: Case) {
    let slots = if matches!(case, Case::LinkedStart) {
        2
    } else {
        1
    };
    let budgets = host_memory_budgets(
        4 * usize::try_from(PAGE).expect("one Wasm page fits usize"),
        slots,
    )
    .expect("bounded native budgets");
    let engine = build_engine_with_host_memory(&[], budgets).expect("production WAMN engine");
    let start_loop = "(func $start (loop br 0)) (start $start)";
    let components = match case {
        Case::RootStart | Case::NativeDeadlineOnly => vec![component(
            "root",
            &scalar_component(start_loop, "i32.const 7", false),
        )],
        Case::LinkedStart => vec![
            component("root", LINKED_CALLER),
            component("callee", &scalar_component(start_loop, "i32.const 7", true)),
        ],
        Case::Export | Case::Cancellation => vec![component(
            "root",
            &scalar_component("", "(loop br 0) unreachable", false),
        )],
    };
    let hostile = target(&engine, components).await;
    let entered = Arc::new(AtomicBool::new(false));
    let (answer, reply) = oneshot::channel();
    if matches!(case, Case::NativeDeadlineOnly) {
        native_deadline_control(&engine, hostile, entered, answer).await;
        unreachable!("the negative control ends at its process watchdog");
    }
    if matches!(case, Case::Cancellation) {
        let (started, start) = oneshot::channel();
        let call = Run {
            // The enclosing deadline must win over this native relative timeout.
            deadline: Duration::from_secs(30),
            entered: Arc::clone(&entered),
            started: Some(started),
            answer,
        };
        let task = tokio::spawn(async move { hostile.dispatch(call).await });
        let wait_started = Instant::now();
        timeout_at(wait_started + CLEANUP_BUDGET, start)
            .await
            .expect("the guest call starts before cancellation")
            .expect("the guest call signals entry");
        assert!(
            wait_started.elapsed() < CLEANUP_BUDGET,
            "guest entry is prompt"
        );
        assert_eq!(engine.guest_memory().in_use(), PAGE);
        let abort_started = Instant::now();
        task.abort();
        assert!(
            timeout_at(abort_started + CLEANUP_BUDGET, task)
                .await
                .expect("the dispatcher cancellation stays bounded")
                .expect_err("the dispatcher is cancelled")
                .is_cancelled()
        );
        assert!(
            abort_started.elapsed() < CLEANUP_BUDGET,
            "cancellation is prompt"
        );
    } else {
        let call = Run {
            // The enclosing deadline must win over this native relative timeout.
            deadline: Duration::from_secs(30),
            entered: Arc::clone(&entered),
            started: None,
            answer,
        };
        let started = Instant::now();
        timeout_at(started + CALL_BUDGET, hostile.dispatch(call))
            .await
            .expect_err("the enclosing deadline must stop the CPU loop");
        assert!(
            started.elapsed() < CALL_BUDGET + CLEANUP_BUDGET,
            "native epoch yields must return control near the enclosing deadline: elapsed={:?}",
            started.elapsed()
        );
        assert_eq!(entered.load(Ordering::SeqCst), matches!(case, Case::Export));
    }
    assert!(
        timeout_at(Instant::now() + CLEANUP_BUDGET, reply)
            .await
            .expect("the cancelled call releases its reply channel")
            .is_err(),
        "a stopped infinite guest produces no answer"
    );
    assert!(
        engine.guest_memory().high_water() >= PAGE,
        "the hostile guest allocated memory"
    );
    // Native cancellation aborts an owned task. Let that task unwind, under a
    // bound, before judging its store's memory and allocator refunds.
    timeout_at(Instant::now() + CLEANUP_BUDGET, async {
        while engine.guest_memory().in_use() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native cancellation refunds every store's memory");

    let finite = target(
        &engine,
        vec![component(
            "root",
            &scalar_component("", "i32.const 7", false),
        )],
    )
    .await;
    let (answer, reply) = oneshot::channel();
    timeout_at(
        Instant::now() + CLEANUP_BUDGET,
        finite.dispatch(Run {
            deadline: Duration::from_secs(30),
            entered,
            started: None,
            answer,
        }),
    )
    .await
    .expect("the next finite guest stays within its deadline")
    .expect("the next finite guest can use the released allocator capacity");
    assert_eq!(
        timeout_at(Instant::now() + CLEANUP_BUDGET, reply)
            .await
            .expect("the finite guest replies within the bound")
            .expect("finite guest result"),
        7
    );
    assert_eq!(engine.guest_memory().in_use(), 0);
}

async fn native_deadline_control(
    engine: &Engine,
    target: DispatchTarget,
    entered: Arc<AtomicBool>,
    answer: oneshot::Sender<u32>,
) {
    // Arm only after compilation and resolution. The observation below must
    // also check that the root allocated memory before this watchdog wins.
    let (done, finished) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if finished.recv_timeout(CLEANUP_BUDGET) == Err(mpsc::RecvTimeoutError::Timeout) {
            eprintln!("native startup control watchdog expired");
            std::process::exit(124);
        }
    });
    let mut dispatch = Box::pin(target.dispatch(Run {
        deadline: CALL_BUDGET,
        entered: Arc::clone(&entered),
        started: None,
        answer,
    }));
    tokio::select! {
        result = &mut dispatch => panic!("native dispatch stopped the root start loop: {result:?}"),
        () = tokio::time::sleep(2 * CALL_BUDGET) => {}
    }
    assert_eq!(engine.guest_memory().in_use(), PAGE);
    assert!(!entered.load(Ordering::SeqCst));
    eprintln!("native deadline elapsed while root initialization kept running");
    let result = dispatch.await;
    done.send(()).expect("stop negative-control watchdog");
    watchdog.join().expect("join negative-control watchdog");
    panic!("native dispatch stopped before the external watchdog: {result:?}");
}
