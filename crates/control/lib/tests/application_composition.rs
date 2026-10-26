//! Publish-time HTTP composition, without loading a served workload.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use wamn_control::component_admission::{
    AdmitComponentRequest, ComponentAdmission, admit_component,
};
use wamn_control::publish_release::compose_application;
use wamn_engine::component_admission::component_digest;

const OPERATION: &str = "source-fixture:item/get@1.0.0";
const HANDLER: &str = "wasi:http/handler@0.3.0";

fn component(resolve: &wit_parser::Resolve, world: wit_parser::WorldId) -> Vec<u8> {
    let mut module =
        wit_component::dummy_module(resolve, world, wit_parser::ManglingAndAbi::Standard32);
    wit_component::embed_component_metadata(
        &mut module,
        resolve,
        world,
        wit_component::StringEncoding::UTF8,
    )
    .expect("embed fixture metadata");
    wit_component::ComponentEncoder::default()
        .module(&module)
        .expect("read fixture module")
        .validate(true)
        .encode()
        .expect("encode fixture component")
}

fn application(extra: &str) -> Vec<u8> {
    let mut resolve = wit_parser::Resolve::new();
    resolve
        .push_str(
            "node.wit",
            include_str!("../../../execution/workflow/router/wit/package.wit"),
        )
        .expect("read node contract");
    resolve
        .push_str(
            "routing.wit",
            include_str!("../../../platform/runtime/wit/deps/wamn-flow-http-routing/package.wit"),
        )
        .expect("read routing contract");
    resolve
        .push_str(
            "postgres.wit",
            include_str!("../../../platform/runtime/wit/deps/wamn-postgres-0.4/package.wit"),
        )
        .expect("read PostgreSQL contract");
    resolve
        .push_str(
            "operation.wit",
            include_str!("../../../platform/runtime/wit/deps/wamn-operation-0.1/package.wit"),
        )
        .expect("read operation contract");
    let package = resolve
        .push_str(
            "application.wit",
            &format!(
                "package source-fixture:item@1.0.0;
                 interface get {{
                   use wamn:node/types@0.1.0.{{node-context, emission, node-error}};
                   run: func(context: node-context, input: string) -> result<emission, node-error>;
                 }}
                 world application {{ export get; {extra} }}"
            ),
        )
        .expect("read application contract");
    let world = resolve
        .select_world(&[package], None)
        .expect("resolve application");
    component(&resolve, world)
}

fn admit(bytes: &[u8]) -> anyhow::Result<ComponentAdmission> {
    admit_declared(bytes, &[])
}

