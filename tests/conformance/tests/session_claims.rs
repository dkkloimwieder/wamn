//! Production SQL must not read identity claims that a guest session can forge.

use std::path::{Path, PathBuf};

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tests/conformance lives two levels below the repository root")
        .to_path_buf()
}

fn production_sql_fragments(
    repository: &Path,
    roots: &[&str],
) -> Result<Vec<(String, usize, String)>, String> {
    let mut files = Vec::new();
    for root in roots {
        collect_files(repository, &repository.join(root), &mut files)?;
    }
    files.sort();
    files.dedup();

    let mut fragments = Vec::new();
    for path in files {
        let relative = relative_path(repository, &path)?;
        let source =
            std::fs::read_to_string(&path).map_err(|error| format!("read {relative}: {error}"))?;
        let extension = path.extension().and_then(|value| value.to_str());
        let literals = match extension {
            Some("rs") if is_test_gated_module(repository, &relative) => continue,
            Some("rs") => rust_string_literals(production_rust_source(&source)),
            Some("sql") => vec![(1, strip_sql_comments(&source))],
            _ => continue,
        };
        for (line, literal) in literals {
            fragments.push((relative.clone(), line, literal));
        }
    }
    Ok(fragments)
}

fn collect_files(
    repository: &Path,
    current: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), String> {
    if current.is_file() {
        let relative = relative_path(repository, current)?;
        if is_excluded(&relative)
            || !matches!(
                current.extension().and_then(|value| value.to_str()),
                Some("rs" | "sql")
            )
        {
            return Ok(());
        }
        files.push(current.to_path_buf());
        return Ok(());
    }
    let entries = std::fs::read_dir(current)
        .map_err(|error| format!("read directory {}: {error}", current.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let relative = relative_path(repository, &entry.path())?;
        if is_excluded(&relative) {
            continue;
        }
        collect_files(repository, &entry.path(), files)?;
    }
    Ok(())
}

// Preserve the existing production scan boundary. These paths contain test
// fixtures, examples, or SQL generator input rather than production SQL.
fn is_excluded(path: &str) -> bool {
    Path::new(path)
        .components()
        .any(|component| matches!(component.as_os_str().to_str(), Some("tests" | "examples")))
        || path_is_within(path, "apps/platform/fixtures")
        || matches!(
            path,
            "deploy/sql/postgres-init.sql" | "crates/schema/generator/src/sql.rs"
        )
}

/// Is this file a module its parent gates with `#[cfg(test)]`?
///
/// [`production_rust_source`] sees one file at a time, so it strips a
/// `#[cfg(test)] mod tests` block but cannot know that a whole FILE is test
/// source because the `mod` declaration naming it is gated in its parent. That
/// blind spot is how a test fixture's seeding INSERT came to demand a
/// declared production writer (`wamn-p3lf`). Recurses, because a module under a
/// test-gated module is test source too.
fn is_test_gated_module(repository: &Path, relative: &str) -> bool {
    let path = Path::new(relative);
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    // A crate root has no `mod` declaration naming it.
    if matches!(stem, "lib" | "main" | "build") {
        return false;
    }
    let Some(directory) = path.parent() else {
        return false;
    };
    // `a/b/mod.rs` is named by `mod b;` one level further up.
    let (name, directory) = if stem == "mod" {
        match (
            directory.file_name().and_then(|name| name.to_str()),
            directory.parent(),
        ) {
            (Some(name), Some(parent)) => (name, parent),
            _ => return false,
        }
    } else {
        (stem, directory)
    };
    let candidates = [
        directory.with_extension("rs"),
        directory.join("mod.rs"),
        directory.join("lib.rs"),
        directory.join("main.rs"),
    ];
    for parent in candidates {
        let Some(parent) = parent.to_str() else {
            continue;
        };
        let Ok(source) = std::fs::read_to_string(repository.join(parent)) else {
            continue;
        };
        if declares_module_under_cfg_test(&source, name) {
            return true;
        }
        return is_test_gated_module(repository, parent);
    }
    false
}

/// Does `source` declare `mod <name>;` immediately under a `#[cfg(test)]`?
fn declares_module_under_cfg_test(source: &str, name: &str) -> bool {
    let declarations = [format!("mod {name};"), format!("pub mod {name};")];
    let mut gated = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        if declarations
            .iter()
            .any(|declaration| trimmed == declaration)
        {
            return gated;
        }
        gated = trimmed == "#[cfg(test)]";
    }
    false
}

