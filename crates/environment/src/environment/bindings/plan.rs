use super::{BindingError, ConnectionMode};

pub(super) fn refuse(code: &'static str, context: impl Into<String>) -> BindingError {
    BindingError {
        code,
        context: context.into(),
        source: None,
    }
}

pub(super) fn mode_satisfies(
    mode: ConnectionMode,
    interface: &str,
    write_interfaces: &[String],
) -> bool {
    mode != ConnectionMode::Read
        || !write_interfaces
            .iter()
            .any(|export| interface_compatible(interface, export))
}

pub(crate) fn exports_satisfy(exports: &[String], required: &[String]) -> bool {
    required.iter().all(|interface| {
        exports
            .iter()
            .any(|export| interface_compatible(interface, export))
    })
}

pub(crate) fn interface_compatible(import: &str, export: &str) -> bool {
    use wit_parser::{PackageName, ParsedUsePath, parse_use_path};
    let (Ok(ParsedUsePath::Package(import, name)), Ok(ParsedUsePath::Package(export, export_name))) =
        (parse_use_path(import), parse_use_path(export))
    else {
        return false;
    };
    if import.namespace != export.namespace || import.name != export.name || name != export_name {
        return false;
    }
    match (import.version, export.version) {
        (Some(import), Some(export)) => {
            export >= import
                && PackageName::version_compat_track(&import)
                    == PackageName::version_compat_track(&export)
        }
        (None, None) => true,
        _ => false,
    }
}

/// The project catalog identity keeps equal names in separate applications apart.
pub fn key(application: &str, alias: &str) -> String {
    format!("{application}/{alias}")
}