/// Admit `bytes` under the fixture declaration with the `extra` operations added.
fn admit_declared(bytes: &[u8], extra: &[&str]) -> anyhow::Result<ComponentAdmission> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let stem = std::env::temp_dir().join(format!(
        "wamn-application-composition-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let path = stem.with_extension("wasm");
    std::fs::write(&path, bytes).expect("save fixture component");
    let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/component_package");
    let mut declaration: serde_json::Value = serde_json::from_slice(
        &std::fs::read(package.join("component.json")).expect("read fixture declaration"),
    )
    .expect("parse fixture declaration");
    for operation in extra {
        // A platform node operation registers no operation of its own.
        let mut entry = declaration["operations"][OPERATION].clone();
        entry
            .as_object_mut()
            .expect("an operation declaration")
            .remove("registered-operation");
        declaration["operations"][*operation] = entry;
    }
    let declaration_path = stem.with_extension("json");
    std::fs::write(&declaration_path, declaration.to_string()).expect("save fixture declaration");
    let result = admit_component(AdmitComponentRequest {
        declaration: declaration_path.clone(),
        package,
        component_bytes: path.clone(),
        // A platform node operation needs its platform package admitted.
        admitted_platform_packages: if extra.is_empty() {
            Vec::new()
        } else {
            vec!["wamn:node".to_owned()]
        },
        base_digests: BTreeSet::new(),
    });
    std::fs::remove_file(path).expect("remove fixture component");
    std::fs::remove_file(declaration_path).expect("remove fixture declaration");
    result
}

fn route() -> Vec<u8> {
    let mut resolve = wit_parser::Resolve::new();
    let package = resolve
        .push_str(
            "route.wit",
            "package wasi:http@0.3.0;
             interface handler { handle: func(); }
             world route { export handler; }",
        )
        .expect("read fixture route");
    let world = resolve
        .select_world(&[package], None)
        .expect("resolve route");
    component(&resolve, world)
}

fn assert_composition(route: &[u8]) {
    let application = admit(&application("")).expect("admit the application");
    let pin = component_digest(route);
    let first = compose_application(&application, route, &pin).expect("first composition");
    let second = compose_application(&application, route, &pin).expect("second composition");
    assert_eq!(first.bytes(), second.bytes());
    assert_eq!(first.digest(), second.digest(), "two builds, one digest");
    assert_ne!(first.digest(), application.component_digest());
    assert_eq!(first.digest(), component_digest(first.bytes()));

    let engine = wamn_engine::build_engine(&[]).expect("build inspection engine");
    let component =
        wash_runtime::wasmtime::component::Component::new(engine.inner(), first.bytes())
            .expect("the composed artifact compiles");
    let exports = component
        .component_type()
        .exports(engine.inner())
        .map(|(name, _)| name.to_owned())
        .collect::<BTreeSet<_>>();
    assert!(exports.contains(HANDLER));
    assert!(exports.contains(OPERATION));
    let error = compose_application(&application, route, application.component_digest())
        .expect_err("a stale route pin refuses");
    assert!(
        error.to_string().contains("route layer bytes differ"),
        "{error}"
    );
}

#[test]
fn publication_builds_two_identical_http_applications_from_pinned_bytes() {
    assert_composition(&route());
}

#[test]
fn publication_records_the_exact_entry_and_members() {
    use wamn_catalog::{PackageCoordinate, ServingComponent, ServingManifest, ServingRelease};
    use wamn_control::publish_release::{PublishedRelease, record_composed_entry};

    let application = admit(&application("")).unwrap();
    let route = route();
    let pin = component_digest(&route);
    let entry = compose_application(&application, &route, &pin).unwrap();
    let fact = application.facts();
    let manifest = ServingManifest::new(
        ServingRelease {
            packages: BTreeSet::from([PackageCoordinate::new(
                &fact.scope.package_id,
                &fact.scope.package_version,
            )
            .unwrap()]),
        },
        BTreeSet::from([ServingComponent::project(fact, Vec::new()).unwrap()]),
        BTreeSet::new(),
        BTreeMap::new(),
    )
    .unwrap();
    let mut published = PublishedRelease {
        digest: manifest.digest(),
        canonical_bytes: manifest.canonical_bytes(),
        manifest,
    };
    let before = published.digest.clone();
    let reference = format!("entries@sha256:{}", "a".repeat(64));
    record_composed_entry(&mut published, &entry, &reference).unwrap();
    let recorded = published.manifest.composed_entry.as_ref().unwrap();
    assert_eq!(recorded.reference, reference);
    assert_eq!(recorded.route_digest, pin);
    assert_eq!(recorded.application.package_id, fact.scope.package_id);
    assert_eq!(recorded.application.component, fact.component);
    assert_eq!(recorded.application.digest, fact.component_digest);
    assert_ne!(before, published.digest);
    assert_eq!(
        ServingManifest::from_canonical_bytes(&published.canonical_bytes).unwrap(),
        (published.manifest, published.digest)
    );
}

#[test]
#[ignore = "requires: WAMN_FLOW_HTTP_COMPONENT built from the http-route guest"]
fn publication_composes_only_the_application_that_covers_the_routes() {
    use wamn_catalog::{PackageCoordinate, ServingComponent, ServingManifest, ServingRelease};
    use wamn_control::publish_release::{PublishedRelease, compose_release_entry};

    let app_bytes = application("");
    // A released workflow node is larger than the application in WMS, and
    // the real jsonata, blob-put and label-render nodes refuse composition
    // with the route layer ("package `wamn:node@0.1.0` creates a dependency
    // cycle", platform_apply_kind 4.14). Publish selects by route coverage
    // first, so it composes only the application.
    let node_bytes = application("export wamn:node/handler@0.1.0;");
    assert!(node_bytes.len() > app_bytes.len());
    let app = admit(&app_bytes).unwrap();
    let node = admit_declared(&node_bytes, &["wamn:node/handler@0.1.0"]).unwrap();
    let route = std::fs::read(
        std::env::var_os("WAMN_FLOW_HTTP_COMPONENT")
            .expect("set WAMN_FLOW_HTTP_COMPONENT to the pinned http-route guest"),
    )
    .expect("read the built HTTP route guest");
    let pin = component_digest(&route);
    let error = compose_application(&node, &route, &pin).expect_err("a node does not compose");
    assert!(
        format!("{error:#}").contains("dependency cycle"),
        "{error:#}"
    );
    let expected = compose_application(&app, &route, &pin).unwrap();

    let fact = app.facts();
    let manifest = ServingManifest::new(
        ServingRelease {
            packages: BTreeSet::from([PackageCoordinate::new(
                &fact.scope.package_id,
                &fact.scope.package_version,
            )
            .unwrap()]),
        },
        BTreeSet::from([ServingComponent::project(fact, Vec::new()).unwrap()]),
        BTreeSet::from([serde_json::from_value(serde_json::json!({
            "package-id": fact.scope.package_id.as_str(),
            "component": fact.component.as_str(),
            "operation": OPERATION,
            "type": "get",
        }))
        .unwrap()]),
        BTreeMap::new(),
    )
    .unwrap();
    let published = PublishedRelease {
        digest: manifest.digest(),
        canonical_bytes: manifest.canonical_bytes(),
        manifest,
    };
    let applications = [(
        "fixture".into(),
        wamn_catalog::ApplicationDefinition {
            packages: [fact.scope.package_id.clone()].into(),
        },
    )]
    .into();
    let entry = compose_release_entry(&published, &[node, app], &route, &applications, None)
        .expect("publish selects the covering application");
    assert_eq!(entry.digest(), expected.digest());
}

#[test]
fn publication_refuses_a_member_with_operation_authority() {
    let bytes = application("import wamn:operation/operation@0.1.0;");
    let error = admit(&bytes).expect_err("entry-only authority refuses");
    assert!(
        format!("{error:#}").contains("entry-only package wamn:operation"),
        "{error:#}"
    );
}

#[test]
fn publication_refuses_an_application_with_its_own_http_handler() {
    let error = admit(&route()).expect_err("application-owned handler refuses");
    assert!(
        format!("{error:#}").contains("supplies its own wasi:http/handler"),
        "{error:#}"
    );
}

#[test]
fn composition_preserves_overlay_exports_that_use_base_types() {
    let mut resolve = wit_parser::Resolve::new();
    let base_package = resolve
        .push_str(
            "base.wit",
            "package test:zbase@1.0.0;
             interface types { record value { x: u32 } }
             interface op { use types.{value}; run: func() -> value; }
             world base { export types; export op; }",
        )
        .expect("read base contract");
    let world = resolve
        .select_world(&[base_package], None)
        .expect("resolve base");
    let base = component(&resolve, world);
    let overlay_package = resolve
        .push_str(
            "overlay.wit",
            "package test:aoverlay@1.0.0;
             interface op { use test:zbase/types@1.0.0.{value}; run: func() -> value; }
             world overlay { import test:zbase/op@1.0.0; export op; }",
        )
        .expect("read overlay contract");
    let world = resolve
        .select_world(&[overlay_package], None)
        .expect("resolve overlay");
    let application = wamn_component_composer::compose(component(&resolve, world), vec![base])
        .expect("compose the typed overlay");
    let composed = wamn_component_composer::compose_application(route(), application)
        .expect("preserve the base type before the overlay export");
    let engine = wamn_engine::build_engine(&[]).expect("build inspection engine");
    wash_runtime::wasmtime::component::Component::new(engine.inner(), composed)
        .expect("the typed application wrapper compiles");
}

#[test]
fn composition_validates_sql_ownership_through_the_route_wrapper() {
    let member = |imports: &str, export: &str| {
        wat::parse_str(format!(
            r#"(component
                (import "wamn:postgres/statements@0.4.0" (instance))
                {imports}
                (instance $operation)
                (export "{export}" (instance $operation))
            )"#
        ))
        .expect("encode SQL member")
    };
    let base = member("", "test:base/run@1.0.0");
    let overlay = member(
        r#"(import "test:base/run@1.0.0" (instance))"#,
        "test:overlay/run@1.0.0",
    );
    let application =
        wamn_component_composer::compose(overlay, vec![base]).expect("compose the SQL application");
    let expected = wamn_engine::statement_instances::statement_instances(&application)
        .expect("validate the application's SQL ownership");
    let composed = wamn_component_composer::compose_application(route(), application)
        .expect("wrap the application with the route layer");
    assert_eq!(
        wamn_engine::statement_instances::statement_instances(&composed)
            .expect("validate SQL ownership through the wrapper"),
        expected
    );

    // Change only the outer record, leaving both nested members unchanged.
    let mut changed = composed;
    let digest = expected.values().next().expect("the application has SQL");
    let offset = changed
        .windows(digest.len())
        .rposition(|bytes| bytes == digest.as_bytes())
        .expect("the outer record holds the member digest");
    let digit = &mut changed[offset + "sha256:".len()];
    *digit = if *digit == b'a' { b'b' } else { b'a' };
    let error = wamn_engine::statement_instances::statement_instances(&changed)
        .expect_err("a substituted owner refuses");
    assert!(
        error.to_string().contains("but the record names"),
        "{error}"
    );
}

