//! Platform document schemas owned by environment control.
use std::path::Path;
/// Compile a platform document with its embedded schema.
pub(crate) fn compile(file: &Path, name: &str) -> anyhow::Result<Vec<u8>> {
    let (declaration, source) = match name {
        "environment" => (
            include_str!("../kcl/environment/kcl.mod"),
            include_str!("../kcl/environment/environment.k"),
        ),
        "cluster" => (
            include_str!("../kcl/cluster/kcl.mod"),
            include_str!("../kcl/cluster/cluster.k"),
        ),
        "project" => (
            include_str!("../kcl/project/kcl.mod"),
            include_str!("../kcl/project/project.k"),
        ),
        _ => anyhow::bail!("unknown platform schema {name}"),
    };
    wamn_compiler::kcl::compile_file(file, name, declaration, source)
}
