//! Readiness dispatch initializes native instances without invoking their handlers.

use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use tokio::time::{Instant, timeout_at};
use wamn_runtime::engine::{build_engine_with_host_memory, host_memory_budgets};
use wash_runtime::engine::Engine;
use wash_runtime::engine::dispatch::DispatchTarget;
use wash_runtime::engine::workload::{ResolvedWorkload, WorkloadItem};
use wash_runtime::host::http::NullServer;
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin::{HostPlugin, PluginBindings, WitInterfaces};
use wash_runtime::types::{Component, Workload};
use wash_runtime::wit::{WitInterface, WitWorld};

use super::prepare_native;

const OBSERVE: &str = "proof:readiness/observe@1.0.0";
const HANDLER: &str = "proof:readiness/handler@1.0.0";
const CHILD_MARKER: &str = "WAMN_NATIVE_READINESS_CHILD";
const PAGE: usize = 65_536;
const DEADLINE: Duration = Duration::from_millis(200);
const CLEANUP: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
enum Case {
    Success,
    Trap,
    Deadline,
}

fn fixture_bytes(case: Case) -> Vec<u8> {
    let start = match case {
        Case::Success => "",
        Case::Trap => "unreachable",
        Case::Deadline => "(loop br 0)",
    };
    // The handler traps if readiness invokes it. The initializer's host call
    // distinguishes real instantiation from a no-op readiness success.
    wat::parse_str(format!(
        r#"(component
          (import "{OBSERVE}" (instance $observe
            (export "started" (func))
            (export "ran" (func))))
          (core func $started (canon lower (func $observe "started")))
          (core func $ran (canon lower (func $observe "ran")))
          (core module $code
            (import "observe" "started" (func $started))
            (import "observe" "ran" (func $ran))
            (memory 1)
            (func $start call $started {start})
            (start $start)
            (func (export "run") (result i32) call $ran unreachable))
          (core instance $code (instantiate $code
            (with "observe" (instance
              (export "started" (func $started))
              (export "ran" (func $ran))))))
          (func $run (result u32) (canon lift (core func $code "run")))
          (instance $handler (export "run" (func $run)))
          (export "{HANDLER}" (instance $handler)))"#
    ))
    .expect("encode the readiness fixture")
}

#[derive(Debug)]
struct Observer {
    engine: Arc<Engine>,
    starts: Arc<AtomicUsize>,
    runs: Arc<AtomicUsize>,
    allocated_starts: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl HostPlugin for Observer {
    fn id(&self) -> &'static str {
        "native-readiness-proof"
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from(OBSERVE)]),
            exports: HashSet::from([WitInterface::from(HANDLER)]),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let mut instance = item.linker().instance(OBSERVE)?;
        let starts = Arc::clone(&self.starts);
        let allocated_starts = Arc::clone(&self.allocated_starts);
        let engine = Arc::clone(&self.engine);
        instance.func_wrap("started", move |_store, (): ()| {
            starts.fetch_add(1, Ordering::SeqCst);
            if engine.guest_memory().in_use() >= PAGE as u64 {
                allocated_starts.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        })?;
        let runs = Arc::clone(&self.runs);
        instance.func_wrap("ran", move |_store, (): ()| {
            runs.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })?;
        Ok(())
    }
}

struct Fixture {
    engine: Arc<Engine>,
    observer: Arc<Observer>,
    workload: ResolvedWorkload,
    target: DispatchTarget,
}

