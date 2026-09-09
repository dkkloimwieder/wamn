//! Developer-owned Rust composition over generated operator screens.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use clap::{Args, Subcommand};
use wamn_schema_generator::client_ir::ClientContractIr;
use wamn_schema_generator::client_rust::emit_rust_client;
use wamn_schema_generator::client_tui::emit_tui;
use wamn_schema_generator::{GeneratedFile, PackageManifest};

use crate::dev::watch::GitSource;

/// Operator interface commands.
#[derive(Debug, Args)]
pub struct UiArgs {
    #[command(subcommand)]
    pub command: UiCommand,
}

/// One explicit customization action.
#[derive(Debug, Subcommand)]
pub enum UiCommand {
    /// Copy generated screens into a developer-owned Rust crate.
    Scaffold(ScaffoldArgs),
}

/// Select a package directory and optionally one model.operation screen.
#[derive(Debug, Args)]
pub struct ScaffoldArgs {
    pub package: String,
    pub screen: Option<String>,
}

/// Create a scaffold from the current generated release projection.
pub async fn run(args: UiArgs) -> anyhow::Result<()> {
    let UiCommand::Scaffold(args) = args.command;
    let current = std::env::current_dir().context("read the current directory")?;
    let git = GitSource::discover(&current)
        .await
        .context("discover the originating Git worktree")?;
    let output = scaffold_package(git.repository_root(), &args)?;
    println!("scaffold created: {}", output.display());
    Ok(())
}

#[derive(Debug)]
struct ScreenFunction {
    model: String,
    module: String,
    name: String,
    function: String,
    operation: String,
    kind: String,
    spec: String,
    source: String,
}

fn selected_package(repository: &Path, selector: &str) -> anyhow::Result<PathBuf> {
    ensure!(
        selector
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase())
            && selector.bytes().all(|byte| byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-')),
        "package must name one directory under packages/"
    );
    let authored = repository.join("packages").join(selector);
    let root = authored
        .canonicalize()
        .with_context(|| format!("read package directory {}", authored.display()))?;
    ensure!(
        root == authored,
        "package directory must resolve to {}",
        authored.display()
    );
    ensure!(
        root.join("wamn.json").is_file(),
        "{} is not a declared package directory",
        root.display()
    );
    Ok(root)
}

fn scaffold_package(repository: &Path, args: &ScaffoldArgs) -> anyhow::Result<PathBuf> {
    let package = selected_package(repository, &args.package)?;
    let output = package.join("ui");
    ensure!(
        !output.exists(),
        "{} already exists; edit the existing Rust crate",
        output.display()
    );
    let manifest = PackageManifest::from_slice(&fs::read(package.join("wamn.json"))?)
        .context("read the package manifest")?;
    let ir = ClientContractIr::from_release(
        &manifest.package.id,
        &package.join("generated/contracts"),
        &package.join("publication/attachments.json"),
    )
    .context("read generated contracts; run Generate for this package before scaffolding")?;
    let emitted = emit_tui(&ir, &args.package).context("emit the current screen definitions")?;
    require_current(&package, &emitted)?;
    require_current(
        &package,
        &emit_rust_client(&ir).context("emit current client bindings")?,
    )?;
    let screens = screen_functions(&ir, &args.package, &emitted)?;
    ensure!(
        !screens.is_empty(),
        "this package has no operator screens to scaffold"
    );
    let selected = if let Some(selector) = &args.screen {
        let Some((model, name)) = selector.split_once('.') else {
            bail!("screen must spell model.operation");
        };
        ensure!(
            !model.is_empty() && !name.is_empty() && !name.contains('.'),
            "screen must spell model.operation"
        );
        ensure!(
            screens
                .iter()
                .any(|screen| screen.model == model && screen.name == name),
            "screen {selector:?} is not in the generated package"
        );
        Some((model, name))
    } else {
        None
    };
    let files = scaffold_files(&args.package, &ir.package, &screens, selected)?;
    fs::create_dir(&output).with_context(|| {
        format!(
            "create new scaffold {}; existing files are never overwritten",
            output.display()
        )
    })?;
    for (relative, source) in files {
        let path = output.join(relative);
        fs::create_dir_all(path.parent().expect("scaffold file has a parent"))
            .with_context(|| format!("create scaffold directory for {}", path.display()))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("create new scaffold file {}", path.display()))?;
        file.write_all(source.as_bytes())
            .with_context(|| format!("write scaffold file {}", path.display()))?;
    }
    Ok(output)
}

