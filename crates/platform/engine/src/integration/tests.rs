use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
use wit_parser::Resolve;

use self::node_bindings::wamn::node::types::{Emission, NodeContext, NodeError};

use super::*;
use crate::component_admission::{ComponentAdmissionErrorType, validate_integration_admission};

mod node_bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: "../../../wit/wamn-node",
        world: "node",
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

fn guest(world: &str, wat: &str, labels: &[String]) -> anyhow::Result<Vec<u8>> {
    let mut resolve = Resolve::new();
    resolve.push_str("io.wit", include_str!("io.wit"))?;
    resolve.push_str("sockets.wit", include_str!("sockets.wit"))?;
    resolve.push_str(
        "integration.wit",
        include_str!("../../../../../wit/wamn-integration/package.wit"),
    )?;
    resolve.push_str(
        "node.wit",
        include_str!("../../../../../wit/wamn-node/package.wit"),
    )?;
    let mut imports = String::new();
    for name in labels {
        write!(imports, "import {name}: protocol;")?;
    }
    let package = resolve.push_str(
        "fixture.wit",
        &format!(r"
        package test:integration;
        interface protocol {{
          variant error {{ unbound, unavailable, authority-denied, credential-unavailable, timeout, transport(string) }}
          get: func(request: list<u8>) -> result<list<u8>, error>;
        }}
        world socket-integration {{
          import wamn:integration/bound@0.1.0;
          import wasi:sockets/instance-network@0.2.12;
          import wasi:sockets/tcp-create-socket@0.2.12;
          import wasi:sockets/tcp@0.2.12;
          import wasi:io/poll@0.2.12;
          import wasi:io/streams@0.2.12;
          export protocol;
          export wamn:integration/health@0.1.0;
        }}
        world application {{
          {imports}
          export wamn:node/handler@0.1.0;
        }}
    "),
    )?;
    let world = resolve.select_world(&[package], Some(world))?;
    let mut module = wat::parse_str(wat)?;
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8)?;
    ComponentEncoder::default()
        .module(&module)?
        .validate(true)
        .encode()
}

fn socket_integration(ip: std::net::Ipv4Addr, delta: i32) -> anyhow::Result<Vec<u8>> {
    let wat = include_str!("integration.wat").replace("PORT_DELTA", &delta.to_string());
    let wat = ["HOST_A", "HOST_B", "HOST_C", "HOST_D"]
        .into_iter()
        .zip(ip.octets())
        .fold(wat, |wat, (key, value)| {
            wat.replace(key, &value.to_string())
        });
    guest("socket-integration", &wat, &[])
}

#[test]
fn integration_byte_admission_refuses_postgres_blobstore_and_other_effects() -> anyhow::Result<()> {
    let engine = crate::build_engine(&[])?;
    for interface in [
        "wamn:postgres/statements@0.4.0",
        "wamn:postgres/client@0.4.0",
        "wasmcloud:blobstore/blobstore@0.1.0",
        "wasi:http/outgoing-handler@0.2.12",
        "wamn:integration/health@0.1.0",
        "wasi:filesystem/types@0.2.12",
        "wasi:sockets/tcp@0.3.0",
    ] {
        let bytes = wat::parse_str(format!(
            r#"(component (import "{interface}" (instance (export "effect" (func)))))"#
        ))?;
        let error = validate_integration_admission(&engine, &bytes, "forbidden")
            .expect_err("integration effect must refuse");
        assert_eq!(
            error.error_type(),
            ComponentAdmissionErrorType::ImportPolicyRefused,
            "{interface}"
        );
        assert!(error.to_string().contains(interface));
    }
    Ok(())
}

#[test]
fn integration_byte_admission_refuses_missing_health_and_wrong_signature() -> anyhow::Result<()> {
    let engine = crate::build_engine(&[])?;
    let empty = wat::parse_str("(component)")?;
    let mut resolve = Resolve::new();
    let package = resolve.push_str(
        "wrong.wit",
        r"
        package wamn:integration@0.1.0;
        interface health { check: func(request: string) -> u32; }
        world integration { export health; }
    ",
    )?;
    let world = resolve.select_world(&[package], Some("integration"))?;
    let mut module =
        wit_component::dummy_module(&resolve, world, wit_parser::ManglingAndAbi::Standard32);
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8)?;
    let wrong = ComponentEncoder::default()
        .module(&module)?
        .validate(true)
        .encode()?;
    for bytes in [empty, wrong] {
        let error = validate_integration_admission(&engine, &bytes, "wrong-health")
            .expect_err("the exact health contract is required");
        assert_eq!(
            error.error_type(),
            ComponentAdmissionErrorType::IntegrationExportMismatch
        );
    }
    Ok(())
}