async fn load_fixture(case: Case) -> Fixture {
    let budgets = host_memory_budgets(4 * PAGE, 1).expect("bounded readiness memory");
    let engine =
        Arc::new(build_engine_with_host_memory(&[], budgets).expect("production native engine"));
    let observer = Arc::new(Observer {
        engine: Arc::clone(&engine),
        starts: Arc::default(),
        runs: Arc::default(),
        allocated_starts: Arc::default(),
    });
    let unresolved = engine
        .initialize_workload(
            "native-readiness-proof",
            Workload {
                namespace: "proof".into(),
                name: "native-readiness-proof".into(),
                annotations: HashMap::new(),
                service: None,
                components: vec![Component {
                    name: "readiness".into(),
                    bytes: fixture_bytes(case).into(),
                    pool_size: 0,
                    max_concurrency: 1,
                    ..Component::default()
                }],
                host_interfaces: vec![WitInterface::from(OBSERVE), WitInterface::from(HANDLER)],
                volumes: Vec::new(),
            },
        )
        .expect("native workload initialization");
    let plugins: HashMap<&'static str, Arc<dyn HostPlugin>> =
        HashMap::from([(observer.id(), Arc::clone(&observer) as Arc<dyn HostPlugin>)]);
    let workload = unresolved
        .resolve(
            Some(&plugins),
            &PluginBindings::new(),
            Arc::new(NullServer::default()),
            &Meters::new(MeterKind::Off),
        )
        .await
        .expect("native workload resolution");
    let id = workload
        .components()
        .read()
        .await
        .values()
        .next()
        .expect("the readiness component exists")
        .id()
        .to_owned();
    let target = workload
        .dispatch_target(&id, observer.id())
        .await
        .expect("native readiness dispatch target");
    assert_eq!(observer.starts.load(Ordering::SeqCst), 0);
    assert_eq!(engine.guest_memory().in_use(), 0);
    Fixture {
        engine,
        observer,
        workload,
        target,
    }
}

async fn assert_memory_returned(engine: &Engine) {
    timeout_at(Instant::now() + CLEANUP, async {
        while engine.guest_memory().in_use() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native readiness returns every guest memory reservation");
}

async fn prove(case: Case) {
    let fixture = load_fixture(case).await;
    let deadline = Instant::now()
        + if matches!(case, Case::Deadline) {
            DEADLINE
        } else {
            CLEANUP
        };
    let result = prepare_native(&fixture.target, deadline).await;
    assert_eq!(fixture.observer.starts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.observer.allocated_starts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.observer.runs.load(Ordering::SeqCst), 0);
    match case {
        Case::Success => {
            result.expect("real initialization succeeds without invoking the trap handler");
            assert_memory_returned(&fixture.engine).await;
            prepare_native(&fixture.target, Instant::now() + CLEANUP)
                .await
                .expect("a second native readiness call initializes a fresh instance");
            assert_eq!(fixture.observer.starts.load(Ordering::SeqCst), 2);
            assert_eq!(fixture.observer.allocated_starts.load(Ordering::SeqCst), 2);
            assert_eq!(fixture.observer.runs.load(Ordering::SeqCst), 0);
        }
        Case::Trap => {
            let error = result.expect_err("a trapping initializer cannot report ready");
            assert!(format!("{error:#}").contains("unreachable"), "{error:#}");
        }
        Case::Deadline => {
            let error =
                result.expect_err("an infinite initializer must reach the enclosing deadline");
            assert!(Instant::now() >= deadline);
            assert!(Instant::now() < deadline + CLEANUP);
            assert!(
                format!("{error:#}").contains("native readiness enclosing deadline elapsed"),
                "{error:#}"
            );
        }
    }
    assert_memory_returned(&fixture.engine).await;
    fixture
        .workload
        .unbind_all_plugins()
        .await
        .expect("unbind readiness proof workload");
}

fn isolated(name: &str, case: Case) {
    if std::env::var(CHILD_MARKER).as_deref() != Ok(name) {
        let full_name = format!("router_driver::native_call::tests::{name}");
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", &full_name, "--nocapture"])
            .env(CHILD_MARKER, name)
            .output()
            .expect("start isolated readiness proof");
        assert!(
            output.status.success(),
            "{full_name}: {}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    let (done, finished) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(60)) == Err(mpsc::RecvTimeoutError::Timeout) {
            eprintln!("native readiness proof exceeded its process watchdog");
            std::process::exit(124);
        }
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .event_interval(1)
        .enable_all()
        .build()
        .expect("isolated readiness runtime");
    runtime.block_on(prove(case));
    drop(runtime);
    done.send(()).expect("finish watchdog");
    watchdog.join().expect("join watchdog");
}

#[test]
fn native_readiness_initializes_without_running_the_handler() {
    isolated(
        "native_readiness_initializes_without_running_the_handler",
        Case::Success,
    );
}

#[test]
fn native_readiness_refuses_a_trapping_initializer() {
    isolated(
        "native_readiness_refuses_a_trapping_initializer",
        Case::Trap,
    );
}

#[test]
fn native_readiness_bounds_initialization_and_returns_memory() {
    isolated(
        "native_readiness_bounds_initialization_and_returns_memory",
        Case::Deadline,
    );
}
