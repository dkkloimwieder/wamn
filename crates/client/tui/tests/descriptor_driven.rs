//! The controls are driven by the descriptors the SHIPPED contracts declare.
//!
//! Every unit test in this crate hand-writes a descriptor to exercise one
//! behaviour. This one projects the Receiving package's real contracts and
//! renders the result, so "driven by IR descriptors rather than hand-written
//! field lists" is proven against the corpus rather than against a fixture
//! shaped to agree — and a field added to a contract shows up here without
//! anyone editing this file.
//!
//! The generator is a DEV-dependency only. A terminal must not link a code
//! generator to read four strings; the production graph of this crate is
//! `wamn-client` alone, and that is what puts the descriptor type there.

use std::path::{Path, PathBuf};

use wamn_client::FieldDescriptor;
use wamn_client_tui::form::{FieldEditor, Form};
use wamn_client_tui::{Table, render_to_buffer, row_text};
use wamn_schema_generator::client_ir::{ClientContractIr, FieldIr};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("the crate sits three levels below the repository root")
        .to_path_buf()
}

/// Turn a projected field into the runtime descriptor a control consumes.
///
/// Leaks, deliberately and only in tests: `FieldDescriptor` borrows because
/// every production value is a constant in generated code, and a test that
/// builds one from a runtime projection has to provide that lifetime somehow.
fn descriptor(field: &FieldIr) -> FieldDescriptor {
    let values: Vec<&'static str> = field
        .values
        .iter()
        .map(|value| &*Box::leak(value.clone().into_boxed_str()))
        .collect();
    FieldDescriptor {
        path: Box::leak(field.path.clone().into_boxed_str()),
        type_name: Box::leak(field.type_name.clone().into_boxed_str()),
        nullable: field.nullable,
        values: Box::leak(values.into_boxed_slice()),
    }
}

/// The `purchase_order` model's fields, as the shipped release declares them.
fn purchase_order_fields() -> Vec<FieldDescriptor> {
    let root = repository_root();
    let ir = ClientContractIr::from_release(
        "receiving",
        &root.join("packages/receiving/generated/contracts"),
        &root.join("packages/receiving/publication/attachments.json"),
    )
    .expect("the shipped Receiving release projects");
    ir.models
        .iter()
        .find(|model| model.name == "purchase_order")
        .expect("the shipped release has a purchase_order model")
        .fields
        .iter()
        .map(descriptor)
        .collect()
}

#[test]
fn a_table_renders_every_contract_field_as_a_column() {
    let fields = purchase_order_fields();
    assert!(fields.len() >= 6, "the projection is suspiciously small");

    let rows = vec![
        fields
            .iter()
            .map(|field| format!("<{}>", field.leaf()))
            .collect::<Vec<String>>(),
    ];
    let buffer = render_to_buffer(Table::new(&fields, &rows), 200, 2);
    let header = row_text(&buffer, 0);
    for field in &fields {
        assert!(
            header.contains(field.leaf()),
            "{} is missing from the header: {header}",
            field.leaf()
        );
    }
}

/// A path appears at most once. The model's field set is a union across its
/// operations, and before it merged by path a table rendered `id` twice.
#[test]
fn no_column_is_rendered_twice() {
    let fields = purchase_order_fields();
    let mut paths: Vec<&str> = fields.iter().map(|field| field.path).collect();
    let before = paths.len();
    paths.sort_unstable();
    paths.dedup();
    assert_eq!(before, paths.len(), "a duplicated column: {paths:?}");
}

/// Optionality reaches the screen from the contract, field by field — the
/// form marks exactly what the projection says is required.
#[test]
fn a_form_marks_exactly_the_contracts_required_fields() {
    let fields = purchase_order_fields();
    let height = u16::try_from(fields.len()).expect("small");
    let buffer = render_to_buffer(Form::new(&fields, &[]), 120, height);
    for (index, field) in fields.iter().enumerate() {
        let line = row_text(&buffer, u16::try_from(index).expect("small"));
        assert_eq!(
            line.contains('*'),
            !field.nullable,
            "{}: {line}",
            field.leaf()
        );
    }
}

/// The editor a field gets follows its declared domain, across the whole
/// model — closed becomes a choice, open becomes text.
#[test]
fn every_editor_follows_its_fields_declared_domain() {
    for field in &purchase_order_fields() {
        let expected = if field.values.is_empty() {
            FieldEditor::Text
        } else {
            FieldEditor::Choice
        };
        assert_eq!(FieldEditor::of(field), expected, "{}", field.path);
    }
}