fn require_current(package: &Path, files: &[GeneratedFile]) -> anyhow::Result<()> {
    for file in files {
        let path = package.join(file.path());
        let actual = fs::read(&path).with_context(|| {
            format!(
                "read {}; run Generate for this package before scaffolding",
                path.display()
            )
        })?;
        ensure!(
            actual == file.bytes(),
            "{} is stale; run Generate for this package before scaffolding",
            path.display()
        );
    }
    Ok(())
}

fn screen_functions(
    ir: &ClientContractIr,
    directory: &str,
    files: &[GeneratedFile],
) -> anyhow::Result<Vec<ScreenFunction>> {
    let mut functions = Vec::new();
    for model in &ir.models {
        let path = format!("generated/{directory}-tui/src/screens/{}.rs", model.name);
        let source = files
            .iter()
            .find(|file| file.path() == path)
            .with_context(|| format!("screen emitter omitted {path}"))?;
        let source =
            std::str::from_utf8(source.bytes()).context("generated screens must be UTF-8")?;
        let module = generated_module(files, directory, &model.name)?;
        for operation in model
            .operations
            .iter()
            .filter(|operation| operation.kind != "event_handler")
        {
            let mut candidates =
                source
                    .match_indices("#[must_use]\npub fn ")
                    .filter_map(|(start, _)| {
                        let tail = &source[start + "#[must_use]\npub fn ".len()..];
                        let function = tail.split_once('(')?.0;
                        (function.strip_prefix("r#").unwrap_or(function) == operation.name)
                            .then_some((start, function))
                    });
            let (start, function) = candidates.next().with_context(|| {
                format!("screen emitter omitted {}.{}", model.name, operation.name)
            })?;
            ensure!(
                candidates.next().is_none(),
                "screen emitter duplicated {}.{}",
                model.name,
                operation.name
            );
            // The verified emitter produces one constructor body with no nested blocks.
            let end = source[start..]
                .find("\n}")
                .context("generated screen constructor has no closing brace")?
                + start
                + 2;
            functions.push(ScreenFunction {
                model: model.name.clone(),
                module: module.clone(),
                name: operation.name.clone(),
                function: function.to_owned(),
                operation: operation.operation.clone(),
                kind: operation.kind.clone(),
                spec: format!("{}_SPEC", operation.name.to_uppercase()),
                source: source[start..end].to_owned(),
            });
        }
    }
    functions.sort_by(|left, right| (&left.model, &left.name).cmp(&(&right.model, &right.name)));
    Ok(functions)
}

fn generated_module(
    files: &[GeneratedFile],
    directory: &str,
    model: &str,
) -> anyhow::Result<String> {
    let path = format!("generated/{directory}-tui/src/screens/mod.rs");
    let source = files
        .iter()
        .find(|file| file.path() == path)
        .context("screen emitter omitted its module list")?;
    let source = std::str::from_utf8(source.bytes()).context("generated modules must be UTF-8")?;
    source
        .lines()
        .filter_map(|line| {
            line.strip_prefix("pub mod ")
                .and_then(|line| line.strip_suffix(';'))
        })
        .find(|module| module.strip_prefix("r#").unwrap_or(module) == model)
        .map(str::to_owned)
        .with_context(|| format!("screen emitter omitted module {model}"))
}

