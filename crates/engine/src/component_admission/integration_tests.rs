use super::{ComponentAdmissionErrorType, validate_integration_admission};
use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
use wit_parser::Resolve;

#[test]
#[ignore = "requires: runtime"]
fn integration_byte_admission_refuses_postgres_blobstore_and_other_effects() -> anyhow::Result<()> {
    let engine = crate::build_engine(&[])?;
    for interface in [
        "wamn:postgres/statements@0.4.0",
        "wamn:postgres/client@0.4.0",
        "wasmcloud:blobstore/blobstore@0.1.0",
        "wasi:http/outgoing-handler@0.2.12",
        "wamn:integration/health@0.1.0",
        "wasi:filesystem/types@0.2.12",
        "wasi:sockets/tcp@0.3.0",
    ] {
        let bytes = wat::parse_str(format!(
            r#"(component (import "{interface}" (instance (export "effect" (func)))))"#
        ))?;
        let error = validate_integration_admission(&engine, &bytes, "forbidden")
            .expect_err("integration effect must refuse");
        assert_eq!(
            error.error_type(),
            ComponentAdmissionErrorType::ImportPolicyRefused,
            "{interface}"
        );
    }
    Ok(())
}

#[test]
#[ignore = "requires: runtime"]
fn integration_byte_admission_refuses_missing_health_and_wrong_signature() -> anyhow::Result<()> {
    let engine = crate::build_engine(&[])?;
    let empty = wat::parse_str("(component)")?;
    let mut resolve = Resolve::new();
    let package = resolve.push_str(
        "wrong.wit",
        r"
        package wamn:integration@0.1.0;
        interface health { probe: func(request: string) -> u32; }
        world integration { export health; }
    ",
    )?;
    let world = resolve.select_world(&[package], Some("integration"))?;
    let mut module =
        wit_component::dummy_module(&resolve, world, wit_parser::ManglingAndAbi::Standard32);
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8)?;
    let wrong = ComponentEncoder::default()
        .module(&module)?
        .validate(true)
        .encode()?;
    for bytes in [empty, wrong] {
        let error = validate_integration_admission(&engine, &bytes, "wrong-health")
            .expect_err("the exact health contract is required");
        assert_eq!(
            error.error_type(),
            ComponentAdmissionErrorType::IntegrationExportMismatch
        );
    }
    Ok(())
}
