use std::collections::BTreeMap;
use wamn_digest::bytes_sha256;
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
    let composed = application;
    let engine = wamn_engine::build_engine(&[]).expect("build inspection engine");
    wash_runtime::wasmtime::component::Component::new(engine.inner(), composed)
        .expect("the typed application wrapper compiles");
}

#[test]
fn composition_refuses_a_substituted_statement_owner() {
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
    // Change only the outer record, leaving both nested members unchanged.
    let mut changed = application;
    let digest = expected.values().next().expect("the application has SQL");
    let offset = changed
        .windows(digest.len())
        .rposition(|bytes| bytes == digest.as_bytes())
        .expect("the outer record holds the member digest");
    let digit = &mut changed[offset + "sha256:".len()];
    *digit = if *digit == b'a' { b'b' } else { b'a' };
    assert!(wamn_engine::statement_instances::statement_instances(&changed).is_err());
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
            bytes_sha256(&wat::parse_str(&base).expect("encode base")),
        ),
        (
            "sql-1".to_owned(),
            bytes_sha256(&wat::parse_str(&overlay).expect("encode overlay")),
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
            assert!(result.is_err());
        }
    }
}
