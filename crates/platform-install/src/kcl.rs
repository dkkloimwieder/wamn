//! Cluster document compilation.
use std::path::Path;
pub(crate) fn compile(file: &Path, name: &str) -> anyhow::Result<Vec<u8>> {
    wamn_codegen::kcl::compile_file(
        file,
        name,
        include_str!("../kcl/cluster/kcl.mod"),
        include_str!("../kcl/cluster/cluster.k"),
    )
}
