const DEPENDENCIES: [&str; 3] = [
    "serde_json = { workspace = true }",
    "wamn-node-guest = { workspace = true }",
    "wamn-node-sdk = { workspace = true }",
];

pub fn validate_manifest(source: &str, node_type: &str) -> Result<(), String> {
    for required in [
        format!("name = \"{node_type}\""),
        String::from("crate-type = [\"cdylib\"]"),
        format!("node-type = \"{node_type}\""),
        String::from("output-ports = [\"main\"]"),
        String::from("purity = \"pure\""),
    ] {
        if !source.lines().any(|line| line.trim() == required) {
            return Err(format!("missing exact declaration {required:?}"));
        }
    }

    let dependency_lines = section_lines(source, "[dependencies]");
    if dependency_lines != DEPENDENCIES {
        return Err(format!(
            "zero-import dependency boundary drifted: expected {DEPENDENCIES:?}, got {dependency_lines:?}"
        ));
    }
    Ok(())
}

fn section_lines<'a>(source: &'a str, section: &str) -> Vec<&'a str> {
    let mut inside = false;
    let mut lines = Vec::new();
    for line in source.lines().map(str::trim) {
        if line == section {
            inside = true;
            continue;
        }
        if inside && line.starts_with('[') {
            break;
        }
        if inside && !line.is_empty() && !line.starts_with('#') {
            lines.push(line);
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::validate_manifest;

    const MANIFEST: &str = include_str!("../Cargo.toml");

    #[test]
    fn manifest_without_explicit_purity_is_rejected() {
        let mutant = MANIFEST.replace("purity = \"pure\"\n", "");
        assert!(validate_manifest(&mutant, "normalize-receipt").is_err());
    }

    #[test]
    fn undeclared_import_dependency_is_rejected() {
        let mutant = MANIFEST.replace("[dependencies]\n", "[dependencies]\nwasi-http = \"0.1\"\n");
        assert!(validate_manifest(&mutant, "normalize-receipt").is_err());
    }

    #[test]
    fn undeclared_output_port_is_rejected() {
        let mutant = MANIFEST.replace(
            "output-ports = [\"main\"]",
            "output-ports = [\"main\", \"other\"]",
        );
        assert!(validate_manifest(&mutant, "normalize-receipt").is_err());
    }
}