#[test]
fn composition_refuses_a_wrapper_that_forwards_sql_to_the_wrong_leaf() {
    let leaf = |name| {
        format!(
            r#"(component
                (import "wamn:postgres/statements@0.4.0" (instance))
                (instance $operation)
                (export "test:{name}/run@1.0.0" (instance $operation))
            )"#
        )
    };
    let base = leaf("base");
    let overlay = leaf("overlay");
    let expected = BTreeMap::from([
        (
            "sql-0".to_owned(),
            component_digest(&wat::parse_str(&base).expect("encode base")),
        ),
        (
            "sql-1".to_owned(),
            component_digest(&wat::parse_str(&overlay).expect("encode overlay")),
        ),
    ]);
    let record = serde_json::to_string(&serde_json::to_string(&expected).expect("encode record"))
        .expect("quote record for WAT");
    let imports = r#"
        (import "sql-0" (implements "wamn:postgres/statements@0.4.0") (instance))
        (import "sql-1" (implements "wamn:postgres/statements@0.4.0") (instance))
    "#;
    let application = format!(
        r#"(component
            {imports}
            {base}
            {overlay}
            (instance (instantiate 0 (with "wamn:postgres/statements@0.4.0" (instance 0))))
            (instance (instantiate 1 (with "wamn:postgres/statements@0.4.0" (instance 1))))
            (@custom "wamn:statement-instances" {record})
        )"#
    );
    let mut config = wash_runtime::wasmtime::Config::new();
    config.wasm_component_model_implements(true);
    let engine = wash_runtime::wasmtime::Engine::new(&config).expect("build inspection engine");
    for (first, second) in [(0, 1), (1, 0)] {
        let wrapper = wat::parse_str(format!(
            r#"(component
                {imports}
                {application}
                (instance (instantiate 0
                    (with "sql-0" (instance {first}))
                    (with "sql-1" (instance {second}))))
                (@custom "wamn:statement-instances" {record})
            )"#
        ))
        .expect("encode wrapper");
        wash_runtime::wasmtime::component::Component::new(&engine, &wrapper)
            .expect("both forwarding graphs are valid components");
        let result = wamn_engine::statement_instances::statement_instances(&wrapper);
        if first == 0 {
            assert_eq!(
                result.expect("correct forwarding preserves ownership"),
                expected
            );
        } else {
            let error = result.expect_err("forwarding to the wrong leaf refuses");
            assert!(
                error.to_string().contains("but the record names"),
                "{error}"
            );
        }
    }
}