fn production_rust_source(source: &str) -> &str {
    let mut offset = 0;
    while let Some(found) = source[offset..].find("#[cfg(test)]") {
        let start = offset + found;
        let tail = &source[start + "#[cfg(test)]".len()..];
        let following = tail.trim_start();
        if following.starts_with("mod tests") {
            return &source[..start];
        }
        offset = start + "#[cfg(test)]".len();
    }
    source
}

fn rust_string_literals(source: &str) -> Vec<(usize, String)> {
    let bytes = source.as_bytes();
    let mut literals = Vec::new();
    let mut index = 0;
    let mut line = 1;
    while index < bytes.len() {
        if bytes.get(index..index + 2) == Some(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if bytes.get(index..index + 2) == Some(b"/*") {
            let mut depth = 1;
            index += 2;
            while index < bytes.len() && depth > 0 {
                if bytes.get(index..index + 2) == Some(b"/*") {
                    depth += 1;
                    index += 2;
                } else if bytes.get(index..index + 2) == Some(b"*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    if bytes[index] == b'\n' {
                        line += 1;
                    }
                    index += 1;
                }
            }
            continue;
        }
        if bytes[index] == b'\n' {
            line += 1;
            index += 1;
            continue;
        }
        if bytes[index] == b'r' {
            let mut hashes = 0;
            let mut cursor = index + 1;
            while cursor < bytes.len() && bytes[cursor] == b'#' {
                hashes += 1;
                cursor += 1;
            }
            if cursor < bytes.len() && bytes[cursor] == b'"' {
                let start_line = line;
                cursor += 1;
                let body_start = cursor;
                while cursor < bytes.len() {
                    if bytes[cursor] == b'\n' {
                        line += 1;
                    }
                    if bytes[cursor] == b'"'
                        && bytes.get(cursor + 1..cursor + 1 + hashes) == Some(&vec![b'#'; hashes])
                    {
                        literals.push((start_line, source[body_start..cursor].to_string()));
                        index = cursor + 1 + hashes;
                        break;
                    }
                    cursor += 1;
                }
                if cursor >= bytes.len() {
                    break;
                }
                continue;
            }
        }
        if bytes[index] == b'"' {
            let start_line = line;
            index += 1;
            let mut literal = String::new();
            while index < bytes.len() {
                match bytes[index] {
                    b'"' => {
                        index += 1;
                        literals.push((start_line, literal));
                        break;
                    }
                    b'\\' if index + 1 < bytes.len() => {
                        index += 1;
                        match bytes[index] {
                            b'\n' => {
                                line += 1;
                                literal.push(' ');
                            }
                            b'n' | b'r' | b't' => literal.push(' '),
                            escaped => literal.push(char::from(escaped)),
                        }
                        index += 1;
                    }
                    b'\n' => {
                        line += 1;
                        literal.push('\n');
                        index += 1;
                    }
                    byte if byte.is_ascii() => {
                        literal.push(char::from(byte));
                        index += 1;
                    }
                    _ => {
                        let character = source[index..]
                            .chars()
                            .next()
                            .expect("index is on a UTF-8 boundary");
                        literal.push(character);
                        index += character.len_utf8();
                    }
                }
            }
            continue;
        }
        index += 1;
    }
    literals
}

fn strip_sql_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| line.split_once("--").map_or(line, |(before, _)| before))
        .collect::<Vec<_>>()
        .join("\n")
}

fn path_is_within(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn relative_path(repository: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(repository)
        .map_err(|error| error.to_string())?
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("non-UTF-8 repository path {}", path.display()))
}