#[test]
fn integration_binding_refuses_metadata_and_nonexact_authorities() {
    for authority in [
        "169.254.1.1:80",
        "169.254.169.254:80",
        "[fe80::1]:80",
        "[::ffff:169.254.169.254]:80",
        "[fd00:ec2::254]:80",
        "metadata.google.internal:80",
        "METADATA.GOOGLE.INTERNAL.:80",
        "metadata:80",
        "instance-data.ec2.internal:80",
        "host.wasmcloud.internal:80",
        "*.example.com:80",
        "user@example.com:80",
        "example.com",
        "example.com:0",
    ] {
        assert!(
            IntegrationBinding::new(authority, None).is_err(),
            "{authority}"
        );
    }
    for authority in [
        "10.0.0.2:502",
        "192.168.1.2:502",
        "172.16.0.2:502",
        "[fd00::1]:502",
        "plc.example.com:502",
    ] {
        IntegrationBinding::new(authority, None).expect("an exact private authority is allowed");
    }
    let binding =
        IntegrationBinding::new("plc.example.com:502", Some("private-secret".to_owned())).unwrap();
    assert!(!format!("{binding:?}").contains("private-secret"));
}

#[test]
fn integration_address_policy_requires_a_private_ip_literal() {
    for (authority, allow_private) in [
        ("10.0.0.2:502", true),
        ("192.168.1.2:502", true),
        ("172.16.0.2:502", true),
        ("100.64.0.1:502", true),
        ("[fd00::1]:502", true),
        ("[::ffff:10.0.0.2]:502", true),
        ("8.8.8.8:502", false),
        ("[2606:4700:4700::1111]:502", false),
        ("plc.example.com:502", false),
    ] {
        let authority = authority.parse().expect("authority");
        let policy = integration_address_policy(&authority);
        assert_eq!(policy.allow_private, allow_private, "{authority}");
        assert_eq!(policy.permits("10.0.0.2".parse().unwrap()), allow_private);
        assert!(policy.permits("8.8.8.8".parse().unwrap()));
        for denied in [
            "169.254.1.1",
            "fe80::1",
            "fd00:ec2::254",
            "::ffff:169.254.169.254",
        ] {
            assert!(
                !policy.permits(denied.parse().unwrap()),
                "{authority}: {denied}"
            );
        }
    }
}

async fn read(
    integration: &Integration,
    request: Vec<u8>,
) -> anyhow::Result<Result<Vec<u8>, String>> {
    let result = integration
        .call(
            "test:integration/protocol",
            "get",
            &[Val::List(request.into_iter().map(Val::U8).collect())],
        )
        .await?;
    match result.as_slice() {
        [Val::Result(Ok(Some(value)))] => match value.as_ref() {
            Val::List(values) => Ok(Ok(values
                .iter()
                .map(|value| match value {
                    Val::U8(byte) => *byte,
                    _ => panic!("byte"),
                })
                .collect())),
            _ => anyhow::bail!("fixture returned no bytes"),
        },
        [Val::Result(Err(Some(value)))] => match value.as_ref() {
            Val::Variant(name, _) => Ok(Err(name.clone())),
            _ => anyhow::bail!("fixture returned no error"),
        },
        _ => anyhow::bail!("fixture returned an invalid result"),
    }
}

async fn private_listener() -> anyhow::Result<(tokio::net::TcpListener, std::net::Ipv4Addr)> {
    // Connecting this UDP socket selects a local route and sends no datagram.
    let route = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    route.connect("10.255.255.254:9").await?;
    let std::net::IpAddr::V4(ip) = route.local_addr()?.ip() else {
        anyhow::bail!("fixture needs an IPv4 route");
    };
    anyhow::ensure!(
        ip.is_private(),
        "fixture needs a private interface, got {ip}"
    );
    Ok((tokio::net::TcpListener::bind((ip, 0)).await?, ip))
}

fn server(
    listener: tokio::net::TcpListener,
    credential: &'static str,
    status: u16,
) -> (Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let accepts = Arc::new(AtomicUsize::new(0));
    let count = accepts.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            let mut request = Vec::new();
            loop {
                request.push(socket.read_u8().await.unwrap());
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
                assert!(request.len() < 4096);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.contains(&format!("Authorization: Bearer {credential}\r\n")));
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 {status} OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    (accepts, task)
}

