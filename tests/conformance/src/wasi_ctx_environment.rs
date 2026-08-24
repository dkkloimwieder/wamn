//! Conformance proof that no guest is handed the host process environment.
//!
//! `deploy/platform/executor.yaml` and `deploy/platform/values-host-default.yaml`
//! inject a credentialed `WAMN_PG_URL` into the host process environment from a
//! `secretKeyRef`. Whether a guest that held `wasi:cli/environment` would observe
//! it is decided by how the pinned runtime builds the guest `WasiCtx`, and that
//! construction lives in the fork, not in-tree.
//!
//! Measured at the pin: it would observe nothing. The fork builds a guest
//! environment in exactly two places — a fallback carrying no environment at all,
//! and a workload path populated solely from `localResources.environment` — and
//! never calls `inherit_env`, the one `wasmtime-wasi` call that copies
//! `std::env::vars()` into a guest. The executor takes the fallback.
//!
//! That makes the `wasi:cli` import denial defence in depth rather than the sole
//! control, which is only true for as long as it stays true: this guard pins both
//! constructions as literals so a fork bump that adds inheritance fails loudly
//! instead of silently widening the blast radius behind the denial.
//!
//! The fork is located through [`super::ip_name_lookup::runtime_package`], the
//! one mechanism this crate uses to resolve the pinned checkout.

use std::fs;
use std::path::{Path, PathBuf};

use super::ip_name_lookup::{
    EXPECTED_REVISION, EXPECTED_VERSION, RuntimePackage, compact, repository_root, require,
    runtime_package,
};

/// The only `wasmtime-wasi` call that copies the host process environment into a
/// guest. Absent from the whole pinned runtime, which is what this guard holds.
const INHERIT_ENV: &str = "inherit_env";

/// Guest environment construction. Counted across the runtime crate so a bump that
/// moves the seam into a new file cannot slip past the two pinned literals below.
const WASI_CTX_CONSTRUCTOR: &str = "WasiCtxBuilder::new()";

const CTX_FILE: &str = "src/engine/ctx.rs";
const LINKED_CALL_FILE: &str = "src/engine/linked_call.rs";

/// The fallback `WasiCtx`, taken whenever a caller supplies none — args and stderr
/// only, no environment of any kind. Pinned through the `unwrap_or_else` that makes
/// it a fallback, so a bump cannot turn it into an unconditional context.
const CTX_FALLBACK_EMPTY_ENVIRONMENT: &str = "ctx:self.ctx.unwrap_or_else(||{WasiCtxBuilder::new().args(&[\"main.wasm\"]).inherit_stderr().build()}),";

/// The workload path's environment, populated solely from the template's declared
/// `localResources.environment`. Pinned whole: any extra source spliced into that
/// chain — a second `.envs(...)`, an inherit — breaks this marker.
const LINKED_CALL_EXPLICIT_ENVIRONMENT: &str = "letmutwasi_ctx_builder=WasiCtxBuilder::new();wasi_ctx_builder.envs(template.local_resources.environment.iter().map(|kv|(kv.0.as_str(),kv.1.as_str())).collect::<Vec<_>>().as_slice(),).inherit_stdout().inherit_stderr();";

/// The one place the workload builder's context is installed, and therefore the one
/// way a guest gets anything other than the empty fallback.
const WASI_CTX_INSTALL: &str = "with_wasi_ctx";

const LINKED_CALL_INSTALL: &str = ".with_wasi_ctx(wasi_ctx_builder.build())";

const ROUTER_DRIVER_FILE: &str = "crates/execution/host/src/router_driver.rs";

/// The executor's guest context: plugins only, no `WasiCtx`, so it resolves to the
/// fork's empty fallback.
const ROUTER_DRIVER_CTX: &str =
    "letctx=Ctx::builder(scope.to_string(),scope.to_string()).with_plugins(plugins).build();";

/// Same boundary [`super::runtime_inventory`] slices this file at. The claim is
/// about the production path, so a test module that legitimately builds an
/// environment-carrying context must not read as a production leak.
const CFG_TEST_MODULE: &str = "#[cfg(test)]\nmod tests {";

/// Every `.rs` file under the pinned runtime's `src/`, keyed by its path relative to
/// the package root and sorted, so the counts below cover the crate rather than only
/// the two files that build a `WasiCtx` today.
struct RuntimeSources {
    files: Vec<(String, String)>,
}

fn collect_rust_sources(directory: &Path, prefix: &str, files: &mut Vec<(String, String)>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read dir {}: {error}", directory.display()));
    for entry in entries {
        let entry = entry.expect("read a pinned runtime source entry");
        let path = entry.path();
        let relative = format!("{prefix}/{}", entry.file_name().to_string_lossy());
        if path.is_dir() {
            collect_rust_sources(&path, &relative, files);
        } else if relative.ends_with(".rs") {
            let body = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            files.push((relative, body));
        }
    }
}