/// The two session claims wamn-0h0g.22.23 measured a guest session can forge.
const SESSION_FORGEABLE_CLAIMS: [&str; 2] = ["app.role", "app.user_id"];

/// Every `current_setting` read of a [`SESSION_FORGEABLE_CLAIMS`] entry in
/// `sql`, as `(claim, call)`.
///
/// Keys on the READ. `set_config('app.role', $5, true)` — the host's own
/// `GUEST_CLAIM_SQL` binding, which wamn-0h0g.23.1 ruled deliberate — WRITES the
/// claims and is not a consumer. Doubled single quotes collapse first, so a
/// policy assembled inside an `EXECUTE` string reads like a literal one. A claim
/// name arriving through a bind parameter or a `format!` hole is out of reach.
fn forgeable_claim_reads(sql: &str) -> Vec<(&'static str, String)> {
    let normalized = sql.to_ascii_lowercase().replace("''", "'");
    let mut reads = Vec::new();
    let mut offset = 0;
    while let Some(found) = normalized[offset..].find("current_setting") {
        let start = offset + found;
        let tail = &normalized[start..];
        let call = tail.find(')').map_or(tail, |end| &tail[..=end]);
        for claim in SESSION_FORGEABLE_CLAIMS {
            if call.contains(claim) {
                reads.push((claim, call.trim().to_owned()));
            }
        }
        offset = start + "current_setting".len();
    }
    reads
}

/// No production RLS policy or generated API reads these claims while
/// wamn-0h0g.22.23 remains unresolved. Its live case showed that a guest could
/// rewrite both claims through a DO-wrapped EXECUTE and access another user's rows.
/// The tenant boundary held because it derives from CURRENT_USER.
///
/// Keep the existing SQL-text scope: production Rust literals, deployment SQL,
/// and application SQL. Test modules, examples, and fixture inputs stay excluded.
#[test]
fn app_role_and_app_user_id_have_no_production_reader_while_the_claim_escape_is_open() {
    // A fence that has quietly stopped matching is the failure this one exists
    // to prevent, so show the discrimination before trusting the scan.
    assert!(
        forgeable_claim_reads(
            "select set_config('app.role', $5, true), set_config('app.user_id', $6, true)"
        )
        .is_empty(),
        "the fence must not fire on the GUEST_CLAIM_SQL binding, which writes \
         the claims (wamn-0h0g.23.1 ruled that deliberate)"
    );
    assert_eq!(
        forgeable_claim_reads(
            "create policy p on t using (owner = nullif(current_setting(''app.user_id'', true), '''')::uuid)"
        )
        .len(),
        1,
        "the fence must see a claim read, including one nested in an EXECUTE string"
    );

    let repository = repository();
    let roots = ["crates", "services", "apps", "deploy/sql"];
    let readers = production_sql_fragments(&repository, &roots)
        .expect("scan production SQL")
        .into_iter()
        .flat_map(|(path, line, sql)| {
            forgeable_claim_reads(&sql)
                .into_iter()
                .map(move |(claim, call)| format!("  {path}:{line} reads {claim} — {call}"))
        })
        .collect::<Vec<_>>();

    assert!(
        readers.is_empty(),
        "production SQL must not read the session-forgeable identity claims \
         `app.role` or `app.user_id` while wamn-0h0g.22.23 is OPEN.\n\n\
         wamn-0h0g.22.23 measured that a guest session rewrites both claims past \
         the claim blocklist with a DO-wrapped EXECUTE, then reads and zeroes \
         another user's rows. A policy that reads either claim is that hole made \
         reachable. Settle wamn-0h0g.22.23 first — the owner ruling of 2026-09-04 \
         is to re-key the per-user layer onto something the session cannot \
         rewrite (the wamn-0h0g.22.6 option (c) `current_user` pattern one layer \
         down), which charters with the identity epic. This fence is \
         wamn-0h0g.22.44; do not widen it to admit the reader.\n\n\
         production readers found:\n{}",
        readers.join("\n")
    );
}
