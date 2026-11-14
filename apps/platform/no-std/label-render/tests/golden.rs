//! Renderer refusals for invalid templates and field values.

#[path = "../src/template.rs"]
mod template;

use serde_json::json;
use template::{RenderErrorType, render};

#[test]
fn an_unknown_template_is_refused() {
    let error = render("shipping_manifest", &json!({})).expect_err("must refuse");
    assert_eq!(error.error_type(), RenderErrorType::UnknownTemplate);
}

#[test]
fn a_missing_required_field_is_refused() {
    let error =
        render("packaging", &json!({"packaging_id": "PAL-000042"})).expect_err("must refuse");
    assert_eq!(error.error_type(), RenderErrorType::MissingField);
}

/// `^` and `~` are ZPL command prefixes. A value carrying either could close
/// the field and inject commands, so the renderer refuses rather than escapes.
#[test]
fn a_zpl_control_character_is_refused() {
    for injected in ["PAL^XZ", "PAL~JA"] {
        let error = render(
            "packaging",
            &json!({"packaging_id": injected, "location_id": "LOC-0007"}),
        )
        .expect_err("must refuse");
        assert_eq!(
            error.error_type(),
            RenderErrorType::InvalidField,
            "{injected}"
        );
    }
}

#[test]
fn a_non_string_or_empty_field_is_refused() {
    for bad in [json!(42), json!(""), json!(null)] {
        let error = render(
            "packaging",
            &json!({"packaging_id": bad, "location_id": "LOC-0007"}),
        )
        .expect_err("must refuse");
        assert_eq!(error.error_type(), RenderErrorType::InvalidField, "{bad}");
    }
}