fn runtime_sources(root: &Path) -> RuntimeSources {
    let mut files = Vec::new();
    collect_rust_sources(&root.join("src"), "src", &mut files);
    files.sort();
    RuntimeSources { files }
}

fn source<'a>(sources: &'a RuntimeSources, path: &str) -> Result<&'a str, String> {
    sources
        .files
        .iter()
        .find(|(candidate, _)| candidate == path)
        .map(|(_, body)| body.as_str())
        .ok_or_else(|| format!("fork WasiCtx seam: {path} is gone from the pinned runtime"))
}

fn validate_revision(package: &RuntimePackage, revision: &str) -> Result<(), String> {
    if package.source.contains(&format!("rev={revision}"))
        && package.source.ends_with(&format!("#{revision}"))
    {
        Ok(())
    } else {
        Err(format!(
            "wash-runtime must resolve to fork revision {revision}, got {}",
            package.source
        ))
    }
}

fn validate_wasi_ctx_environment(sources: &RuntimeSources) -> Result<(), String> {
    let inheriting = sources
        .files
        .iter()
        .filter(|(_, body)| body.contains(INHERIT_ENV))
        .map(|(path, _)| path.as_str())
        .collect::<Vec<_>>();
    if !inheriting.is_empty() {
        return Err(format!(
            "fork WasiCtx seam: `{INHERIT_ENV}` must appear nowhere in the pinned runtime, or a \
             guest sees the host process environment; found in {}",
            inheriting.join(", ")
        ));
    }

    let constructing = sources
        .files
        .iter()
        .filter(|(_, body)| body.contains(WASI_CTX_CONSTRUCTOR))
        .map(|(path, _)| path.as_str())
        .collect::<Vec<_>>();
    let constructions = sources
        .files
        .iter()
        .map(|(_, body)| body.matches(WASI_CTX_CONSTRUCTOR).count())
        .sum::<usize>();
    if constructing != [CTX_FILE, LINKED_CALL_FILE] || constructions != 2 {
        return Err(format!(
            "fork WasiCtx seam: the pinned runtime must build a guest environment only in \
             {CTX_FILE} and {LINKED_CALL_FILE}; found {constructions} construction(s) in \
             {constructing:?}"
        ));
    }

    require(
        &compact(source(sources, CTX_FILE)?),
        CTX_FALLBACK_EMPTY_ENVIRONMENT,
        "fork WasiCtx fallback",
    )?;

    let linked_call = compact(source(sources, LINKED_CALL_FILE)?);
    require(
        &linked_call,
        LINKED_CALL_EXPLICIT_ENVIRONMENT,
        "fork WasiCtx workload environment",
    )?;
    require(
        &linked_call,
        LINKED_CALL_INSTALL,
        "fork WasiCtx workload install",
    )
}

fn production_half<'a>(source: &'a str, seam: &str) -> Result<&'a str, String> {
    match source.matches(CFG_TEST_MODULE).count() {
        0 => Ok(source),
        1 => Ok(source
            .split_once(CFG_TEST_MODULE)
            .expect("the counted cfg(test) module must split")
            .0),
        found => Err(format!(
            "{seam} must carry at most one terminal `{CFG_TEST_MODULE}` module; found {found}"
        )),
    }
}

fn validate_executor_takes_the_fallback(router_driver: &str) -> Result<(), String> {
    let production = compact(production_half(router_driver, "executor guest context")?);
    if production.contains(WASI_CTX_INSTALL) {
        return Err(format!(
            "executor guest context: {ROUTER_DRIVER_FILE} must never call `{WASI_CTX_INSTALL}`; \
             installing a context would take the executor off the fork's empty fallback"
        ));
    }
    require(&production, ROUTER_DRIVER_CTX, "executor guest context")
}

fn router_driver_path() -> PathBuf {
    repository_root().join(ROUTER_DRIVER_FILE)
}

