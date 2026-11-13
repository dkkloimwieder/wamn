//! Every executable Cargo target has exactly one execution tier.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde_json::Value;

type Target = (String, String, String);

fn key(row: &Value) -> Target {
    (
        row["workspace"].as_str().unwrap().into(),
        row["package"].as_str().unwrap().into(),
        row["target"].as_str().unwrap().into(),
    )
}

fn assignments(policy: &Value, targets: &BTreeSet<Target>) -> Result<(), String> {
    let mut rows = BTreeMap::new();
    for row in policy["targets"].as_array().unwrap() {
        let target = key(row);
        let tier = row["tier"].as_str().unwrap();
        if !["unit", "contract", "live-postgres", "live-nats", "kind"].contains(&tier) {
            return Err(format!("unknown tier {tier}: {target:?}"));
        }
        if !targets.contains(&target) {
            return Err(format!("row has no executable target: {target:?}"));
        }
        if rows.insert(target.clone(), tier).is_some() {
            return Err(format!("duplicate target: {target:?}"));
        }
    }
    for target in targets {
        if !rows.contains_key(target) {
            return Err(format!("target has no row: {target:?}"));
        }
    }
    for suite in policy["suites"].as_array().unwrap() {
        for target in suite["target"].as_array().unwrap() {
            let target = (
                suite["workspace"].as_str().unwrap().into(),
                suite["package"].as_str().unwrap().into(),
                target.as_str().unwrap().into(),
            );
            if rows.get(&target).copied() != suite["tier"].as_str() {
                return Err(format!("suite {} disagrees with {target:?}", suite["id"]));
            }
        }
    }
    Ok(())
}

fn policy(root: &Path) -> Value {
    let output = Command::new("python3")
        .args([
            "-c",
            "import json,sys,tomllib; print(json.dumps(tomllib.load(open(sys.argv[1], 'rb'))))",
        ])
        .arg(root.join("tests/selection.toml"))
        .output()
        .expect("read the selection file");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn real_metadata_has_exactly_one_tier_per_executable_target() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut targets = BTreeSet::new();
    for (workspace, manifest) in [
        ("root", "Cargo.toml"),
        ("apps", "apps/Cargo.toml"),
        ("no-std", "apps/platform/no-std/Cargo.toml"),
    ] {
        let output = Command::new(env!("CARGO"))
            .current_dir(&root)
            .args([
                "metadata",
                "--locked",
                "--offline",
                "--no-deps",
                "--format-version",
                "1",
                "--manifest-path",
                manifest,
            ])
            .output()
            .expect("read offline Cargo metadata");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
        for package in metadata["packages"].as_array().unwrap() {
            if !metadata["workspace_members"]
                .as_array()
                .unwrap()
                .contains(&package["id"])
            {
                continue;
            }
            for target in package["targets"].as_array().unwrap() {
                if target["test"] == true {
                    targets.insert((
                        workspace.into(),
                        package["name"].as_str().unwrap().into(),
                        format!(
                            "{}:{}",
                            target["kind"][0].as_str().unwrap(),
                            target["name"].as_str().unwrap()
                        ),
                    ));
                }
            }
        }
    }
    assignments(&policy(&root), &targets).unwrap();
}

#[test]
fn missing_duplicate_stale_and_unknown_assignments_fail() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let policy = policy(&root);
    let targets = policy["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(key)
        .collect();
    assignments(&policy, &targets).unwrap();
    for defect in ["missing", "duplicate", "stale", "unknown", "suite"] {
        let mut broken = policy.clone();
        let rows = broken["targets"].as_array_mut().unwrap();
        match defect {
            "missing" => {
                rows.remove(0);
            }
            "duplicate" => rows.push(rows[0].clone()),
            "stale" => rows[0]["target"] = "test:absent".into(),
            "unknown" => rows[0]["tier"] = "absent".into(),
            "suite" => {
                broken["suites"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|suite| !suite["target"].as_array().unwrap().is_empty())
                    .unwrap()["tier"] = "unit".into();
            }
            _ => unreachable!(),
        }
        assert!(assignments(&broken, &targets).is_err(), "accepted {defect}");
    }
}

