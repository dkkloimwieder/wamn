//! Numbered SQL migrations have unique numbers within each directory.
//!
//! Names follow `NNNN_<name>.sql` (docs/history/schema-upgrade.md §2), with
//! lowercase letters, digits and underscores as required by the constraints
//! in deploy/sql/{app,system}-schema-migrations.sql.

use std::collections::BTreeMap;
use std::path::Path;

use super::Problems;

pub(super) fn check(root: &Path, problems: &mut Problems) {
    let mut stack = vec![root.join("deploy/sql/migrations")];
    while let Some(directory) = stack.pop() {
        let relative = directory.strip_prefix(root).unwrap_or(&directory);
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                problems.push(format!("read {}: {error}", relative.display()));
                continue;
            }
        };
        let mut paths = entries
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        let mut numbers = BTreeMap::new();
        for path in paths {
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(number) = migration_number(name) else {
                continue;
            };
            if let Some(previous) = numbers.insert(number.to_owned(), name.to_owned()) {
                problems.push(format!(
                    "{}: migration number {number} is shared by {previous} and {name}",
                    relative.display()
                ));
            }
        }
    }
}

fn migration_number(name: &str) -> Option<&str> {
    let (number, label) = name.strip_suffix(".sql")?.split_once('_')?;
    (number.len() == 4
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && !label.is_empty()
        && label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'))
    .then_some(number)
}

#[cfg(test)]
mod tests {
    use super::{Problems, check};

    fn problems_in(name: &str, files: &[&str]) -> Vec<String> {
        let root = std::env::temp_dir().join(format!(
            "wamn-migration-numbers-{name}-{}",
            std::process::id()
        ));
        for file in files {
            let path = root.join("deploy/sql/migrations").join(file);
            std::fs::create_dir_all(path.parent().expect("a migration has a directory"))
                .expect("create the fixture tree");
            std::fs::write(path, "").expect("write a migration");
        }
        let mut problems = Problems::default();
        check(&root, &mut problems);
        std::fs::remove_dir_all(&root).expect("remove the fixture tree");
        problems.0
    }

    #[test]
    fn duplicate_numbers_in_one_directory_are_refused() {
        assert_eq!(
            problems_in(
                "duplicate",
                &[
                    "project/nested/0040_second.sql",
                    "project/nested/0040_first.sql",
                ]
            ),
            [
                "deploy/sql/migrations/project/nested: migration number 0040 is shared by 0040_first.sql and 0040_second.sql"
            ]
        );
    }

    #[test]
    fn distinct_numbers_pass_and_other_names_do_not_count() {
        let problems = problems_in(
            "distinct",
            &[
                "project/0040_first.sql",
                "project/0041_second.sql",
                "project/0040_notes.txt",
                "project/0040_.sql",
                "project/0040_bad-name.sql",
                "project/0040_UPPER.sql",
                "project/040_short.sql",
                "project/00040_long.sql",
            ],
        );
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn the_same_number_in_different_directories_passes() {
        let problems = problems_in(
            "directories",
            &[
                "project/0040_first.sql",
                "system/0040_second.sql",
                "project/nested/0040_third.sql",
            ],
        );
        assert!(problems.is_empty(), "{problems:?}");
    }
}
