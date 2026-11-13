//! Run the KCL binary with a caller-owned schema and an isolated package cache.
use anyhow::{Context as _, Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
/// Select a KCL binary instead of `kcl` on PATH.
pub const KCL_ENV: &str = "WAMN_KCL";
/// Compile a document with the schema owned by its caller.
pub fn compile_file(file: &Path, name: &str, declaration: &str, source: &str) -> Result<Vec<u8>> {
    let module = SchemaModule::write(name, declaration, source)?;
    let directory = module.root.join("document");
    fs::create_dir_all(&directory)?;
    let copy = directory.join("document.k");
    fs::copy(file, &copy)?;
    run_kcl(&module, &directory, Path::new("document.k")).map_err(|error| {
        anyhow::anyhow!(
            "{}",
            format!("{error:#}").replace(&copy.display().to_string(), &file.display().to_string())
        )
    })
}
/// Compile package authorship in place so relative imports keep their owner.
pub(crate) fn compile_package(
    directory: &Path,
    declaration: &str,
    source: &str,
) -> Result<Vec<u8>> {
    let module = SchemaModule::write("manifest", declaration, source)?;
    run_kcl(&module, directory, Path::new("wamn.k"))
}
/// Run `kcl` on one file in `directory` against one schema module.
fn run_kcl(module: &SchemaModule, directory: &Path, file: &Path) -> Result<Vec<u8>> {
    let kcl = kcl_binary()?;
    // A private package cache: kcl prints "waiting for package-cache lock..."
    // to stdout while another run holds the shared one.
    let output = Command::new(&kcl)
        .env("KCL_PKG_PATH", module.root.join("packages"))
        .arg("run")
        .arg(file)
        .arg("-E")
        .arg(format!(
            "{}={}",
            module.name,
            module.root.join(&module.name).display()
        ))
        .args(["--format", "json", "--disable_none"])
        .current_dir(directory)
        .output()
        .with_context(|| format!("run {}", kcl.display()))?;
    if !output.status.success() {
        bail!(
            "compile {}:\n{}",
            directory.join(file).display(),
            without_terminal_colors(&String::from_utf8_lossy(&output.stderr)).trim_end()
        );
    }
    let json = String::from_utf8(output.stdout).context("kcl wrote JSON that is not UTF-8")?;
    Ok(json.into_bytes())
}

/// Resolve KCL from the environment or the process search path.
fn kcl_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(KCL_ENV) {
        return Ok(PathBuf::from(path));
    }
    let pins: toml::Value = toml::from_str(include_str!("../../../../mise.toml"))?;
    let version = pins["tools"]["http:kcl"]["version"]
        .as_str()
        .context("KCL version is absent")?;
    let local = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../.tools/kcl")
        .join(version)
        .join("kcl");
    Ok(if local.is_file() {
        local
    } else {
        PathBuf::from("kcl")
    })
}

/// A schema module and a package cache, in a private directory for one compile.
struct SchemaModule {
    root: PathBuf,
    name: String,
}

impl SchemaModule {
    fn write(name: &str, declaration: &str, source: &str) -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "wamn-kcl-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let module = Self {
            root,
            name: name.to_owned(),
        };
        let directory = module.root.join(name);
        fs::create_dir_all(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        fs::write(directory.join("kcl.mod"), declaration)
            .with_context(|| format!("write the {name} schema module"))?;
        fs::write(directory.join(format!("{name}.k")), source)
            .with_context(|| format!("write the {name} schema module"))?;
        Ok(module)
    }
}

impl Drop for SchemaModule {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Remove the ANSI color sequences kcl writes into its errors.
fn without_terminal_colors(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
