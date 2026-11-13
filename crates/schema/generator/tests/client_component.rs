use wamn_compiler::client_ir::FieldIr;

#[path = "support/platform_fixture.rs"]
mod fixture;
#[path = "support/client_component_compile.rs"]
mod typescript;

#[test]
fn every_ir_field_has_an_authored_label_or_its_exact_path() {
    fn visit(fields: &[FieldIr]) {
        for field in fields {
            assert!(field.label.is_some(), "{} has no label", field.path);
            visit(&field.children);
        }
    }
    let ir = fixture::client_release();
    for model in &ir.models {
        visit(&model.fields);
        for op in &model.operations {
            assert!(op.label.is_some());
            visit(&op.input_fields);
            visit(&op.result_fields);
            if let Some(route) = &op.route {
                visit(&route.response.fields);
            }
        }
    }
}

#[test]
fn emitted_descriptions_compile_against_wfe() {
    typescript::compile_descriptions(&fixture::client_release())
        .expect("the emitted descriptions satisfy the pinned wfe types");
}
