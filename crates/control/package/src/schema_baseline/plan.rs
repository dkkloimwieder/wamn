use super::{InitializationIdentity, MigrationFile, SchemaPlan, SchemaReport};
use wamn_control_provision::schema_migrations::MigrationDatabase;
use wamn_schema_control::migrations::{MigrationPlan, plan_migrations, validate_execution_sql};
use wamn_schema_control::{
    MigrationOutcome, MigrationSource, PackageMigrationError, PackageMigrationErrorType,
    RecordedMigration,
};

pub(super) fn migrations(
    sources: &[MigrationSource],
    recorded: &[RecordedMigration],
    prefix: &str,
) -> Result<(MigrationPlan, Option<PackageMigrationError>), PackageMigrationError> {
    let (planned, missing) = match plan_migrations(sources, recorded, prefix, None) {
        Ok(plan) => (plan, None),
        Err(error)
            if error.error_type() == PackageMigrationErrorType::MigrationDrift
                && error.actual_hash() == Some("missing") =>
        {
            // Validate every available checksum even when this image predates the history.
            let plan = plan_migrations(sources, &recorded[..sources.len()], prefix, None)?;
            (plan, Some(error))
        }
        Err(error) => return Err(error),
    };
    for (file, sql) in planned.pending.iter().zip(&planned.statements) {
        validate_execution_sql(file, sql)?;
    }
    Ok((planned, missing))
}

pub(super) fn files(
    sources: &[MigrationSource],
    target: MigrationDatabase,
) -> anyhow::Result<Vec<MigrationFile>> {
    let (label, _, _) = super::target_names(target);
    let planned = plan_migrations(sources, &[], &format!("migrations/{label}/"), None)?;
    Ok(planned
        .pending
        .into_iter()
        .map(|file| MigrationFile {
            ordinal: file.ordinal,
            relative_path: file.relative_path,
            sha256: file.sha256,
        })
        .collect())
}

pub(super) fn report(
    target: MigrationDatabase,
    planned: &SchemaPlan,
    sources: &[MigrationSource],
) -> SchemaReport {
    observe(
        target,
        Some(&planned.initialization),
        &planned.recorded,
        sources,
    )
}

