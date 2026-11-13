use anyhow::Context as _;
use tokio_postgres::{Client, Transaction};

pub use crate::migrations::{MigrationResolution, Resolution, resolution_package};

pub(super) async fn begin<'a>(
    client: &'a mut Client,
    env_key: &str,
) -> anyhow::Result<Transaction<'a>> {
    let tx = client.transaction().await.context("begin package SQL")?;
    tx.query_one(super::ASSIGN_ENV_KEY_SQL, &[&env_key])
        .await
        .context("bind package identity")?;
    super::bind_apply_package_principal(&tx).await?;
    Ok(tx)
}

/// Package authority stays outside the shared migration rules.
pub(super) struct PackageMigrationHooks<'a> {
    pub env_key: &'a str,
    pub package: &'a super::PreparedPackage,
    pub plan: &'a wamn_schema_control::PackageMigrationPlan,
    pub coordinate: &'a str,
    pub initial: bool,
    pub artifact_digest: Option<&'a str>,
    pub local_comment: Option<wamn_runtime::local_application::LocalTargetComment>,
    pub changed: bool,
    pub drops: wamn_broker::auth_changes::AuthDrops,
}

impl crate::migrations::MigrationHooks for PackageMigrationHooks<'_> {
    async fn begin<'a>(&mut self, client: &'a mut Client) -> anyhow::Result<Transaction<'a>> {
        begin(client, self.env_key).await
    }

    async fn initialize(&mut self, tx: &Transaction<'_>) -> anyhow::Result<()> {
        if self.initial {
            self.changed |= crate::connection_requirement::install(
                tx,
                self.env_key,
                &self.package.root,
                &self.package.manifest,
                self.package.component_source.as_ref(),
            )
            .await?
                != 0;
        }
        tx.batch_execute(&wamn_control_provision::sql::install_platform_extensions_sql())
            .await
            .context("install the platform extensions")?;
        super::ensure_model_schemas(tx, self.plan, &self.package.manifest).await
    }

    async fn before(
        &mut self,
        tx: &Transaction<'_>,
        file: &wamn_schema_control::PendingMigration,
        accept: bool,
    ) -> anyhow::Result<()> {
        let mutations = self
            .package
            .policy
            .mutations
            .iter()
            .filter(|mutation| mutation.relative_path.as_ref() == file.relative_path)
            .collect::<Vec<_>>();
        super::definition_ownership::validate_definition_ownership_before_apply(
            tx,
            self.env_key,
            self.coordinate,
            self.plan.coordinate.package_id(),
            &self.package.manifest,
            if accept { &[] } else { &mutations },
        )
        .await
    }

    async fn transactional(
        &mut self,
        tx: &Transaction<'_>,
        sql: &wamn_schema_control::SqlStatement,
    ) -> anyhow::Result<()> {
        super::roles::execute_migration(tx, &self.package.manifest, sql, true).await
    }

    async fn nontransactional(
        &mut self,
        client: &Client,
        file: &wamn_schema_control::PendingMigration,
        sql: &wamn_schema_control::SqlStatement,
    ) -> anyhow::Result<()> {
        super::roles::execute_migration(client, &self.package.manifest, sql, false).await
            .with_context(|| format!("migration {} has an uncertain outcome; inspect its effects and use --resolve {}:{}=applied|retry", file.relative_path, self.plan.coordinate.package_id(), file.relative_path))
    }

    async fn after(
        &mut self,
        tx: &Transaction<'_>,
        file: &wamn_schema_control::PendingMigration,
    ) -> anyhow::Result<()> {
        let mutations = self
            .package
            .policy
            .mutations
            .iter()
            .filter(|mutation| mutation.relative_path.as_ref() == file.relative_path)
            .collect::<Vec<_>>();
        self.changed |= super::definition_ownership::reconcile_definition_ownership(
            tx,
            self.env_key,
            self.coordinate,
            self.plan.coordinate.package_id(),
            &self.package.manifest,
            &mutations,
        )
        .await?;
        Ok(())
    }

    async fn finish(&mut self, tx: &Transaction<'_>) -> anyhow::Result<()> {
        let manifest = &self.package.manifest;
        let directory = &self.package.directory;
        super::roles::assert_host_role(tx).await?;
        self.changed |= super::record_history::create_history_tables(tx, manifest).await?;
        super::entity_maps::reconcile_entity_maps(tx, self.plan, manifest).await?;
        self.changed |=
            super::record_history::reconcile_record_history_triggers(tx, manifest).await?;
        let fingerprint = super::installed_physical_fingerprint(tx, directory, manifest).await?;
        self.changed |= super::package_version::record_package_schema(
            tx,
            self.env_key,
            &self.plan.coordinate,
            self.artifact_digest,
            &fingerprint,
        )
        .await
        .context("record the completed package schema")?;
        if self.initial {
            self.changed |=
                super::operation_grants::ensure_admin_role(tx, self.env_key, &mut self.drops)
                    .await?;
        }
        if let Some(comment) = self.local_comment.as_mut() {
            self.changed |= super::local_target::record_manifest(
                tx,
                comment,
                self.coordinate,
                &super::local_target::manifest_digest(directory),
            )
            .await?;
        }
        Ok(())
    }
}
