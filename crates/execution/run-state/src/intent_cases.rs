//! The intent store case set: the rules every [`IntentStore`] keeps, as
//! async cases that any store runs (`wamn-an24.2`).
//!
//! The SQLite store runs each case in its own test, over a fresh file. The
//! Postgres store runs them over one disposable database, with fresh tenants
//! for each case. A case asks its fixture for stores by case name, so no case
//! sees another's rows.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::IntentStore;
use crate::intent_store::{Begun, Intent, IntentId, StoreErrorKind, StoredOutcome};
use crate::operator_action::OperatorActionBasis;

/// The stores of one case: a store for a tenant that no other case uses, and
/// a store for a second such tenant. A store that serves every tenant gives
/// the same store twice.
pub struct CaseStores {
    pub store: Arc<dyn IntentStore>,
    pub tenant: String,
    pub other: Arc<dyn IntentStore>,
    pub other_tenant: String,
}

impl std::fmt::Debug for CaseStores {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaseStores")
            .field("tenant", &self.tenant)
            .field("other_tenant", &self.other_tenant)
            .finish_non_exhaustive()
    }
}

/// Opens empty stores for one case.
#[async_trait]
pub trait IntentStoreFixture: Sync {
    async fn stores(&self, case: &'static str) -> CaseStores;
}

/// Every case, by name, for a store that runs them all in one test.
pub const CASES: [&str; 8] = [
    "a_finished_key_returns_its_stored_outcome",
    "a_begun_key_is_uncertain_and_never_new_again",
    "a_repeated_key_with_another_input_conflicts",
    "keys_belong_to_their_tenant",
    "finish_closes_an_intent_once",
    "uncertain_lists_open_intents_oldest_first_up_to_the_limit",
    "a_resolved_intent_leaves_the_list_and_answers_its_basis",
    "a_finished_intent_does_not_resolve",
];

/// Run the case named `case`.
pub async fn run(fixture: &dyn IntentStoreFixture, case: &'static str) {
    match case {
        "a_finished_key_returns_its_stored_outcome" => {
            a_finished_key_returns_its_stored_outcome(fixture).await;
        }
        "a_begun_key_is_uncertain_and_never_new_again" => {
            a_begun_key_is_uncertain_and_never_new_again(fixture).await;
        }
        "a_repeated_key_with_another_input_conflicts" => {
            a_repeated_key_with_another_input_conflicts(fixture).await;
        }
        "keys_belong_to_their_tenant" => keys_belong_to_their_tenant(fixture).await,
        "finish_closes_an_intent_once" => finish_closes_an_intent_once(fixture).await,
        "uncertain_lists_open_intents_oldest_first_up_to_the_limit" => {
            uncertain_lists_open_intents_oldest_first_up_to_the_limit(fixture).await;
        }
        "a_resolved_intent_leaves_the_list_and_answers_its_basis" => {
            a_resolved_intent_leaves_the_list_and_answers_its_basis(fixture).await;
        }
        "a_finished_intent_does_not_resolve" => a_finished_intent_does_not_resolve(fixture).await,
        other => panic!("no intent store case is named {other}"),
    }
}

fn intent<'a>(tenant: &'a str, key: &'a str, input_hash: &'a str) -> Intent<'a> {
    Intent {
        tenant,
        release: "release-1",
        package: "scale",
        operation: "record_sample",
        idempotency_key: key,
        input_hash,
        deadline_ms: 5_000,
    }
}

fn new_id(begun: Begun) -> IntentId {
    match begun {
        Begun::New(id) => id,
        other => panic!("expected a new intent, got {other:?}"),
    }
}

pub async fn a_finished_key_returns_its_stored_outcome(fixture: &dyn IntentStoreFixture) {
    let CaseStores { store, tenant, .. } = fixture
        .stores("a_finished_key_returns_its_stored_outcome")
        .await;
    for (key, outcome) in [
        (
            "k-completed",
            StoredOutcome::Completed(json!({"grams": 1250})),
        ),
        (
            "k-failed",
            StoredOutcome::Failed(json!({"code": "refused"})),
        ),
    ] {
        let id = new_id(
            store
                .begin(&intent(&tenant, key, "h1"))
                .await
                .expect("begin"),
        );
        store.finish(&id, &outcome).await.expect("finish");
        assert_eq!(
            store
                .begin(&intent(&tenant, key, "h1"))
                .await
                .expect("begin again"),
            Begun::Finished(outcome)
        );
    }
    assert!(store.uncertain(10).await.expect("uncertain").is_empty());
}

pub async fn a_begun_key_is_uncertain_and_never_new_again(fixture: &dyn IntentStoreFixture) {
    let CaseStores { store, tenant, .. } = fixture
        .stores("a_begun_key_is_uncertain_and_never_new_again")
        .await;
    let id = new_id(
        store
            .begin(&intent(&tenant, "k1", "h1"))
            .await
            .expect("begin"),
    );
    assert_eq!(
        store
            .begin(&intent(&tenant, "k1", "h1"))
            .await
            .expect("begin again"),
        Begun::Uncertain(id)
    );
}