pub(super) fn observe(
    target: MigrationDatabase,
    initialization: Option<&InitializationIdentity>,
    recorded: &[RecordedMigration],
    sources: &[MigrationSource],
) -> SchemaReport {
    let (label, _, candidate) = super::target_names(target);
    let mut installed = Vec::new();
    let mut uncertain = Vec::new();
    let mut missing = Vec::new();
    let mut warnings = Vec::new();
    let comparable = match initialization {
        Some(identity) if identity.sha256 != candidate => {
            warnings.push(format!(
                "{label} initialization digest differs: installed={} candidate={candidate}",
                identity.sha256
            ));
            false
        }
        None => {
            warnings.push("initialization is absent".into());
            true
        }
        Some(_) => true,
    };
    let image = match files(sources, target) {
        Ok(files) => Some(files),
        Err(error) => {
            warnings.push(format!(
                "selected {label} SQL cannot be compared: {error:#}"
            ));
            None
        }
    };
    let mut matching = comparable && image.is_some();
    for row in recorded {
        let file = MigrationFile {
            ordinal: row.ordinal,
            relative_path: row.relative_path.clone(),
            sha256: row.sha256.clone(),
        };
        if comparable && let Some(image) = &image {
            match image
                .iter()
                .find(|source| source.relative_path == row.relative_path)
            {
                None => missing.push(file.clone()),
                Some(source) if source.sha256 != row.sha256 || source.ordinal != row.ordinal => {
                    warnings.push(format!(
                        "recorded {label} file {} differs: installed={} ordinal={} candidate={} ordinal={}",
                        row.relative_path, row.sha256, row.ordinal, source.sha256, source.ordinal
                    ));
                    matching = false;
                }
                Some(_) => {}
            }
        }
        if row.outcome == MigrationOutcome::Started {
            uncertain.push(file);
        } else {
            installed.push(file);
        }
    }
    let pending = image
        .into_iter()
        .flatten()
        .filter(|file| matching && !recorded.iter().any(|row| row.ordinal == file.ordinal))
        .collect::<Vec<_>>();
    for file in &missing {
        warnings.push(format!(
            "selected image lacks recorded {label} file {}",
            file.relative_path
        ));
    }
    if !pending.is_empty() {
        warnings.push(format!("selected image has pending {label} SQL; inspect wamn platform schema diff --target {label} --chart DIR"));
    }
    for file in &uncertain {
        warnings.push(format!("uncertain (started): {}; inspect its effects and use wamn platform schema apply --target {label} --chart DIR --resolve {}=applied|retry", file.relative_path, file.relative_path));
    }
    SchemaReport {
        initialization: initialization.cloned(),
        installed,
        pending,
        uncertain,
        missing,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_reports_digest_and_checksum_differences_without_refusal() {
        let target = MigrationDatabase::System;
        let sources = vec![MigrationSource {
            relative_path: "migrations/system/0001_done.sql".into(),
            bytes: b"SELECT 1;".to_vec(),
        }];
        let image = files(&sources, target).unwrap();
        let identity = InitializationIdentity {
            sha256: "installed-digest".into(),
            installed_at: "original-identity".into(),
        };
        let recorded = vec![RecordedMigration {
            ordinal: image[0].ordinal,
            relative_path: image[0].relative_path.clone(),
            sha256: image[0].sha256.clone(),
            outcome: MigrationOutcome::Applied,
        }];
        let observed = observe(target, Some(&identity), &recorded, &sources);
        assert_eq!(observed.initialization.as_ref(), Some(&identity));
        assert_eq!(observed.installed, image);
        assert!(observed.pending.is_empty());
        assert!(observed.missing.is_empty());
        let candidate = super::super::target_names(target).2;
        assert!(
            observed.warnings.iter().any(|warning| {
                warning.contains(&identity.sha256) && warning.contains(&candidate)
            })
        );

        let identity = InitializationIdentity {
            sha256: candidate,
            ..identity
        };
        let changed = vec![MigrationSource {
            bytes: b"SELECT 2;".to_vec(),
            ..sources.into_iter().next().unwrap()
        }];
        let candidate = files(&changed, target).unwrap();
        let observed = observe(target, Some(&identity), &recorded, &changed);
        assert_eq!(observed.initialization.as_ref(), Some(&identity));
        assert_eq!(observed.installed, image);
        assert!(observed.pending.is_empty());
        assert!(observed.warnings.iter().any(|warning| {
            warning.contains(&recorded[0].sha256) && warning.contains(&candidate[0].sha256)
        }));
    }

    #[test]
    fn report_separates_initialization_installed_pending_and_uncertain() {
        let sources = vec![
            MigrationSource {
                relative_path: "migrations/system/0001_done.sql".into(),
                bytes: b"SELECT 1;".to_vec(),
            },
            MigrationSource {
                relative_path: "migrations/system/0002_uncertain.sql".into(),
                bytes: b"-- wamn:no-transaction\nSELECT 2;".to_vec(),
            },
            MigrationSource {
                relative_path: "migrations/system/0003_pending.sql".into(),
                bytes: b"SELECT 3;".to_vec(),
            },
        ];
        let all = files(&sources, MigrationDatabase::System).unwrap();
        let recorded = all[..2]
            .iter()
            .enumerate()
            .map(|(index, file)| RecordedMigration {
                ordinal: file.ordinal,
                relative_path: file.relative_path.clone(),
                sha256: file.sha256.clone(),
                outcome: if index == 0 {
                    MigrationOutcome::Applied
                } else {
                    MigrationOutcome::Started
                },
            })
            .collect::<Vec<_>>();
        let (migrations, missing) = migrations(&sources, &recorded, "migrations/system/").unwrap();
        let planned = SchemaPlan {
            initialization: InitializationIdentity {
                sha256: super::super::target_names(MigrationDatabase::System).2,
                installed_at: "identity".into(),
            },
            adoption: false,
            migrations,
            recorded,
            missing,
        };
        let observed = report(MigrationDatabase::System, &planned, &sources);
        assert_eq!(
            observed.initialization.as_ref(),
            Some(&planned.initialization)
        );
        assert_eq!(observed.installed, all[..1]);
        assert_eq!(observed.uncertain, all[1..2]);
        assert_eq!(observed.pending, all[2..]);
        assert!(observed.missing.is_empty());
        let (migrations, missing) =
            super::migrations(&sources[..1], &planned.recorded, "migrations/system/").unwrap();
        let planned = SchemaPlan {
            migrations,
            missing,
            ..planned
        };
        let observed = report(MigrationDatabase::System, &planned, &sources[..1]);
        assert_eq!(observed.missing, all[1..2]);
        assert!(observed.pending.is_empty());
    }
}
