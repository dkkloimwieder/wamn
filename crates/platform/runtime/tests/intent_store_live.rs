//! The Postgres intent store runs the intent store case set (`wamn-an24.2`).
//!
//! One disposable database holds the run plane of record, and the executor
//! generation holds the production surface. Each case binds two components to
//! two tenants that no other case uses, so row-level security gives each case
//! an empty record, as a fresh SQLite file does.

use std::sync::Arc;

use wamn_project_state::PlatformComponent;
use wamn_run_state::IntentStore;
use wamn_run_state::intent_cases::{self, CaseStores, IntentStoreFixture};
use wamn_runtime::plugins::wamn_postgres::{PostgresIntentStore, WamnPostgres};

mod common;

use common::{SCHEMA, install_fixture, teardown};

struct Tenants {
    plugin: Arc<WamnPostgres>,
}

impl Tenants {
    fn store(&self, tenant: &str) -> Arc<dyn IntentStore> {
        let component = format!("intents-{tenant}");
        let executor = PlatformComponent::Executor;
        self.plugin.set_tenant(&component, tenant).expect("tenant");
        self.plugin.set_schema(&component, SCHEMA).expect("schema");
        self.plugin
            .set_runner(&component, &component)
            .expect("runner");
        self.plugin
            .set_user_id(&component, &executor.principal_id().to_string())
            .expect("principal");
        self.plugin
            .set_operation(&component, executor.principal_name())
            .expect("operation");
        Arc::new(PostgresIntentStore::new(
            Arc::clone(&self.plugin),
            component,
        ))
    }
}

#[async_trait::async_trait]
impl IntentStoreFixture for Tenants {
    async fn stores(&self, case: &'static str) -> CaseStores {
        let tenant = format!("{case}-a").replace('_', "-");
        let other_tenant = format!("{case}-b").replace('_', "-");
        CaseStores {
            store: self.store(&tenant),
            other: self.store(&other_tenant),
            tenant,
            other_tenant,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_postgres_intent_store_keeps_every_intent_rule() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_catalog::test_database::tenant();
    let fixture = install_fixture(database.url()).await?;
    let tenants = Tenants {
        plugin: Arc::clone(&fixture.plugin),
    };
    for case in intent_cases::CASES {
        intent_cases::run(&tenants, case).await;
    }
    drop(tenants);
    teardown(fixture).await
}