fn scaffold_files(
    directory: &str,
    package: &str,
    screens: &[ScreenFunction],
    selected: Option<(&str, &str)>,
) -> anyhow::Result<BTreeMap<PathBuf, String>> {
    let slug = directory.replace('_', "-");
    let crate_name = format!("wamn_{}_ui", slug.replace('-', "_"));
    let mut files = BTreeMap::new();
    files.insert(PathBuf::from(".gitignore"), "/target/\n".to_owned());
    files.insert(PathBuf::from("Cargo.toml"), format!(
        "[package]\nname = \"wamn-{slug}-ui\"\nversion = \"0.1.0\"\nedition = \"2024\"\nlicense = \"Apache-2.0\"\n\n[workspace]\n\n[dependencies]\ngenerated = {{ package = \"wamn-generated-{slug}-tui\", path = \"../generated/{directory}-tui\" }}\nwamn-client-tui = {{ path = \"../../../crates/client/tui\" }}\nwamn-client-terminal = {{ path = \"../../../crates/client/terminal\" }}\ntokio = {{ version = \"1\", features = [\"macros\", \"rt-multi-thread\"] }}\n\n[dev-dependencies]\nwamn-client = {{ path = \"../../../crates/client/core\" }}\n"
    ));
    files.insert(PathBuf::from("src/main.rs"), format!(
        "#[tokio::main]\nasync fn main() -> Result<wamn_client_terminal::operator::ExitReason, Box<dyn std::error::Error>> {{\n    wamn_client_terminal::operator::run({package:?}, {crate_name}::screens).await\n}}\n"
    ));
    let mut library = String::from(
        "//! Developer-owned composition over regenerated screen contracts.\n\nuse wamn_client_tui::screen::Screen;\nuse wamn_client_tui::submission::SessionBinding;\n\npub mod screens;\n",
    );
    let modules = screens
        .iter()
        .map(|screen| screen.module.as_str())
        .collect::<BTreeSet<_>>();
    for module in modules {
        writeln!(library, "pub use ::generated::{module};").expect("write scaffold source");
    }
    library.push_str(
        "\n#[must_use]\npub fn screens(binding: SessionBinding) -> Vec<Screen> {\n    vec![\n",
    );
    let mut overrides = BTreeMap::<&str, Vec<&ScreenFunction>>::new();
    for (index, screen) in screens.iter().enumerate() {
        let copied =
            selected.is_none_or(|(model, name)| screen.model == model && screen.name == name);
        let owner = if copied {
            "screens"
        } else {
            "::generated::screens"
        };
        let binding = if index + 1 == screens.len() {
            "binding"
        } else {
            "binding.clone()"
        };
        writeln!(
            library,
            "        {owner}::{}::{}({binding}),",
            screen.module, screen.function
        )
        .expect("write scaffold source");
        if copied {
            overrides.entry(&screen.model).or_default().push(screen);
        }
    }
    library.push_str("    ]\n}\n");
    files.insert(PathBuf::from("src/lib.rs"), library);
    let mut modules = String::from(
        "//! Copied functions remain ordinary Rust and use the generated contracts.\n\n",
    );
    for (model, copied) in &overrides {
        let module = &copied[0].module;
        writeln!(modules, "pub mod {module};").expect("write scaffold modules");
        let specs = copied
            .iter()
            .map(|screen| screen.spec.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let mut source = format!(
            "//! Developer-owned screens for {model}.\n\nuse wamn_client_tui::{{screen, submission}};\nuse ::generated::screens::{module}::{{{specs}}};\n"
        );
        for screen in copied {
            source.push('\n');
            source.push_str(&screen.source);
            source.push('\n');
        }
        files.insert(PathBuf::from(format!("src/screens/{model}.rs")), source);
    }
    files.insert(PathBuf::from("src/screens/mod.rs"), modules);
    let chosen = overrides
        .values()
        .next()
        .and_then(|screens| screens.first())
        .context("scaffold selected no screen")?;
    files.insert(
        PathBuf::from("tests/scaffold_tracks_contract.rs"),
        interaction_tests(&crate_name, chosen),
    );
    files.insert(PathBuf::from("README.md"), format!(
        "This crate is developer-owned Rust over the generated {directory} screens.\nEdit the copied functions in `src/screens/` to add composition.\nKeep the direct calls in `src/lib.rs` for screens that you do not override.\nTo remove an override, call its function under `generated::screens` again.\n\nTyped API incompatibilities fail this crate's build.\nThis scaffold must pass its declared interaction tests against regenerated bindings.\nThe initial tests cover the selected operation kind and session reset.\nAdd assertions for your custom workflow.\nAn additive field that no assertion reads can pass.\n\nAfter Generate completes, run `cargo test --manifest-path packages/{directory}/ui/Cargo.toml`.\nSupply `WAMN_BASE_URL`, `WAMN_HOST`, `WAMN_TOKEN`, and `WAMN_TARGET_INSTANCE` from the active development session before launch.\n"
    ));
    Ok(files)
}

fn interaction_tests(crate_name: &str, selected: &ScreenFunction) -> String {
    let mut source = r#"use wamn_client::descriptor::FieldSchema;
use wamn_client::FieldDescriptor;
use wamn_client_tui::screen::{Availability, Screen};
use wamn_client_tui::submission::SessionBinding;

fn binding(instance: &str) -> SessionBinding {
    SessionBinding {
        url: "http://127.0.0.1:31001".to_owned(),
        host: Some("scaffold.localhost".to_owned()),
        target_instance: instance.to_owned(),
    }
}

fn selected_screen() -> Screen {
    __CRATE__::screens(binding("first"))
        .into_iter()
        .find(|screen| screen.spec().operation == __OPERATION__)
        .expect("the custom composition retains its selected operation")
}

fn assert_declared_interaction(mut screen: Screen) {
    assert_eq!(screen.spec().operation, __OPERATION__);
    assert_eq!(screen.spec().kind, __KIND__);
    assert!(screen.submission().available());
    assert!(!screen.activate(binding("first")));
    screen.invalidate();
    assert_eq!(screen.availability(), Availability::Unavailable);
    assert!(screen.activate(binding("second")));
    assert!(screen.rows().is_empty());
    assert!(screen.cursor().is_none());
    assert!(!screen.dirty());
}

#[test]
fn scaffold_tracks_contract() {
    assert_declared_interaction(selected_screen());
}

#[test]
fn a_changed_declared_kind_fails_the_interaction_assertion() {
    let mut spec = *selected_screen().spec();
    spec.kind = if spec.kind == "delete" { "query" } else { "delete" };
    spec.response.kind = spec.kind;
    let changed = Screen::new(Box::leak(Box::new(spec)), binding("first"));
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_declared_interaction(changed);
    })).is_err());
}