#[tokio::test]
async fn enforced_integration_reaches_bound_private_address_and_never_opens_unbound_connection()
-> anyhow::Result<()> {
    // The application engine keeps its production Count default.
    let _application_engine = crate::build_engine(&[])?;
    let (listener, ip) = private_listener().await?;
    let port = listener.local_addr()?.port();
    let (accepts, allowed_server) = server(listener, "private-secret", 200);
    let denied_listener = tokio::net::TcpListener::bind((ip, 0)).await?;
    let denied_port = denied_listener.local_addr()?.port();
    let (denied_accepts, denied_server) = server(denied_listener, "private-secret", 200);
    for (authority, delta, expected) in [
        (format!("{ip}:{port}"), 0, true),
        (
            format!("{ip}:{port}"),
            i32::from(denied_port) - i32::from(port),
            false,
        ),
        (format!("10.255.255.253:{port}"), 0, false),
    ] {
        let integration = Integration::load(
            &socket_integration(ip, delta)?,
            IntegrationBinding::new(&authority, Some("private-secret".to_owned()))?,
        )
        .await?;
        let before = accepts.load(Ordering::SeqCst);
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            read(&integration, vec![0, 255, 128, 1]),
        )
        .await??;
        if expected {
            assert_eq!(result.unwrap(), b"200\0\xff");
            assert_eq!(accepts.load(Ordering::SeqCst), before + 1);
        } else {
            assert!(matches!(result, Err(ref error) if error == "authority-denied"));
            assert_eq!(
                accepts.load(Ordering::SeqCst),
                before,
                "a refused connection never reaches the listener"
            );
        }
    }
    assert_eq!(denied_accepts.load(Ordering::SeqCst), 0);
    allowed_server.abort();
    denied_server.abort();
    Ok(())
}

struct Application {
    store: Store<SharedCtx>,
    run: wash_runtime::wasmtime::component::TypedFunc<
        (NodeContext, String),
        (Result<Emission, NodeError>,),
    >,
    context: NodeContext,
    workload: ResolvedWorkload,
    _egress: Arc<dyn HostHandler>,
}

impl Application {
    async fn run(&mut self, connection: &str) -> anyhow::Result<Emission> {
        let (result,) = tokio::time::timeout(
            Duration::from_secs(5),
            self.run.call_async(
                &mut self.store,
                (self.context.clone(), connection.to_owned()),
            ),
        )
        .await??;
        Ok(result.expect("fixture node returned an emission"))
    }
}

async fn application(
    connections: HashMap<String, Arc<Integration>>,
) -> anyhow::Result<Application> {
    let engine = crate::build_engine(&[])?;
    let mut labels: Vec<_> = connections.keys().cloned().collect();
    labels.sort();
    if labels.is_empty() {
        labels.push("fixture".into());
    }
    let mut imports = String::new();
    for (index, label) in labels.iter().enumerate() {
        write!(
            imports,
            r#"(import "{label}" "get" (func $get-{index} (param i32 i32 i32)))"#
        )?;
    }
    let call = if labels.len() == 1 {
        "i32.const 2048 i32.const 4 i32.const 16 call $get-0".to_owned()
    } else {
        format!(
            "(if (i32.eq (i32.load8_u (i32.load offset=96 (local.get $args))) (i32.const {})) (then i32.const 2048 i32.const 4 i32.const 16 call $get-1) (else i32.const 2048 i32.const 4 i32.const 16 call $get-0))",
            labels[1].as_bytes()[0]
        )
    };
    let wat = include_str!("application.wat")
        .replace("IMPORTS", &imports)
        .replace("CALL", &call);
    let bytes = guest("application", &wat, &labels)?;
    let plugin: Arc<dyn HostPlugin> = Arc::new(IntegrationImports::new(
        connections,
        HashSet::from(["test:integration/protocol".to_owned()]),
    ));
    let plugins = HashMap::from([(plugin.id(), plugin)]);
    let egress: Arc<dyn HostHandler> = Arc::new(NullServer::default());
    let unresolved = engine.initialize_workload(
        "application",
        Workload {
            namespace: "test".to_owned(),
            name: "application".to_owned(),
            components: vec![Component {
                name: "application".to_owned(),
                bytes: bytes.into(),
                ..Component::default()
            }],
            host_interfaces: vec![WitInterface::from("test:integration/protocol")],
            annotations: HashMap::new(),
            service: None,
            volumes: Vec::new(),
        },
    )?;
    let workload = unresolved
        .resolve(
            Some(&plugins),
            &PluginBindings::new(),
            &HostRef::from_handler(&egress),
            &Meters::new(MeterKind::Off),
        )
        .await?;
    let id = workload
        .components()
        .read()
        .await
        .keys()
        .next()
        .unwrap()
        .to_string();
    let mut store = workload.new_store(&id).await?;
    store.set_epoch_deadline(u64::MAX / 2);
    let instance = workload
        .instantiate_pre(&id)
        .await?
        .instantiate_async(&mut store)
        .await?;
    let handler = instance
        .get_export_index(&mut store, None, "wamn:node/handler@0.1.0")
        .context("application handler")?;
    let run = instance
        .get_export_index(&mut store, Some(&handler), "run")
        .context("application run")?;
    let run = instance
        .get_typed_func::<(NodeContext, String), (Result<Emission, NodeError>,)>(&mut store, run)?;
    let context = NodeContext {
        wiring_id: "fixture".to_owned(),
        wiring_version: 1,
        node_id: "reader".to_owned(),
        delivery_id: "delivery".to_owned(),
        input_port: None,
        occurrence: 0,
        traceparent: None,
        tracestate: None,
        deadline_ms: None,
        config: "{}".to_owned(),
    };
    Ok(Application {
        store,
        run,
        context,
        workload,
        _egress: egress,
    })
}