fn ignored_reason(function: &syn::ItemFn) -> Option<String> {
    function.attrs.iter().find_map(|attribute| {
        if !attribute.path().is_ident("ignore") {
            return None;
        }
        Some(match &attribute.meta {
            syn::Meta::NameValue(value) => match &value.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(reason),
                    ..
                }) => reason.value(),
                _ => String::new(),
            },
            _ => String::new(),
        })
    })
}

fn literal(expression: &syn::Expr) -> Option<String> {
    if let syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Str(value),
        ..
    }) = expression
    {
        Some(value.value())
    } else {
        None
    }
}

fn has_prerequisite(function: &syn::ItemFn, reason: &str) -> bool {
    match function
        .block
        .stmts
        .iter()
        .find(|statement| !matches!(statement, syn::Stmt::Item(_)))
    {
        Some(syn::Stmt::Expr(syn::Expr::Call(call), _)) => {
            matches!(&*call.func, syn::Expr::Path(path)
                if path.path.segments.last().unwrap().ident == "require_ignored_prerequisites")
                && call.args.len() == 1
                && literal(&call.args[0]).as_deref() == Some(reason)
        }
        // Guest tests read their sole environment input directly before file access.
        Some(syn::Stmt::Local(local)) => {
            let names = wamn_test_postgres::parse_prerequisites(reason).unwrap();
            if names.len() != 1 || names[0].0 != "env" {
                return false;
            }
            let Some(init) = &local.init else {
                return false;
            };
            let syn::Expr::MethodCall(expect) = &*init.expr else {
                return false;
            };
            let syn::Expr::MethodCall(filter) = &*expect.receiver else {
                return false;
            };
            let Some(syn::Expr::Closure(predicate)) = filter.args.first() else {
                return false;
            };
            let syn::Expr::Unary(syn::ExprUnary {
                op: syn::UnOp::Not(_),
                expr,
                ..
            }) = &*predicate.body
            else {
                return false;
            };
            let syn::Expr::Call(call) = &*filter.receiver else {
                return false;
            };
            expect.method == "expect"
                && filter.method == "filter"
                && matches!(&**expr, syn::Expr::MethodCall(method)
                    if method.method == "is_empty" && method.args.is_empty())
                && matches!(&*call.func, syn::Expr::Path(path)
                    if path.path.segments.last().unwrap().ident == "var_os")
                && call.args.len() == 1
                && literal(&call.args[0]).as_deref() == Some(names[0].1)
        }
        _ => false,
    }
}

#[derive(Default)]
struct ChildBody {
    input: bool,
    returns: bool,
    waits: bool,
}

impl<'ast> syn::visit::Visit<'ast> for ChildBody {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*call.func {
            let name = path.path.segments.last().unwrap().ident.to_string();
            self.input |= matches!(name.as_str(), "var" | "var_os");
            self.waits |= matches!(name.as_str(), "sleep" | "pending" | "park");
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_return(&mut self, expression: &'ast syn::ExprReturn) {
        self.returns = true;
        syn::visit::visit_expr_return(self, expression);
    }
}

fn ignore_errors(function: &syn::ItemFn) -> Vec<String> {
    use syn::visit::Visit as _;

    let Some(reason) = ignored_reason(function) else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    if let Err(error) = wamn_test_postgres::parse_prerequisites(&reason) {
        errors.push(error);
    } else if !has_prerequisite(function, &reason) {
        errors.push("first executable statement does not require the ignore reason".into());
    }
    let mut body = ChildBody::default();
    body.visit_block(&function.block);
    if body.input && body.returns && body.waits {
        errors.push("ignored test body is a child helper".into());
    }
    errors
}

struct IgnoredTests<'a> {
    workspace: &'a str,
    package: &'a str,
    file: &'a Path,
    modules: Vec<String>,
    exclusions: &'a mut BTreeSet<Target>,
    errors: &'a mut Vec<String>,
}