#[test]
fn an_unasserted_additive_result_field_is_allowed() {
    let mut spec = *selected_screen().spec();
    let mut fields = spec.response.fields.to_vec();
    fields.push(FieldSchema {
        field: FieldDescriptor {
            path: "scaffold_additive_example",
            type_name: "text",
            nullable: true,
            values: &[],
        },
        required: false,
        children: &[],
        minimum: None,
        maximum: None,
    });
    spec.response.fields = Box::leak(fields.into_boxed_slice());
    assert_declared_interaction(Screen::new(Box::leak(Box::new(spec)), binding("first")));
}
"#
    .to_owned();
    for (token, value) in [
        ("__CRATE__", crate_name.to_owned()),
        ("__OPERATION__", format!("{:?}", selected.operation)),
        ("__KIND__", format!("{:?}", selected.kind)),
    ] {
        source = source.replace(token, &value);
    }
    source
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "wamn-ui-scaffold-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(root.join("packages/receiving")).expect("create scaffold fixture");
            let original = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/receiving");
            let package = root.join("packages/receiving");
            fs::copy(original.join("wamn.json"), package.join("wamn.json"))
                .expect("copy package manifest");
            copy_tree(
                &original.join("generated/contracts"),
                &package.join("generated/contracts"),
            );
            copy_tree(&original.join("publication"), &package.join("publication"));
            let manifest = PackageManifest::from_slice(
                &fs::read(package.join("wamn.json")).expect("read manifest"),
            )
            .expect("parse manifest");
            let ir = ClientContractIr::from_release(
                &manifest.package.id,
                &package.join("generated/contracts"),
                &package.join("publication/attachments.json"),
            )
            .expect("project fixture release");
            let files = emit_tui(&ir, "receiving")
                .expect("emit fixture screens")
                .into_iter()
                .chain(emit_rust_client(&ir).expect("emit fixture bindings"));
            for file in files {
                let path = package.join(file.path());
                fs::create_dir_all(path.parent().expect("file parent"))
                    .expect("create emitted file parent");
                fs::write(path, file.bytes()).expect("write fixture generated source");
            }
            Self { root }
        }
    }

    fn args(screen: Option<&str>) -> ScaffoldArgs {
        ScaffoldArgs {
            package: "receiving".to_owned(),
            screen: screen.map(str::to_owned),
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _removed = fs::remove_dir_all(&self.root);
        }
    }

    fn copy_tree(source: &Path, target: &Path) {
        fs::create_dir_all(target).expect("create fixture directory");
        for entry in fs::read_dir(source).expect("read source directory") {
            let entry = entry.expect("read source entry");
            let target = target.join(entry.file_name());
            if entry.file_type().expect("read source type").is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).expect("copy source file");
            }
        }
    }

    #[test]
    fn one_screen_scaffold_keeps_explicit_generated_fallbacks_and_live_specs() {
        let fixture = Fixture::new();
        let output = scaffold_package(&fixture.root, &args(Some("purchase_order.get")))
            .expect("scaffold one screen");
        let main = fs::read_to_string(output.join("src/main.rs")).expect("read executable entry point");
        assert!(main.contains(
            "async fn main() -> Result<wamn_client_terminal::operator::ExitReason, Box<dyn std::error::Error>>"
        ));
        let copied = fs::read_to_string(output.join("src/screens/purchase_order.rs"))
            .expect("read copied function");
        assert!(copied.contains("use ::generated::screens::purchase_order::{GET_SPEC};"));
        assert!(copied.contains("screen::Screen::new(&GET_SPEC, binding)"));
        assert!(!copied.contains("pub static"));
        assert!(!copied.contains("pub fn update("));
        let library =
            fs::read_to_string(output.join("src/lib.rs")).expect("read direct composition");
        assert!(library.contains("screens::purchase_order::get(binding.clone())"));
        assert!(library.contains("::generated::screens::purchase_order::update(binding.clone())"));
        assert!(!library.contains("HashMap"));
        let cargo =
            fs::read_to_string(output.join("Cargo.toml")).expect("read standalone manifest");
        assert!(cargo.contains("[workspace]\n"));
        assert!(cargo.contains("path = \"../generated/receiving-tui\""));
        let tests = fs::read_to_string(output.join("tests/scaffold_tracks_contract.rs"))
            .expect("read declared interaction tests");
        assert!(tests.contains("fn scaffold_tracks_contract()"));
        assert!(tests.contains("a_changed_declared_kind_fails_the_interaction_assertion"));
        assert!(tests.contains("an_unasserted_additive_result_field_is_allowed"));
        assert!(tests.contains("screen.spec().kind, \"get\""));
    }

    #[test]
    fn a_model_named_generated_cannot_shadow_the_generated_dependency() {
        let fixture = Fixture::new();
        let package = fixture.root.join("packages/receiving");
        let mut ir = ClientContractIr::from_release(
            "receiving",
            &package.join("generated/contracts"),
            &package.join("publication/attachments.json"),
        )
        .expect("project the fixture release");
        ir.models
            .iter_mut()
            .find(|model| model.name == "purchase_order")
            .expect("the fixture declares purchase_order")
            .name = "generated".to_owned();
        let emitted = emit_tui(&ir, "receiving").expect("generated is a valid model name");
        let screens = screen_functions(&ir, "receiving", &emitted).expect("find emitted screens");
        let files = scaffold_files(
            "receiving",
            &ir.package,
            &screens,
            Some(("generated", "get")),
        )
        .expect("scaffold the generated model");
        let library = &files[Path::new("src/lib.rs")];
        assert!(library.contains("pub use ::generated::generated;"));
        assert!(library.contains("::generated::screens::generated::update(binding.clone())"));
        let copied = &files[Path::new("src/screens/generated.rs")];
        assert!(copied.contains("use ::generated::screens::generated::{GET_SPEC};"));
    }

    #[test]
    fn all_screen_scaffolding_copies_every_function_without_frozen_metadata() {
        let fixture = Fixture::new();
        let output = scaffold_package(&fixture.root, &args(None)).expect("scaffold all screens");
        let library =
            fs::read_to_string(output.join("src/lib.rs")).expect("read direct composition");
        assert!(!library.contains("generated::screens::"));
        assert!(library.contains("screens::receiving::record_receipt(binding)"));
        let screen = fs::read_to_string(output.join("src/screens/receiving.rs"))
            .expect("read receiving overrides");
        assert!(screen.contains("pub fn record_receipt("));
        assert!(!screen.contains("ScreenSpec {"));
    }

    #[test]
    fn an_existing_custom_crate_is_never_overwritten() {
        let fixture = Fixture::new();
        let output = scaffold_package(&fixture.root, &args(None)).expect("create custom crate");
        let path = output.join("src/lib.rs");
        fs::write(&path, "developer-owned edits\n").expect("make a developer edit");
        assert!(scaffold_package(&fixture.root, &args(Some("purchase_order.get"))).is_err());
        assert_eq!(
            fs::read_to_string(path).expect("read retained edits"),
            "developer-owned edits\n"
        );
    }

    #[test]
    fn stale_generated_source_refuses_before_creating_custom_files() {
        let fixture = Fixture::new();
        fs::write(
            fixture
                .root
                .join("packages/receiving/generated/receiving-tui/src/screens/purchase_order.rs"),
            "stale source\n",
        )
        .expect("change the generated source");
        let error = scaffold_package(&fixture.root, &args(None))
            .expect_err("refuse stale generated screens");
        assert!(error.to_string().contains("run Generate"));
        assert!(!fixture.root.join("packages/receiving/ui").exists());
    }

    #[test]
    fn package_and_screen_selection_refuse_traversal_and_missing_names() {
        let fixture = Fixture::new();
        for package in ["../receiving", "wamn_receiving", "receiving/other"] {
            let args = ScaffoldArgs {
                package: package.to_owned(),
                screen: None,
            };
            assert!(scaffold_package(&fixture.root, &args).is_err());
        }
        for screen in [
            "purchase_order",
            "purchase_order.get.extra",
            "purchase_order.missing",
        ] {
            assert!(scaffold_package(&fixture.root, &args(Some(screen))).is_err());
        }
        assert!(!fixture.root.join("packages/receiving/ui").exists());
    }
}