#[tokio::test]
async fn application_calls_production_integration_imports_in_a_separate_store() -> anyhow::Result<()>
{
    let (listener, ip) = private_listener().await?;
    let port = listener.local_addr()?.port();
    let (accepts, server) = server(listener, "private-secret", 200);
    let integration = Arc::new(
        Integration::load(
            &socket_integration(ip, 0)?,
            IntegrationBinding::new(&format!("{ip}:{port}"), Some("private-secret".to_owned()))?,
        )
        .await?,
    );
    let mut app = application(HashMap::from([("fixture".to_owned(), integration.clone())])).await?;
    for _ in 0..2 {
        assert_eq!(app.run("fixture").await?.payload, "200");
    }
    assert_eq!(accepts.load(Ordering::SeqCst), 2);
    assert_ne!(
        &app.store.data().active_ctx.store_id,
        &integration
            .inner
            .lock()
            .await
            .store
            .data()
            .active_ctx
            .store_id
    );
    server.abort();
    Ok(())
}

#[tokio::test]
async fn application_routes_two_named_integrations_and_refuses_unbound() -> anyhow::Result<()> {
    let (modbus_listener, ip) = private_listener().await?;
    let modbus_port = modbus_listener.local_addr()?.port();
    let printer_listener = tokio::net::TcpListener::bind((ip, 0)).await?;
    let printer_port = printer_listener.local_addr()?.port();
    let (modbus_accepts, modbus_server) = server(modbus_listener, "modbus-secret", 200);
    let (printer_accepts, printer_server) = server(printer_listener, "printer-secret", 201);
    let bytes = socket_integration(ip, 0)?;
    let modbus = Arc::new(
        Integration::load(
            &bytes,
            IntegrationBinding::new(
                &format!("{ip}:{modbus_port}"),
                Some("modbus-secret".to_owned()),
            )?,
        )
        .await?,
    );
    let printer = Arc::new(
        Integration::load(
            &bytes,
            IntegrationBinding::new(
                &format!("{ip}:{printer_port}"),
                Some("printer-secret".to_owned()),
            )?,
        )
        .await?,
    );
    let mut app = application(HashMap::from([
        ("modbus".to_owned(), modbus.clone()),
        ("printer".to_owned(), printer.clone()),
    ]))
    .await?;
    assert_eq!(app.run("modbus").await?.payload, "200");
    assert_eq!(modbus_accepts.load(Ordering::SeqCst), 1);
    assert_eq!(printer_accepts.load(Ordering::SeqCst), 0);
    assert_eq!(app.run("printer").await?.payload, "201");
    let error = application(HashMap::new())
        .await
        .err()
        .expect("an unbound label refuses instantiation");
    assert!(format!("{error:#}").contains("unbound"));
    assert_eq!(modbus_accepts.load(Ordering::SeqCst), 1);
    assert_eq!(printer_accepts.load(Ordering::SeqCst), 1);
    let modbus = modbus.inner.lock().await;
    let printer = printer.inner.lock().await;
    assert_ne!(
        modbus.store.data().active_ctx.store_id,
        printer.store.data().active_ctx.store_id
    );
    assert_ne!(
        app.store.data().active_ctx.store_id,
        modbus.store.data().active_ctx.store_id
    );
    assert_ne!(
        app.store.data().active_ctx.store_id,
        printer.store.data().active_ctx.store_id
    );
    modbus_server.abort();
    printer_server.abort();
    Ok(())
}

#[tokio::test]
async fn an_undeclared_name_refuses_before_dispatch() -> anyhow::Result<()> {
    let integration = Arc::new(
        Integration::load(
            &socket_integration("192.168.1.2".parse()?, 0)?,
            IntegrationBinding::new("192.168.1.2:502", None)?,
        )
        .await?,
    );
    let connections = HashMap::from([("fixture".to_owned(), integration)]);
    let application = application(connections.clone()).await?;
    let imports = Arc::new(IntegrationImports::new(
        connections,
        HashSet::from(["test:integration/protocol".to_owned()]),
    ));
    let components = application.workload.components();
    let mut components = components.write().await;
    let component = components
        .values_mut()
        .next()
        .expect("application component");
    let error = imports
        .add_to_linker(&mut WorkloadItem::Component(component), &HashSet::new())
        .await
        .expect_err("an undeclared label refuses before dispatch");
    assert!(error.to_string().contains("connection undeclared: fixture"));
    Ok(())
}
