use std::{env, fs, path::Path};

fn main() {
    let root =
        Path::new(&env::var("CARGO_MANIFEST_DIR").expect("Cargo supplies the manifest directory"))
            .join("../../../deploy/sql");
    let mut generated = String::new();
    for scope in ["system", "project"] {
        let directory = root.join("migrations").join(scope);
        println!("cargo:rerun-if-changed={}", directory.display());
        let mut files = fs::read_dir(&directory)
            .expect("read migration directory")
            .map(|entry| entry.expect("read migration entry").path())
            .filter(|path| path.file_name().is_none_or(|name| name != ".gitkeep"))
            .collect::<Vec<_>>();
        files.sort();
        generated.push_str(&format!(
            "/// Ordered migration IDs and exact bytes from the image.\npub const {}_MIGRATIONS: &[(&str, &[u8])] = &[\n",
            scope.to_uppercase()
        ));
        for path in files {
            assert!(
                path.is_file(),
                "migration entry must be a file: {}",
                path.display()
            );
            let name = path
                .file_name()
                .expect("migration has a name")
                .to_str()
                .expect("migration name is UTF-8");
            let id = format!("migrations/{scope}/{name}");
            generated.push_str(&format!("({id:?}, include_bytes!({path:?})),\n"));
            println!("cargo:rerun-if-changed={}", path.display());
        }
        generated.push_str("];\n");
    }
    let output = Path::new(&env::var("OUT_DIR").expect("Cargo supplies the output directory"))
        .join("migrations.rs");
    fs::write(output, generated).expect("write embedded migration lists");
}
