//! Golden ZPL vectors for all three templates — the unit gate named by
//! `docs/poc/wms-prep-spec.md` §1b.
//!
//! These are frozen literals, matching the repository's established golden
//! shape. A change to any rendered byte must be a deliberate edit here, not a
//! silent drift: a label already printed and applied to a pallet does not
//! re-render.
//!
//! The geometry line (`^PW812 ^LL1218 ^MD0`) assumes 4in x 6in stock at
//! 203 dpi and is PROVISIONAL — see `LABEL_GEOMETRY` in the crate root. When
//! real stock is chosen these vectors move with it.

use label_template::{RenderErrorKind, TEMPLATE_IDS, render};
use serde_json::json;

const PALLET: &str = "\
^XA
^PW812
^LL1218
^MD0
^CI28
^FO40,40^A0N,36,36^FDPALLET^FS
^FO40,96^A0N,28,28^FDPallet: PAL-000042^FS
^FO40,136^A0N,28,28^FDLocation: LOC-0007^FS
^FO40,176^BY3^BCN,120,Y,N,N^FDPAL-000042^FS
^XZ
";

const LOCATION: &str = "\
^XA
^PW812
^LL1218
^MD0
^CI28
^FO40,40^A0N,36,36^FDLOCATION^FS
^FO40,96^A0N,28,28^FDLocation: LOC-0007^FS
^FO40,136^A0N,28,28^FDZone: A^FS
^FO40,176^BY3^BCN,120,Y,N,N^FDLOC-0007^FS
^XZ
";

const PRODUCT: &str = "\
^XA
^PW812
^LL1218
^MD0
^CI28
^FO40,40^A0N,36,36^FDPRODUCT^FS
^FO40,96^A0N,28,28^FDSKU: SKU-00123^FS
^FO40,136^A0N,28,28^FDDesc: Widget^FS
^FO40,176^BY3^BCN,120,Y,N,N^FDSKU-00123^FS
^XZ
";

#[test]
fn pallet_matches_its_golden_vector() {
    let zpl = render(
        "pallet",
        &json!({"pallet_id": "PAL-000042", "location_id": "LOC-0007"}),
    )
    .expect("the pallet template renders");
    assert_eq!(zpl, PALLET);
}

#[test]
fn location_matches_its_golden_vector() {
    let zpl = render("location", &json!({"location_id": "LOC-0007", "zone": "A"}))
        .expect("the location template renders");
    assert_eq!(zpl, LOCATION);
}

#[test]
fn product_matches_its_golden_vector() {
    let zpl = render(
        "product",
        &json!({"product_id": "SKU-00123", "description": "Widget"}),
    )
    .expect("the product template renders");
    assert_eq!(zpl, PRODUCT);
}

/// The gate covers the whole set, so adding a template without a vector fails
/// here rather than shipping unproven.
#[test]
fn every_template_id_has_a_golden_vector() {
    assert_eq!(TEMPLATE_IDS.len(), 3, "the template set changed size");
    for id in TEMPLATE_IDS {
        assert!(
            matches!(id, "pallet" | "location" | "product"),
            "template {id:?} has no golden vector in this file"
        );
    }
}

#[test]
fn rendering_is_a_pure_function_of_its_input() {
    let fields = json!({"pallet_id": "PAL-000042", "location_id": "LOC-0007"});
    assert_eq!(
        render("pallet", &fields).expect("first render"),
        render("pallet", &fields).expect("second render"),
        "two renders of one input disagreed"
    );
}

/// Extra fields are ignored, so a caller may pass a wider record than the
/// template consumes.
#[test]
fn unknown_extra_fields_are_ignored() {
    let zpl = render(
        "pallet",
        &json!({"pallet_id": "PAL-000042", "location_id": "LOC-0007", "weight_kg": 480}),
    )
    .expect("extra fields do not refuse");
    assert_eq!(zpl, PALLET);
}

#[test]
fn an_unknown_template_is_refused() {
    let error = render("shipping_manifest", &json!({})).expect_err("must refuse");
    assert_eq!(error.kind(), RenderErrorKind::UnknownTemplate);
    assert_eq!(error.code(), "unknown_template");
}

#[test]
fn a_missing_required_field_is_refused() {
    let error = render("pallet", &json!({"pallet_id": "PAL-000042"})).expect_err("must refuse");
    assert_eq!(error.kind(), RenderErrorKind::MissingField);
    assert!(error.detail().contains("location_id"), "{}", error.detail());
}

/// `^` and `~` are ZPL command prefixes. A value carrying either could close
/// the field and inject commands, so the renderer refuses rather than escapes.
#[test]
fn a_zpl_control_character_is_refused() {
    for injected in ["PAL^XZ", "PAL~JA"] {
        let error = render(
            "pallet",
            &json!({"pallet_id": injected, "location_id": "LOC-0007"}),
        )
        .expect_err("must refuse");
        assert_eq!(error.kind(), RenderErrorKind::InvalidField, "{injected}");
    }
}

#[test]
fn a_non_string_or_empty_field_is_refused() {
    for bad in [json!(42), json!(""), json!(null)] {
        let error = render(
            "pallet",
            &json!({"pallet_id": bad, "location_id": "LOC-0007"}),
        )
        .expect_err("must refuse");
        assert_eq!(error.kind(), RenderErrorKind::InvalidField, "{bad}");
    }
}
