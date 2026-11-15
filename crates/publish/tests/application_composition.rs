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