#[test]
#[ignore = "requires: WAMN_FLOW_HTTP_COMPONENT built from the http-route guest"]
fn publication_composes_the_built_http_route_guest() {
    let path = PathBuf::from(
        std::env::var_os("WAMN_FLOW_HTTP_COMPONENT")
            .expect("set WAMN_FLOW_HTTP_COMPONENT to the debug http-route guest"),
    );
    assert_composition(&std::fs::read(path).expect("read the built HTTP route guest"));
}

#[test]
#[ignore = "requires: WAMN_FLOW_HTTP_COMPONENT built from the http-route guest"]
fn platform_control_entry_resolves_dispatch_and_requires_the_route_pin() {
    let path = std::env::var_os("WAMN_FLOW_HTTP_COMPONENT").expect("built route guest");
    let mut route = std::fs::read(path).expect("read the built HTTP route guest");
    let bytes = wamn_component_composer::compose_control_entry(route.clone())
        .expect("compose the platform control entry");
    wamn_component_composer::validate_application_dispatch(&bytes)
        .expect("the host supplies no private dispatch");
    let engine = wamn_engine::build_engine(&[]).expect("build inspection engine");
    let component = wash_runtime::wasmtime::component::Component::new(engine.inner(), bytes)
        .expect("the control entry compiles");
    let exports = component
        .component_type()
        .exports(engine.inner())
        .map(|(name, _)| name.to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(exports, BTreeSet::from([HANDLER.to_owned()]));
    route.push(0);
    let error = wamn_component_composer::compose_control_entry(route)
        .expect_err("the platform control entry requires the pinned guest");
    assert!(
        error
            .to_string()
            .contains("differs from the platform route pin")
    );
}