fn router_driver_source() -> String {
    let path = router_driver_path();
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// Splices `fault` in place of `original` in one file of an in-memory copy of the
/// pinned runtime and returns the error the guard must raise. The checkout itself is
/// a shared read-only cache and is never written.
fn fork_fault_error(file: &str, original: &str, fault: &str) -> String {
    let package = runtime_package();
    let mut sources = runtime_sources(&package.root);
    let target = sources
        .files
        .iter_mut()
        .find(|(path, _)| path == file)
        .unwrap_or_else(|| panic!("{file} must remain in the pinned runtime"));
    assert_eq!(
        target.1.matches(original).count(),
        1,
        "fault injection target must remain unique in {file}"
    );
    target.1 = target.1.replacen(original, fault, 1);
    validate_wasi_ctx_environment(&sources).expect_err("an injected fault must fail the guard")
}

fn router_driver_fault_error(original: &str, fault: &str) -> String {
    let router_driver = router_driver_source();
    assert_eq!(
        router_driver.matches(original).count(),
        1,
        "fault injection target must remain unique in {ROUTER_DRIVER_FILE}"
    );
    validate_executor_takes_the_fallback(&router_driver.replacen(original, fault, 1))
        .expect_err("an injected fault must fail the executor fallback guard")
}

#[test]
fn pinned_runtime_never_inherits_the_host_environment_into_a_guest() {
    let package = runtime_package();
    assert_eq!(package.version, EXPECTED_VERSION);
    validate_revision(&package, EXPECTED_REVISION).unwrap_or_else(|error| panic!("{error}"));
    let sources = runtime_sources(&package.root);
    validate_wasi_ctx_environment(&sources).unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn executor_guests_take_the_empty_wasi_ctx_fallback() {
    validate_executor_takes_the_fallback(&router_driver_source())
        .unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn injected_environment_inheritance_fails_the_wasi_ctx_guard() {
    // The smallest real widening available: the fallback keeps its args and stderr
    // and gains the host process environment, which is the shape a careless fork
    // bump would take.
    let error = fork_fault_error(
        CTX_FILE,
        ".inherit_stderr()",
        ".inherit_stderr().inherit_env()",
    );
    assert!(
        error.contains("WasiCtx") && error.contains(CTX_FILE),
        "fault must fail at the WasiCtx seam and name the file, got: {error}"
    );
}

#[test]
fn a_hand_rolled_host_environment_copy_fails_the_wasi_ctx_guard() {
    // `inherit_env` is the obvious spelling of the leak, not the only one: the same
    // copy can be written out by hand. The pinned literal, not the name scan, is
    // what has to catch that — so this fault deliberately avoids the name.
    let error = fork_fault_error(
        CTX_FILE,
        ".args(&[\"main.wasm\"])",
        ".args(&[\"main.wasm\"]).envs(&std::env::vars().collect::<Vec<_>>())",
    );
    assert!(
        error.contains("fork WasiCtx fallback"),
        "fault must fail at the pinned fallback literal, got: {error}"
    );
}

#[test]
fn a_widened_workload_environment_fails_the_wasi_ctx_guard() {
    // The workload path is the one that already calls `.envs(...)`, so the leak
    // there is an extra source spliced into the same chain rather than a new call.
    let error = fork_fault_error(
        LINKED_CALL_FILE,
        ".inherit_stdout()",
        ".envs(&std::env::vars().collect::<Vec<_>>()).inherit_stdout()",
    );
    assert!(
        error.contains("fork WasiCtx workload environment"),
        "fault must fail at the pinned workload environment literal, got: {error}"
    );
}

#[test]
fn an_uninstalled_workload_context_fails_the_wasi_ctx_guard() {
    // The pinned environment chain only describes what a guest sees for as long as
    // the builder it belongs to is the one installed.
    let error = fork_fault_error(
        LINKED_CALL_FILE,
        ".with_wasi_ctx(wasi_ctx_builder.build())",
        ".with_wasi_ctx(other_ctx_builder.build())",
    );
    assert!(
        error.contains("fork WasiCtx workload install"),
        "fault must fail at the pinned workload install, got: {error}"
    );
}

#[test]
fn a_third_wasi_ctx_construction_site_fails_the_wasi_ctx_guard() {
    let package = runtime_package();
    let mut sources = runtime_sources(&package.root);
    // A bump that builds a guest environment in a file this guard does not pin
    // would otherwise never be read at all.
    sources.files.push((
        "src/engine/other.rs".to_string(),
        format!("fn build() {{ {WASI_CTX_CONSTRUCTOR}.build(); }}"),
    ));
    sources.files.sort();

    let error = validate_wasi_ctx_environment(&sources)
        .expect_err("a third construction site must fail the WasiCtx guard");
    assert!(
        error.contains("src/engine/other.rs"),
        "fault must fail at the construction inventory and name the new file, got: {error}"
    );
}

#[test]
fn installing_a_context_fails_the_executor_fallback_guard() {
    let error = router_driver_fault_error(
        ".with_plugins(plugins)",
        ".with_plugins(plugins)\n            .with_wasi_ctx(wasi_ctx)",
    );
    assert!(
        error.contains(WASI_CTX_INSTALL),
        "fault must fail at the context install, got: {error}"
    );
}

#[test]
fn a_reshaped_executor_guest_context_fails_the_executor_fallback_guard() {
    // The absence check above only means anything while the pinned construction is
    // still the one the executor runs.
    let error = router_driver_fault_error(
        "Ctx::builder(scope.to_string(), scope.to_string())",
        "Ctx::builder(scope.to_string(), other.to_string())",
    );
    assert!(
        error.contains("executor guest context"),
        "fault must fail at the pinned executor construction, got: {error}"
    );
}

#[test]
fn the_wasi_ctx_guard_cannot_pass_against_a_different_revision() {
    let package = runtime_package();
    validate_revision(&package, EXPECTED_REVISION).expect("the pinned revision must validate");
    let other = "0000000000000000000000000000000000000000";
    let error = validate_revision(&package, other)
        .expect_err("a revision other than the pin must never validate");
    assert!(
        error.contains(other),
        "the revision check must name the revision it demanded, got: {error}"
    );
}
