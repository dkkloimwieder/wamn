//! Pure ZPL rendering for the closed label template set.
//!
//! `{ template_id, fields } -> zpl`, with no effects and no ambient authority.
//! The rendering lives here rather than in the `label-render` guest because a
//! `no_std` cdylib guest has **no test target** — its lib-test unit links
//! `test`, which pulls `std` and collides with the guest-supplied
//! `#[panic_handler]` (E0152, duplicate lang item `panic_impl`), which is why
//! both existing guests set `test = false` and `bench = false` (wamn-6i30).
//! This crate supplies no panic handler, so it can carry the golden vectors
//! that `docs/poc/wms-prep-spec.md` §1b names as the unit gate.
//!
//! The template set is **closed**: three templates, no authoring machinery.
//! Template authoring is demand-gated — an application that needs a fourth
//! brings it.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString as _};
use alloc::vec::Vec;
use core::fmt::Write as _;

use serde_json::Value;

/// Every template this crate renders, in wire form.
pub const TEMPLATE_IDS: [&str; 3] = ["pallet", "location", "product"];

/// Print width, label length and media darkness, emitted by every template.
///
/// **Assumed stock: 4in x 6in at 203 dpi** — 812 dots wide, 1218 dots long,
/// the warehouse-standard thermal label. `^MD0` states the printer's
/// configured darkness explicitly rather than leaving it implicit, so a label
/// re-tuned for different stock is a visible diff.
///
/// These values are PROVISIONAL. No printer or label stock has been chosen for
/// the portfolio yet; that is a WMS application fixture decision. Geometry is
/// stated anyway because ZPL without it is not printable, and a golden vector
/// that cannot drive a printer proves nothing. When real stock is chosen, this
/// constant and the golden vectors move together.
const LABEL_GEOMETRY: &str = "^PW812\n^LL1218\n^MD0\n";

/// Why a render was refused.
///
/// This is the implementation-side error: a kind plus the context needed to
/// act on it. The `label-render` guest translates it to a `wamn:node`
/// `node-error` exactly once, at that WIT boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderError {
    kind: RenderErrorKind,
    detail: String,
}

impl RenderError {
    /// Stable refusal class for callers that must not match display text.
    #[must_use]
    pub const fn kind(&self) -> RenderErrorKind {
        self.kind
    }

    /// Human-readable context naming the template or field at fault.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// The wire code the guest reports for this refusal.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.kind.code()
    }
}

impl core::fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}: {}", self.kind.code(), self.detail)
    }
}

/// Stable classification for a refused render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderErrorKind {
    /// `template_id` is not one of [`TEMPLATE_IDS`].
    UnknownTemplate,
    /// The template requires a field the input did not supply.
    MissingField,
    /// A supplied field is not a string, is empty, or carries a ZPL control
    /// character.
    InvalidField,
}

impl RenderErrorKind {
    /// The frozen wire code for this class.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownTemplate => "unknown_template",
            Self::MissingField => "missing_field",
            Self::InvalidField => "invalid_field",
        }
    }
}

/// Render one label.
///
/// `fields` must be a JSON object supplying every field the template names.
/// Unknown extra fields are ignored, so a caller may pass a wider record than
/// the template consumes.
///
/// # Errors
///
/// Returns [`RenderErrorKind::UnknownTemplate`] for a template outside
/// [`TEMPLATE_IDS`], [`RenderErrorKind::MissingField`] when a required field is
/// absent, and [`RenderErrorKind::InvalidField`] when a field is not a
/// non-empty string free of ZPL control characters.
pub fn render(template_id: &str, fields: &Value) -> Result<String, RenderError> {
    let spec = match template_id {
        "pallet" => TemplateSpec {
            title: "PALLET",
            barcode: "pallet_id",
            lines: &["pallet_id", "location_id"],
        },
        "location" => TemplateSpec {
            title: "LOCATION",
            barcode: "location_id",
            lines: &["location_id", "zone"],
        },
        "product" => TemplateSpec {
            title: "PRODUCT",
            barcode: "product_id",
            lines: &["product_id", "description"],
        },
        other => {
            return Err(RenderError {
                kind: RenderErrorKind::UnknownTemplate,
                detail: format!("{other:?} is not one of {TEMPLATE_IDS:?}"),
            });
        }
    };
    spec.render(fields)
}

struct TemplateSpec {
    title: &'static str,
    barcode: &'static str,
    lines: &'static [&'static str],
}

impl TemplateSpec {
    fn render(&self, fields: &Value) -> Result<String, RenderError> {
        let barcode = field(fields, self.barcode)?;
        let lines = self
            .lines
            .iter()
            .map(|name| Ok((*name, field(fields, name)?)))
            .collect::<Result<Vec<_>, RenderError>>()?;

        // Writing to a String is infallible; the `expect` discharges the
        // `fmt::Write` contract without inventing an error path.
        let mut zpl = String::new();
        zpl.push_str("^XA\n");
        zpl.push_str(LABEL_GEOMETRY);
        zpl.push_str("^CI28\n");
        let title = self.title;
        writeln!(zpl, "^FO40,40^A0N,36,36^FD{title}^FS").expect("writing to a String cannot fail");
        let mut offset = 96;
        for (name, value) in lines {
            let label = label_of(name);
            writeln!(zpl, "^FO40,{offset}^A0N,28,28^FD{label}: {value}^FS")
                .expect("writing to a String cannot fail");
            offset += 40;
        }
        writeln!(zpl, "^FO40,{offset}^BY3^BCN,120,Y,N,N^FD{barcode}^FS")
            .expect("writing to a String cannot fail");
        zpl.push_str("^XZ\n");
        Ok(zpl)
    }
}

/// The human label printed beside a field's value.
fn label_of(name: &str) -> &'static str {
    match name {
        "pallet_id" => "Pallet",
        "location_id" => "Location",
        "product_id" => "SKU",
        "description" => "Desc",
        "zone" => "Zone",
        _ => "Field",
    }
}

/// One required field, validated.
///
/// `^` and `~` are ZPL's command prefixes, so a value carrying either could
/// close the field and inject commands. Refuse rather than escape: the closed
/// template set has no legitimate use for them, and refusing keeps the
/// rendered bytes a pure function of validated input.
fn field(fields: &Value, name: &str) -> Result<String, RenderError> {
    let Some(value) = fields.get(name) else {
        return Err(RenderError {
            kind: RenderErrorKind::MissingField,
            detail: format!("field {name:?} is required"),
        });
    };
    let Some(text) = value.as_str() else {
        return Err(RenderError {
            kind: RenderErrorKind::InvalidField,
            detail: format!("field {name:?} must be a string"),
        });
    };
    if text.is_empty() {
        return Err(RenderError {
            kind: RenderErrorKind::InvalidField,
            detail: format!("field {name:?} must not be empty"),
        });
    }
    if text.contains(['^', '~']) {
        return Err(RenderError {
            kind: RenderErrorKind::InvalidField,
            detail: format!("field {name:?} must not contain a ZPL control character"),
        });
    }
    Ok(text.to_string())
}
