use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn source(path: &str) -> String {
    fs::read_to_string(root().join(path)).unwrap_or_else(|error| panic!("read {path}: {error}"))
}

fn compact(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn stored_instance_suffix_owns_every_cluster_global_project_env_name() {
    let names = compact(&source("crates/control/provision/src/name.rs"));
    assert!(names.contains(
        "pub fn project_env_database_name(org: &str, project: &str, env: &str, instance: &str)"
    ));
    assert!(
        names.contains(
            "pub fn cdc_object_name(org: &str, project: &str, env: &str, instance: &str)"
        )
    );

    let provision = compact(&source("crates/control/lib/src/provision_project_env.rs"));
    let record = provision
        .find("let instance = record_project_env(")
        .expect("ordinary provisioning reads or mints the stored suffix");
    let render = provision
        .find("let db_cr = render_project_env_database(&triple, &instance,")
        .expect("ordinary provisioning renders with the stored suffix");
    assert!(
        record < render,
        "registry read-or-mint must precede rendering"
    );
    assert!(
        provision
            .contains("let db_name = project_env_database_name(org, project, env, &instance);")
    );
    assert!(provision.contains(
        "render_project_env_database(&triple, &instance, &cluster, args.connection_limit)"
    ));
    let workload = compact(&source(
        "crates/control/lib/src/provision_project_env/workload.rs",
    ));
    assert!(workload.contains(
        "let instance = read_project_env_instance(system_url, &triple).await?; let database = project_env_database_name(org, project, environment, &instance);"
    ));
    let registry = compact(&source(
        "crates/control/lib/src/provision_project_env/registry.rs",
    ));
    let stored = registry
        .rfind("let stored: String = row.get(0);")
        .expect("recording consumes the returned stored suffix");
    let returned = registry[stored..]
        .find("Ok(stored)")
        .expect("recording returns the stored suffix");
    assert!(returned > 0);
    assert!(!registry[stored..].contains("Ok(minted.to_string())"));

    let copy = compact(&source("crates/control/lib/src/copy_project_env.rs"));
    assert!(copy.contains("read_project_env_instance(system_url, &src).await?"));
    assert!(copy.contains("read_project_env_instance(system_url, &dst).await?"));
    assert!(copy.contains(
        "let src_db = project_env_database_name(&src.org, &src.project, src.env.as_str(), &src_instance);"
    ));
    assert!(copy.contains(
        "let dst_db = project_env_database_name(&dst.org, &dst.project, dst.env.as_str(), &dst_instance);"
    ));

    let restore = compact(&source("crates/control/lib/src/restore_project_env.rs"));
    assert!(restore.contains(
        "let instance = read_project_env_instance(system_url, &triple).await?; restore_in_place(&request, &triple, &instance,"
    ));
    assert!(restore.contains(
        "let db_name = project_env_database_name(&request.org, &request.project, triple.env.as_str(), instance);"
    ));

    let cdc = compact(&source("crates/control/lib/src/enable_cdc_project_env.rs"));
    assert!(cdc.contains("read_project_env_instance(system_url, &triple).await?"));
    assert!(
        cdc.contains("project_env_database_name(&args.org, &args.project, &args.env, &instance)")
    );
    assert!(cdc.contains("cdc_object_name(&args.org, &args.project, &args.env, &instance)"));
}

#[test]
fn namespace_scoped_secret_names_remain_triple_only() {
    let names = compact(&source("crates/control/provision/src/name.rs"));
    assert!(
        names.contains(
            "pub fn project_env_secret_name(org: &str, project: &str, env: &str) -> String"
        )
    );
    assert!(names.contains(
        "pub fn project_env_cdc_secret_name(org: &str, project: &str, env: &str) -> String"
    ));
}
