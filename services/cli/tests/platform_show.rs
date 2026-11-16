//! Platform show preserves the document through the public command interface.

use std::path::Path;

use wamn_cli::platform_verbs::{self, PlatformCommand, ShowArgs};
use wamn_platform_install::platform::document::ClusterDocument;

#[test]
#[ignore = "requires: bin:kcl"]
fn show_preserves_the_cluster_configuration() {
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/gcp/cluster.k");
    let document = ClusterDocument::compile(&committed).expect("compile the committed document");
    let mut output = Vec::new();
    platform_verbs::run(
        PlatformCommand::Show(ShowArgs { file: committed }),
        &mut output,
    )
    .expect("show the cluster document");
    let directory =
        wamn_test_infrastructure::scratch::ScratchRoot::create().expect("create the directory");
    let file = directory.path().join("cluster.k");
    std::fs::write(&file, &output).expect("write show's output");
    let compiled = ClusterDocument::compile(&file).expect("compile show's output");

    assert_eq!(compiled, document);
}
