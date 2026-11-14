//! Copy generated defaults into application-owned source.
use anyhow::{Context as _, Result};
use quote::quote;
use std::{fs, io::Write as _, path::Path};
use syn::{Item, spanned::Spanned as _};

/// Replace an empty action implementation with its current generated default.
/// The operation's types, SQL and validation continue to come from the compiler.
pub fn action(package: &Path, local: &str) -> Result<std::path::PathBuf> {
    let (model, action) = local.split_once('.').context("use model.action")?;
    let output = wamn_compiler::output_root(package);
    let manifest =
        wamn_compiler::manifest::PackageManifest::from_slice(&fs::read(output.join("wamn.json"))?)?;
    let declaration = manifest
        .models
        .get(model)
        .context("model is not declared")?;
    let operation = declaration
        .operations
        .iter()
        .find(|(operation, _)| operation.as_str() == action)
        .map(|(_, declaration)| declaration)
        .context("model action is not declared")?;
    let component = operation
        .component
        .as_ref()
        .or_else(|| {
            (manifest.components.len() == 1)
                .then(|| manifest.components.keys().next())
                .flatten()
        })
        .context("action has no component owner")?;
    let workspace = wamn_compiler::package_workspace(package);
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(workspace.join("Cargo.toml"))
        .no_deps()
        .exec()?;
    let component = metadata
        .packages
        .iter()
        .find(|candidate| candidate.name.as_str() == component.replace('_', "-"))
        .context("component has no Cargo package")?;
    let source = component
        .targets
        .iter()
        .find(|target| target.is_cdylib())
        .context("component has no library")?
        .src_path
        .as_std_path();
    let original = fs::read_to_string(source)?;
    let syntax = syn::parse_file(&original)?;
    let implementation = syntax
        .items
        .iter()
        .find_map(|item| {
            let Item::Impl(implementation) = item else {
                return None;
            };
            let (path, _) = implementation.trait_.as_ref()?;
            let path = path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            (path == ["generated", model, action, "Action"] && implementation.items.is_empty())
                .then_some(implementation)
        })
        .context("action needs an empty generated Action implementation")?;
    let generated = syn::parse_file(&fs::read_to_string(
        output.join("component").join(format!("{model}.rs")),
    )?)?;
    let default = generated
        .items
        .into_iter()
        .find_map(|item| {
            let Item::Mod(module) = item else { return None };
            (module.ident == action).then_some(module)
        })
        .context("compiled action is absent")?;
    let mut function = default
        .content
        .context("compiled action has no body")?
        .1
        .into_iter()
        .find_map(|item| match item {
            Item::Fn(function) if function.sig.ident == "default_handle" => Some(function),
            _ => None,
        })
        .context("compiled action has no default implementation")?;
    function.vis = syn::Visibility::Inherited;
    function.sig.ident = syn::Ident::new("run", function.sig.ident.span());
    let module = syn::Ident::new(&format!("{model}_{action}"), function.sig.ident.span());
    let model_id = syn::Ident::new(model, function.sig.ident.span());
    let action_id = syn::Ident::new(action, function.sig.ident.span());
    let namespace = syn::Ident::new(
        &manifest.package.id.replace('-', "_"),
        function.sig.ident.span(),
    );
    let owned = quote! {
        #[allow(unused_imports)]
        use {
            crate::data::generated::#model_id as model,
            crate::data::generated::error::Error,
            crate::generated::#model_id::#action_id::codec,
            crate::exports::#namespace::#model_id::#action_id as contract,
            wamn_postgres_statements::{Connection, Transaction},
        };
        impl crate::generated::#model_id::#action_id::Action for crate::Component { #function }
    };
    let destination = source.with_file_name(format!("{module}.rs"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)?;
    file.write_all(owned.to_string().as_bytes())?;
    let formatting = std::process::Command::new("rustfmt")
        .args(["--edition", "2024"])
        .arg(&destination)
        .output()?;
    anyhow::ensure!(
        formatting.status.success(),
        "format ejected action: {}",
        String::from_utf8_lossy(&formatting.stderr)
    );
    let span = implementation.span();
    let offset = |line: usize, column: usize| {
        original
            .split_inclusive('\n')
            .take(line - 1)
            .map(str::len)
            .sum::<usize>()
            + column
    };
    let range =
        offset(span.start().line, span.start().column)..offset(span.end().line, span.end().column);
    let mut authored = original;
    authored.replace_range(range, &format!("mod {module};"));
    fs::write(source, authored)?;
    Ok(destination)
}

/// Copy a generated browser component to an application-owned path.
pub fn component(package: &Path, name: &str, destination: &Path) -> Result<()> {
    wamn_compiler::manifest::validate_identifier(name, "component")?;
    let source = wamn_compiler::output_root(package)
        .join("client-ts/components")
        .join(format!("{name}.ts"));
    let source_text = fs::read_to_string(&source)?;
    let origin = source
        .parent()
        .context("generated component directory")?
        .canonicalize()?;
    let authored = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    let common = origin
        .components()
        .zip(authored.components())
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = std::path::PathBuf::new();
    for _ in authored.components().skip(common) {
        relative.push("..");
    }
    relative.extend(origin.components().skip(common));
    relative.push("presentation.js");
    let relative = relative.to_string_lossy().replace('\\', "/");
    let relative = if relative.starts_with('.') {
        relative
    } else {
        format!("./{relative}")
    };
    let source_text =
        source_text.replace("\"./presentation.js\"", &serde_json::to_string(&relative)?);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    file.write_all(source_text.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ejected_action_owns_the_default_and_component_copy_refuses_overwrite() {
        let scratch = tempfile::tempdir().unwrap();
        let workspace = scratch.path().join("apps");
        let package = workspace.join("package");
        let component_root = package.join("component");
        fs::create_dir_all(component_root.join("src")).unwrap();
        fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"package/component\"]\n",
        )
        .unwrap();
        fs::write(component_root.join("Cargo.toml"), "[package]\nname = \"component\"\nversion = \"1.0.0\"\nedition = \"2024\"\n[lib]\ncrate-type = [\"cdylib\"]\n").unwrap();
        let source = component_root.join("src/lib.rs");
        fs::write(
            &source,
            r#"extern crate self as wamn_postgres_statements;
            pub struct Connection;
            pub struct Transaction;
            mod data {
                pub mod generated {
                    pub mod thing { pub struct Row; }
                    pub mod error { pub struct Error; }
                }
            }
            mod exports { pub mod package { pub mod thing { pub mod get {
                pub type GetRequest = ();
                pub type GetResult = ();
            }}}}
            mod generated {
                use crate::data::generated as data;
                pub mod thing { pub mod get {
                    pub(crate) mod codec {}
                    pub trait Action {
                        async fn run(request: crate::exports::package::thing::get::GetRequest)
                            -> crate::exports::package::thing::get::GetResult { request }
                    }
                }}
            }
            struct Component;
            impl generated::thing::get::Action for Component {}
            "#,
        )
        .unwrap();
        let output = wamn_compiler::output_root(&package);
        fs::create_dir_all(output.join("component")).unwrap();
        fs::write(output.join("wamn.json"), serde_json::to_vec(&serde_json::json!({
            "package": {"id": "package", "version": "1.0.0"},
            "required_platform_policy_contract": {"id": "access", "state": "satisfied"},
            "models": {"thing": {"schema": "data", "table": "thing", "owner": "package", "audit_log": {"columns": [], "retention": "none"}, "operations": {"get": {"permission": "thing.get", "result": "one"}}}},
            "connections": {}, "components": {"component": {}}
        })).unwrap()).unwrap();
        fs::write(output.join("component/thing.rs"), "pub mod get { pub async fn default_handle(request: contract::GetRequest) -> contract::GetResult { let _row = model::Row; let _error: Option<Error> = None; request } }").unwrap();
        action(&package, "thing.get").unwrap();
        let compiled = std::process::Command::new("rustc")
            .args([
                "--edition",
                "2024",
                "--crate-type",
                "lib",
                "--emit",
                "metadata",
            ])
            .arg(&source)
            .arg("-o")
            .arg(scratch.path().join("component.rmeta"))
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        assert!(action(&package, "thing.get").is_err());
        fs::create_dir_all(output.join("client-ts/components")).unwrap();
        fs::write(
            output.join("client-ts/components/presentation.ts"),
            "export interface ScreenPresentation { readonly view: string; }",
        )
        .unwrap();
        fs::write(
            output.join("client-ts/components/thing.ts"),
            "import type { ScreenPresentation } from \"./presentation.js\"; export const thing = { view: 'thing' } satisfies ScreenPresentation;",
        )
        .unwrap();
        let destination = package.join("thing.ts");
        component(&package, "thing", &destination).unwrap();
        let compiled = std::process::Command::new("tsc")
            .args([
                "--strict",
                "--noEmit",
                "--module",
                "nodenext",
                "--moduleResolution",
                "nodenext",
            ])
            .arg(&destination)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}{}",
            String::from_utf8_lossy(&compiled.stdout),
            String::from_utf8_lossy(&compiled.stderr)
        );
        assert!(component(&package, "thing", &destination).is_err());
    }
}