pub async fn a_repeated_key_with_another_input_conflicts(fixture: &dyn IntentStoreFixture) {
    let CaseStores { store, tenant, .. } = fixture
        .stores("a_repeated_key_with_another_input_conflicts")
        .await;
    let id = new_id(
        store
            .begin(&intent(&tenant, "k1", "h1"))
            .await
            .expect("begin"),
    );
    assert_eq!(
        store
            .begin(&intent(&tenant, "k1", "h2"))
            .await
            .expect("begin again"),
        Begun::Conflict(id.clone())
    );
    assert_eq!(
        store
            .begin(&intent(&tenant, "k1", "h1"))
            .await
            .expect("begin a third time"),
        Begun::Uncertain(id),
        "a conflict leaves the stored intent unchanged"
    );
}

pub async fn keys_belong_to_their_tenant(fixture: &dyn IntentStoreFixture) {
    let CaseStores {
        store,
        tenant,
        other,
        other_tenant,
    } = fixture.stores("keys_belong_to_their_tenant").await;
    store
        .begin(&intent(&tenant, "k1", "h1"))
        .await
        .expect("begin");
    new_id(
        other
            .begin(&intent(&other_tenant, "k1", "h2"))
            .await
            .expect("another tenant's key is new"),
    );
}

pub async fn finish_closes_an_intent_once(fixture: &dyn IntentStoreFixture) {
    let CaseStores { store, tenant, .. } = fixture.stores("finish_closes_an_intent_once").await;
    let id = new_id(
        store
            .begin(&intent(&tenant, "k1", "h1"))
            .await
            .expect("begin"),
    );
    let outcome = StoredOutcome::Completed(json!(null));
    store.finish(&id, &outcome).await.expect("finish");
    let error = store
        .finish(&id, &outcome)
        .await
        .expect_err("a finished intent does not finish again");
    assert_eq!(error.kind(), StoreErrorKind::Contract);
    let error = store
        .finish(&IntentId("999999".into()), &outcome)
        .await
        .expect_err("an unknown intent does not finish");
    assert_eq!(error.kind(), StoreErrorKind::Contract);
}

pub async fn uncertain_lists_open_intents_oldest_first_up_to_the_limit(
    fixture: &dyn IntentStoreFixture,
) {
    let CaseStores { store, tenant, .. } = fixture
        .stores("uncertain_lists_open_intents_oldest_first_up_to_the_limit")
        .await;
    let first = new_id(
        store
            .begin(&intent(&tenant, "k1", "h1"))
            .await
            .expect("begin"),
    );
    let finished = new_id(
        store
            .begin(&intent(&tenant, "k2", "h2"))
            .await
            .expect("begin"),
    );
    let third = new_id(
        store
            .begin(&intent(&tenant, "k3", "h3"))
            .await
            .expect("begin"),
    );
    store
        .finish(&finished, &StoredOutcome::Completed(json!(1)))
        .await
        .expect("finish");

    let open: Vec<IntentId> = store
        .uncertain(10)
        .await
        .expect("uncertain")
        .into_iter()
        .map(|intent| intent.id)
        .collect();
    assert_eq!(open, [first.clone(), third]);

    let limited = store.uncertain(1).await.expect("uncertain");
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].id, first);
    assert_eq!(limited[0].tenant, tenant);
    assert_eq!(limited[0].idempotency_key, "k1");
    assert_eq!(limited[0].operation, "record_sample");
}

pub async fn a_resolved_intent_leaves_the_list_and_answers_its_basis(
    fixture: &dyn IntentStoreFixture,
) {
    let CaseStores { store, tenant, .. } = fixture
        .stores("a_resolved_intent_leaves_the_list_and_answers_its_basis")
        .await;
    let id = new_id(
        store
            .begin(&intent(&tenant, "k1", "h1"))
            .await
            .expect("begin"),
    );
    store
        .resolve(&id, OperatorActionBasis::OperatorJudgment)
        .await
        .expect("resolve");
    assert!(store.uncertain(10).await.expect("uncertain").is_empty());
    assert_eq!(
        store
            .begin(&intent(&tenant, "k1", "h1"))
            .await
            .expect("begin again"),
        Begun::Resolved {
            id: id.clone(),
            basis: OperatorActionBasis::OperatorJudgment
        }
    );
    let error = store
        .resolve(&id, OperatorActionBasis::OperatorJudgment)
        .await
        .expect_err("a resolved intent does not resolve again");
    assert_eq!(error.kind(), StoreErrorKind::Contract);
    let error = store
        .finish(&id, &StoredOutcome::Completed(json!(1)))
        .await
        .expect_err("a resolved intent does not finish");
    assert_eq!(error.kind(), StoreErrorKind::Contract);
}

pub async fn a_finished_intent_does_not_resolve(fixture: &dyn IntentStoreFixture) {
    let CaseStores { store, tenant, .. } =
        fixture.stores("a_finished_intent_does_not_resolve").await;
    let id = new_id(
        store
            .begin(&intent(&tenant, "k1", "h1"))
            .await
            .expect("begin"),
    );
    store
        .finish(&id, &StoredOutcome::Completed(json!(1)))
        .await
        .expect("finish");
    let error = store
        .resolve(&id, OperatorActionBasis::ExternalEvidence)
        .await
        .expect_err("a finished intent is not uncertain");
    assert_eq!(error.kind(), StoreErrorKind::Contract);
}