impl<'ast> syn::visit::Visit<'ast> for IgnoredTests<'_> {
    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        self.modules.push(module.ident.to_string());
        syn::visit::visit_item_mod(self, module);
        self.modules.pop();
    }

    fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
        if ignored_reason(function).is_none() {
            return;
        }
        let mut identity = self.modules.clone();
        identity.push(function.sig.ident.to_string());
        let identity = identity.join("::");
        if self
            .exclusions
            .remove(&(self.workspace.into(), self.package.into(), identity.clone()))
        {
            return;
        }
        for error in ignore_errors(function) {
            self.errors
                .push(format!("{}: {identity}: {error}", self.file.display()));
        }
    }
}

fn rust_files(directory: &Path, files: &mut BTreeSet<std::path::PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path.file_name().unwrap() != "target" {
                rust_files(&path, files);
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.insert(path);
        }
    }
}

#[test]
fn ignored_tests_name_prerequisites_and_never_act_as_child_helpers() {
    use syn::visit::Visit as _;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let policy = policy(&root);
    let mut exclusions = BTreeSet::new();
    for row in policy["exclusions"].as_array().unwrap() {
        assert_eq!(row.as_object().unwrap().len(), 4, "invalid exclusion {row}");
        assert_eq!(row["bead"], "wamn-kov6v.6", "invalid exclusion {row}");
        assert!(
            exclusions.insert((
                row["workspace"].as_str().unwrap().into(),
                row["package"].as_str().unwrap().into(),
                row["test"].as_str().unwrap().into(),
            )),
            "duplicate exclusion {row}"
        );
    }
    assert_eq!(exclusions.len(), 0, "no test is excluded");
    let mut errors = Vec::new();
    for (workspace, manifest) in [
        ("root", "Cargo.toml"),
        ("apps", "apps/Cargo.toml"),
        ("no-std", "apps/platform/no-std/Cargo.toml"),
    ] {
        let output = Command::new(env!("CARGO"))
            .current_dir(&root)
            .args([
                "metadata",
                "--locked",
                "--offline",
                "--no-deps",
                "--format-version",
                "1",
                "--manifest-path",
                manifest,
            ])
            .output()
            .expect("read offline Cargo metadata");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
        for package in metadata["packages"].as_array().unwrap() {
            if !metadata["workspace_members"]
                .as_array()
                .unwrap()
                .contains(&package["id"])
            {
                continue;
            }
            let directory = Path::new(package["manifest_path"].as_str().unwrap())
                .parent()
                .unwrap();
            let mut files = BTreeSet::new();
            rust_files(directory, &mut files);
            for target in package["targets"].as_array().unwrap() {
                let source = Path::new(target["src_path"].as_str().unwrap())
                    .canonicalize()
                    .unwrap();
                if !source.starts_with(directory) {
                    rust_files(source.parent().unwrap(), &mut files);
                }
            }
            for file in files {
                let source = std::fs::read_to_string(&file).unwrap();
                let syntax = syn::parse_file(&source)
                    .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
                let mut modules = Vec::new();
                if let Ok(source_path) = file
                    .strip_prefix(directory)
                    .and_then(|relative| relative.strip_prefix("src"))
                {
                    modules = source_path
                        .with_extension("")
                        .iter()
                        .map(|part| part.to_str().unwrap().to_owned())
                        .collect();
                    if matches!(
                        modules.last().map(String::as_str),
                        Some("lib" | "main" | "mod")
                    ) {
                        modules.pop();
                    }
                }
                IgnoredTests {
                    workspace,
                    package: package["name"].as_str().unwrap(),
                    file: file.strip_prefix(&root).unwrap(),
                    modules,
                    exclusions: &mut exclusions,
                    errors: &mut errors,
                }
                .visit_file(&syntax);
            }
        }
    }
    for exclusion in exclusions {
        errors.push(format!(
            "exclusion names no existing ignored test: {exclusion:?}"
        ));
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn malformed_reasons_drift_and_child_helper_bodies_fail() {
    for source in [
        r"#[ignore] fn empty() {}",
        r#"#[ignore = "requires:"] fn empty_list() {}"#,
        r#"#[ignore = "requires: PostgreSQL 18"] fn prose() {}"#,
        r#"#[ignore = "requires: env:INPUT"] fn accepts_empty() { let path = std::env::var_os("INPUT").expect("INPUT"); }"#,
        r#"#[ignore = "requires: bin:sh"] fn drift() { wamn_test_postgres::require_ignored_prerequisites("requires: bin:helm"); }"#,
        r#"#[ignore = "requires: env:CHILD"] fn helper() {
            wamn_test_postgres::require_ignored_prerequisites("requires: env:CHILD");
            let Some(path) = std::env::var_os("CHILD") else { return; };
            std::thread::sleep(std::time::Duration::from_secs(600));
        }"#,
    ] {
        let function = syn::parse_str(source).unwrap();
        assert!(!ignore_errors(&function).is_empty(), "accepted {source}");
    }
    for source in [
        r#"#[ignore = "requires: bin:sh"] fn valid() { wamn_test_postgres::require_ignored_prerequisites("requires: bin:sh"); }"#,
        r#"#[ignore = "requires: bin:sh"] fn declarations() { const VALUE: u8 = 1; wamn_test_postgres::require_ignored_prerequisites("requires: bin:sh"); }"#,
        r#"#[ignore = "requires: env:INPUT"] fn direct() { let path = std::env::var_os("INPUT").filter(|value| !value.is_empty()).expect("INPUT"); }"#,
    ] {
        let function = syn::parse_str(source).unwrap();
        assert!(ignore_errors(&function).is_empty(), "refused {source}");
    }
}

fn preparation_errors(policy: &Value, names: &BTreeSet<String>) -> Vec<String> {
    let mut owners: BTreeMap<&str, usize> = BTreeMap::new();
    for step in policy["preparation"].as_array().unwrap() {
        for field in ["outputs", "aliases"] {
            if let Some(outputs) = step[field].as_object() {
                for name in outputs.keys() {
                    *owners.entry(name).or_default() += 1;
                }
            }
        }
    }
    for (name, description) in policy["operator_inputs"].as_object().unwrap() {
        assert!(!description.as_str().unwrap().is_empty());
        *owners.entry(name).or_default() += 1;
    }
    names
        .iter()
        .filter(|name| owners.get(name.as_str()) != Some(&1))
        .map(|name| format!("{name}: expected exactly one preparation or operator input"))
        .collect()
}

#[test]
fn every_ignored_environment_input_has_one_preparation_or_operator_owner() {
    use syn::visit::Visit as _;

    #[derive(Default)]
    struct Inputs(BTreeSet<String>);
    impl<'ast> syn::visit::Visit<'ast> for Inputs {
        fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
            if let Some(reason) = ignored_reason(function)
                && let Ok(names) = wamn_test_postgres::parse_prerequisites(&reason)
            {
                self.0.extend(
                    names
                        .into_iter()
                        .filter(|(kind, _)| *kind == "env")
                        .map(|(_, name)| name.to_owned()),
                );
            }
            syn::visit::visit_item_fn(self, function);
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = BTreeSet::new();
    for directory in ["apps", "crates", "services", "tests", "test-support"] {
        rust_files(&root.join(directory), &mut files);
    }
    let mut inputs = Inputs::default();
    for path in files {
        inputs.visit_file(&syn::parse_file(&std::fs::read_to_string(path).unwrap()).unwrap());
    }
    let policy = policy(&root);
    let errors = preparation_errors(&policy, &inputs.0);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let name = "WAMN_FLOW_HTTP_COMPONENT";
    let mut missing = policy.clone();
    let steps = missing["preparation"].as_array_mut().unwrap();
    let index = steps
        .iter()
        .position(|step| step["outputs"][name].is_string())
        .unwrap();
    steps[index]["outputs"]
        .as_object_mut()
        .unwrap()
        .remove(name);
    assert!(!preparation_errors(&missing, &inputs.0).is_empty());
    let mut duplicate = policy;
    duplicate["operator_inputs"][name] = "duplicate owner".into();
    assert!(!preparation_errors(&duplicate, &inputs.0).is_empty());
}
