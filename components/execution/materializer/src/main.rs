//! Durable JetStream registration delivery through the shared router bridge.

wit_bindgen::generate!({
    world: "materializer",
    path: "wit",
    generate_all,
});

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_json::Value;
use wamn_event_reg::{EventRegistration, RegistrationInput};
use wamn_event_wire::{DerivedEvent, Envelope};
use wamn_materializer::{
    DecideError, MAX_CAUSATION_DEPTH, RefuseReason, SkipReason, Verdict, VerifiedSourceEventId,
    decide, decide_derived, serviceable,
    sql::{select_known_package_ids_sql, select_registrations_sql},
    verified_derived_source_event_id, verified_source_event_id,
};

use wamn::jetstream::consumer::{self, ConsumerConfig, Message};
use wamn::jetstream::types::Header;
use wamn::postgres::client;
use wamn::postgres::types::{PgError, SqlValue};
use wamn::router_delivery::delivery::{
    self, DeliveryError, DeliveryOutcome, DeliveryRequest, ParentCausation, Source,
};

struct Config {
    stream: String,
    org: String,
    project: String,
    env: String,
    tenant: String,
    batch: u32,
    fetch_ms: u64,
    sweep_ms: u64,
    max_sweeps: u64,
    max_depth: u32,
    ack_wait_ms: u64,
    nack_delay_ms: u64,
    max_deliver: u32,
    report_path: Option<String>,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn required(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("missing required env {name}"))
}

impl Config {
    fn from_env() -> Result<Self, String> {
        let max_deliver = env_or("WAMN_MAT_MAX_DELIVER", "5")
            .parse::<u32>()
            .map_err(|error| format!("WAMN_MAT_MAX_DELIVER: {error}"))?;
        if max_deliver == 0 {
            return Err("WAMN_MAT_MAX_DELIVER must be non-zero".into());
        }
        Ok(Self {
            stream: required("WAMN_MAT_STREAM")?,
            org: required("WAMN_MAT_ORG")?,
            project: required("WAMN_MAT_PROJECT")?,
            env: required("WAMN_MAT_ENV")?,
            tenant: required("WAMN_MAT_TENANT")?,
            batch: env_or("WAMN_MAT_BATCH", "64")
                .parse()
                .map_err(|error| format!("WAMN_MAT_BATCH: {error}"))?,
            fetch_ms: env_or("WAMN_MAT_FETCH_MS", "5000")
                .parse()
                .map_err(|error| format!("WAMN_MAT_FETCH_MS: {error}"))?,
            sweep_ms: env_or("WAMN_MAT_SWEEP_MS", "10000")
                .parse()
                .map_err(|error| format!("WAMN_MAT_SWEEP_MS: {error}"))?,
            max_sweeps: env_or("WAMN_MAT_MAX_SWEEPS", "0")
                .parse()
                .map_err(|error| format!("WAMN_MAT_MAX_SWEEPS: {error}"))?,
            max_depth: env_or("WAMN_MAT_MAX_DEPTH", &MAX_CAUSATION_DEPTH.to_string())
                .parse()
                .map_err(|error| format!("WAMN_MAT_MAX_DEPTH: {error}"))?,
            ack_wait_ms: env_or("WAMN_MAT_ACK_WAIT_MS", "30000")
                .parse()
                .map_err(|error| format!("WAMN_MAT_ACK_WAIT_MS: {error}"))?,
            nack_delay_ms: env_or("WAMN_MAT_NACK_DELAY_MS", "5000")
                .parse()
                .map_err(|error| format!("WAMN_MAT_NACK_DELAY_MS: {error}"))?,
            max_deliver,
            report_path: std::env::var("WAMN_MAT_REPORT_PATH").ok(),
        })
    }
}

#[derive(Default)]
struct Counters {
    sweeps: u64,
    deliveries: u64,
    batches: u64,
    acked: u64,
    skip_entity: u64,
    skip_package_by_registration: BTreeMap<String, u64>,
    skip_op: u64,
    skip_foreign_tenant: u64,
    skip_condition_false: u64,
    refuse_depth: u64,
    refuse_tenant_unscopable: u64,
    refuse_old_image_absent: u64,
    refuse_condition_error: u64,
    refuse_source_package_identity_unknown: u64,
    held_registrations: u64,
    poison: u64,
    retry: u64,
    dead_lettered: u64,
    dead_letter_retry: u64,
}

impl Counters {
    fn to_json(&self) -> String {
        serde_json::json!({
            "sweeps": self.sweeps,
            "deliveries": self.deliveries,
            "batches": self.batches,
            "acked": self.acked,
            "skip-entity": self.skip_entity,
            "skip-package-mismatch-by-registration": &self.skip_package_by_registration,
            "skip-op": self.skip_op,
            "skip-foreign-tenant": self.skip_foreign_tenant,
            "skip-condition-false": self.skip_condition_false,
            "refuse-depth": self.refuse_depth,
            "refuse-tenant-unscopable": self.refuse_tenant_unscopable,
            "refuse-old-image-absent": self.refuse_old_image_absent,
            "refuse-condition-error": self.refuse_condition_error,
            "refuse-source-package-identity-unknown": self.refuse_source_package_identity_unknown,
            "held-registrations": self.held_registrations,
            "poison": self.poison,
            "retry": self.retry,
            "dead-lettered": self.dead_lettered,
            "dead-letter-retry": self.dead_letter_retry,
        })
        .to_string()
    }
}

fn pg_name(error: &PgError) -> String {
    match error {
        PgError::SerializationFailure => "serialization-failure".into(),
        PgError::ConnectionUnavailable => "connection-unavailable".into(),
        PgError::StatementTimeout => "statement-timeout".into(),
        PgError::RowLimitExceeded(limit) => format!("row-limit-exceeded({limit})"),
        PgError::UniqueViolation(constraint) => format!("unique-violation({constraint})"),
        PgError::ForeignKeyViolation(constraint) => {
            format!("foreign-key-violation({constraint})")
        }
        PgError::CheckViolation(constraint) => format!("check-violation({constraint})"),
        PgError::PermissionDenied => "permission-denied".into(),
        PgError::QueryError((state, message)) => format!("query-error({state}: {message})"),
    }
}

struct Serving {
    registration: EventRegistration,
    condition: Option<wamn_materializer::CompiledCondition>,
}

struct LoadedServings {
    servings: Vec<Serving>,
    known_packages: BTreeSet<String>,
}

fn durable_name(tenant: &str, package_id: &str, registration_id: &str) -> String {
    let sanitize = |raw: &str| -> String {
        raw.chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect()
    };
    format!(
        "mat_{}_{}_{}",
        sanitize(tenant),
        sanitize(package_id),
        sanitize(registration_id)
    )
}

fn nats_message_ids(headers: &[Header]) -> Vec<&str> {
    headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("Nats-Msg-Id"))
        .map(|header| header.value.as_str())
        .collect()
}

fn load_servings(counters: &mut Counters) -> Result<LoadedServings, String> {
    let package_rows = client::query(&select_known_package_ids_sql(), &[])
        .map_err(|error| pg_name(&error))?
        .rows;
    let mut known_packages = BTreeSet::new();
    for row in package_rows {
        let Some(SqlValue::Text(package_id)) = row.first() else {
            return Err("package identity row shape".into());
        };
        known_packages.insert(package_id.clone());
    }
    let rows = client::query(&select_registrations_sql(), &[])
        .map_err(|error| pg_name(&error))?
        .rows;
    let mut servings = Vec::with_capacity(rows.len());
    for row in rows {
        let (
            Some(SqlValue::Text(registration_id)),
            Some(SqlValue::Text(package_id)),
            Some(document),
        ) = (row.first(), row.get(1), row.get(2))
        else {
            return Err("registration row shape".into());
        };
        let document = match document {
            SqlValue::Text(document) | SqlValue::Json(document) => document,
            other => return Err(format!("registration document shape: {other:?}")),
        };
        let registration = match EventRegistration::from_json(document) {
            Ok(registration) => registration,
            Err(error) => {
                counters.held_registrations += 1;
                eprintln!(
                    "wamn::materializer HELD registration {registration_id}: invalid document ({error})"
                );
                continue;
            }
        };
        if registration.registration_id != *registration_id
            || registration.package_id != *package_id
        {
            counters.held_registrations += 1;
            eprintln!(
                "wamn::materializer HELD registration {registration_id}: trusted identity columns disagree with its document"
            );
            continue;
        }
        let condition = match serviceable(&registration) {
            Ok(condition) => condition,
            Err(DecideError::UnserviceableCondition(reason)) => {
                counters.held_registrations += 1;
                eprintln!(
                    "wamn::materializer HELD registration {registration_id}: condition is not serviceable ({reason:?})"
                );
                continue;
            }
        };
        servings.push(Serving {
            registration,
            condition,
        });
    }
    Ok(LoadedServings {
        servings,
        known_packages,
    })
}

struct PreparedMessage {
    message: Message,
    payload: Value,
    stream_seq: u64,
    source_event_id: String,
    parent_causation: Option<wamn_event_wire::Causation>,
}

enum Preparation {
    Deliver {
        payload: Value,
        stream_seq: u64,
        source_event_id: String,
        parent_causation: Option<wamn_event_wire::Causation>,
    },
    Ack,
    Nack,
    DeadLetter(&'static str),
}

#[derive(Debug, PartialEq)]
enum PreparationGate<'a> {
    Ready(&'a VerifiedSourceEventId),
    MissingSourceId,
    SourcePackageIdentityRefusal(&'a RefuseReason),
}

fn preparation_gate<'a>(
    source_event_id: Option<&'a VerifiedSourceEventId>,
    verdict: &'a Verdict,
) -> PreparationGate<'a> {
    match verdict {
        Verdict::Refuse(reason @ RefuseReason::SourcePackageIdentityUnknown { .. }) => {
            PreparationGate::SourcePackageIdentityRefusal(reason)
        }
        _ => source_event_id.map_or(PreparationGate::MissingSourceId, PreparationGate::Ready),
    }
}

fn record_skip(counters: &mut Counters, serving: &Serving, reason: SkipReason) {
    match reason {
        SkipReason::SourcePackageMismatch => {
            let identity = serving.registration.qualified_id();
            *counters
                .skip_package_by_registration
                .entry(identity)
                .or_default() += 1;
        }
        SkipReason::EntityMismatch => counters.skip_entity += 1,
        SkipReason::OpMismatch => counters.skip_op += 1,
        SkipReason::ForeignTenant => counters.skip_foreign_tenant += 1,
        SkipReason::ConditionFalse => counters.skip_condition_false += 1,
    }
}

fn record_refusal(counters: &mut Counters, reason: &RefuseReason) -> &'static str {
    match reason {
        RefuseReason::SourcePackageIdentityUnknown { .. } => {
            counters.refuse_source_package_identity_unknown += 1;
            "source-package-identity-unknown"
        }
        RefuseReason::DepthExceeded { .. } => {
            counters.refuse_depth += 1;
            "registration-depth-exceeded"
        }
        RefuseReason::TenantUnscopable => {
            counters.refuse_tenant_unscopable += 1;
            "registration-tenant-unscopable"
        }
        RefuseReason::OldImageAbsent => {
            counters.refuse_old_image_absent += 1;
            "registration-old-image-absent"
        }
        RefuseReason::ConditionError(_) => {
            counters.refuse_condition_error += 1;
            "registration-condition-error"
        }
    }
}

fn refuse_message(
    serving: &Serving,
    message: &Message,
    stream_seq: u64,
    counters: &mut Counters,
    reason: &RefuseReason,
) -> Preparation {
    let literal = record_refusal(counters, reason);
    eprintln!(
        "wamn::materializer REFUSED registration={} subject={} stream_seq={} reason={reason:?}",
        serving.registration.qualified_id(),
        message.subject(),
        stream_seq
    );
    Preparation::DeadLetter(literal)
}

#[derive(Debug, PartialEq)]
enum SourceEvent {
    Cdc(Envelope),
    Derived(DerivedEvent),
}

fn decode_source_event(body: &[u8]) -> Result<SourceEvent, &'static str> {
    let value = serde_json::from_slice::<Value>(body).map_err(|_| "poison-invalid-envelope")?;
    if value.get("format-version").is_some() {
        DerivedEvent::from_slice(body)
            .map(SourceEvent::Derived)
            .map_err(|_| "poison-invalid-derived-event")
    } else {
        serde_json::from_value::<Envelope>(value)
            .map(SourceEvent::Cdc)
            .map_err(|_| "poison-invalid-envelope")
    }
}

fn prepare_message(
    config: &Config,
    serving: &Serving,
    known_packages: &BTreeSet<String>,
    message: &Message,
    counters: &mut Counters,
) -> Preparation {
    let metadata = message.metadata();
    if metadata.stream_seq == 0 {
        counters.retry += 1;
        eprintln!("wamn::materializer metadata parse failure — nack for redelivery");
        return Preparation::Nack;
    }
    let body = message.body();
    let source = match decode_source_event(&body) {
        Ok(source) => source,
        Err(reason) => {
            counters.poison += 1;
            eprintln!(
                "wamn::materializer REFUSED poison stream_seq={}: event record parse failed",
                metadata.stream_seq,
            );
            return Preparation::DeadLetter(reason);
        }
    };
    let headers = message.headers();
    let message_ids = nats_message_ids(&headers);
    let (source_event_id, verdict) = match &source {
        SourceEvent::Cdc(envelope) => (
            verified_source_event_id(&config.project, &config.env, envelope, &message_ids),
            decide(
                &serving.registration,
                serving.condition.as_ref(),
                envelope,
                known_packages,
                &config.tenant,
                config.max_depth,
            ),
        ),
        SourceEvent::Derived(event) => (
            verified_derived_source_event_id(
                &config.tenant,
                &config.project,
                &config.env,
                event,
                &message_ids,
            ),
            decide_derived(
                &serving.registration,
                serving.condition.as_ref(),
                event,
                known_packages,
                &config.tenant,
                config.max_depth,
            ),
        ),
    };
    let source_event_id = match preparation_gate(source_event_id.as_ref(), &verdict) {
        PreparationGate::Ready(source_event_id) => source_event_id.as_str().to_string(),
        PreparationGate::SourcePackageIdentityRefusal(reason) => {
            return refuse_message(serving, message, metadata.stream_seq, counters, reason);
        }
        PreparationGate::MissingSourceId => {
            counters.poison += 1;
            eprintln!(
                "wamn::materializer REFUSED poison stream_seq={}: Nats-Msg-Id is missing, duplicated, or inconsistent",
                metadata.stream_seq
            );
            return Preparation::DeadLetter("poison-source-id");
        }
    };
    let parent_causation = match &source {
        SourceEvent::Cdc(envelope) => envelope.causation.clone(),
        SourceEvent::Derived(event) => Some(event.causation.clone()),
    };
    match verdict {
        Verdict::Deliver(payload) => Preparation::Deliver {
            payload,
            stream_seq: metadata.stream_seq,
            source_event_id,
            parent_causation,
        },
        Verdict::Skip(reason) => {
            record_skip(counters, serving, reason);
            Preparation::Ack
        }
        Verdict::Refuse(reason) => {
            refuse_message(serving, message, metadata.stream_seq, counters, &reason)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryDisposition {
    Ack,
    Retry,
    DeadLetter(&'static str),
}

fn delivery_disposition(result: &Result<DeliveryOutcome, DeliveryError>) -> DeliveryDisposition {
    match result {
        Ok(DeliveryOutcome::Respond(_) | DeliveryOutcome::Emit(_) | DeliveryOutcome::Discard) => {
            DeliveryDisposition::Ack
        }
        Ok(DeliveryOutcome::Failed(failure)) => {
            DeliveryDisposition::DeadLetter(match failure.kind {
                delivery::FailureKind::Terminal => "router-terminal",
                delivery::FailureKind::RetryExhausted => "router-retry-exhausted",
                delivery::FailureKind::InvalidInput => "router-invalid-input",
                delivery::FailureKind::HopLimit => "router-hop-limit",
                delivery::FailureKind::UnreleasedCaller => "router-unreleased-caller",
                delivery::FailureKind::MissingDedupId => "router-missing-dedup-id",
                delivery::FailureKind::RespondWithoutCaller => "router-respond-without-caller",
                delivery::FailureKind::SecondVerdict => "router-second-verdict",
            })
        }
        Err(
            DeliveryError::SourceNotFound
            | DeliveryError::InvalidRequest
            | DeliveryError::InvalidPayload,
        ) => DeliveryDisposition::DeadLetter("router-deterministic-refusal"),
        Err(DeliveryError::PermissionDenied(_)) => {
            DeliveryDisposition::DeadLetter("router-permission-denied")
        }
        Ok(DeliveryOutcome::Cancelled) | Err(DeliveryError::ExecutionFailed) => {
            DeliveryDisposition::Retry
        }
    }
}

fn execution_budget_exhausted_before_delivery(delivered: u64, max_deliver: u32) -> bool {
    delivered > u64::from(max_deliver)
}

fn execution_budget_exhausted_after_failure(delivered: u64, max_deliver: u32) -> bool {
    delivered >= u64::from(max_deliver)
}

fn deliver(
    registration_identity: &str,
    delivery_id: String,
    payload: &Value,
    parent_causation: Option<&wamn_event_wire::Causation>,
) -> Result<DeliveryOutcome, DeliveryError> {
    let payload = serde_json::to_string(payload).map_err(|_| DeliveryError::InvalidPayload)?;
    delivery::deliver(DeliveryRequest {
        source: Source::Registration(registration_identity.to_string()),
        delivery_id,
        payload,
        caller: None,
        trace: None,
        parent_causation: parent_causation.map(|parent| ParentCausation {
            root: parent.root.clone(),
            depth: parent.depth,
        }),
    })
}

fn event_delivery_id(registration_id: &str, stream_seq: u64, source_event_id: &str) -> String {
    format!("{registration_id}:event:{stream_seq}:{source_event_id}")
}

fn batch_delivery_id(registration_id: &str, messages: &[PreparedMessage]) -> String {
    let mut delivery_id = format!("{registration_id}:batch");
    for prepared in messages {
        write!(delivery_id, ":{}", prepared.stream_seq).expect("write to String is infallible");
    }
    delivery_id
}

fn batch_payload(messages: &[PreparedMessage]) -> Value {
    ordered_batch_payload(messages.iter().map(|prepared| &prepared.payload))
}

fn ordered_batch_payload<'a>(payloads: impl IntoIterator<Item = &'a Value>) -> Value {
    Value::Array(payloads.into_iter().cloned().collect())
}

fn batch_parent_causation(messages: &[PreparedMessage]) -> Option<&wamn_event_wire::Causation> {
    common_root_parent_causation(
        messages
            .iter()
            .map(|message| message.parent_causation.as_ref()),
    )
}

fn common_root_parent_causation<'a>(
    parents: impl IntoIterator<Item = Option<&'a wamn_event_wire::Causation>>,
) -> Option<&'a wamn_event_wire::Causation> {
    let mut parents = parents.into_iter();
    let first = parents.next()??;
    let mut deepest = first;
    for candidate in parents {
        let candidate = candidate?;
        if candidate.root != first.root {
            return None;
        }
        if candidate.depth > deepest.depth {
            deepest = candidate;
        }
    }
    Some(deepest)
}

fn acknowledge(message: &Message, counters: &mut Counters) {
    match message.ack() {
        Ok(()) => counters.acked += 1,
        Err(error) => {
            counters.retry += 1;
            eprintln!("wamn::materializer ack failed ({error:?}); server redelivery remains armed");
        }
    }
}

fn nack(message: &Message, config: &Config, counters: &mut Counters) {
    counters.retry += 1;
    if let Err(error) = message.nack(config.nack_delay_ms) {
        eprintln!("wamn::materializer nack failed ({error:?}); ack-wait redelivery remains armed");
    }
}

fn term(message: &Message) {
    if let Err(error) = message.term() {
        eprintln!("wamn::materializer term failed ({error:?}); poison may redeliver");
    }
}

fn dead_letter(message: &Message, reason: &'static str, config: &Config, counters: &mut Counters) {
    match message.dead_letter(reason) {
        Ok(()) => {
            counters.dead_lettered += 1;
            term(message);
        }
        Err(error) => {
            counters.dead_letter_retry += 1;
            eprintln!(
                "wamn::materializer dead-letter publish failed reason={reason} ({error:?}); nack for retry"
            );
            nack(message, config, counters);
        }
    }
}

fn settle_delivery(
    result: &Result<DeliveryOutcome, DeliveryError>,
    messages: &[&Message],
    config: &Config,
    counters: &mut Counters,
) {
    match delivery_disposition(result) {
        DeliveryDisposition::Ack => {
            counters.deliveries += 1;
            for message in messages {
                acknowledge(message, counters);
            }
        }
        DeliveryDisposition::Retry => {
            eprintln!("wamn::materializer router delivery did not settle: {result:?}");
            for message in messages {
                if execution_budget_exhausted_after_failure(
                    message.metadata().delivered,
                    config.max_deliver,
                ) {
                    dead_letter(message, "redelivery-budget-exhausted", config, counters);
                } else {
                    nack(message, config, counters);
                }
            }
        }
        DeliveryDisposition::DeadLetter(reason) => {
            for message in messages {
                dead_letter(message, reason, config, counters);
            }
        }
    }
}

fn serve(
    config: &Config,
    serving: &Serving,
    known_packages: &BTreeSet<String>,
    counters: &mut Counters,
) {
    let registration = &serving.registration;
    let registration_identity = registration.qualified_id();
    let filter = format!(
        "evt.{}.{}.{}.{}.>",
        config.org,
        config.project,
        config.env,
        wamn_event_wire::subject_token(registration.entity.as_str())
    );
    let consumer = match consumer::bind_registration(
        &registration.package_id,
        &registration.registration_id,
        &ConsumerConfig {
            stream_name: config.stream.clone(),
            durable: durable_name(
                &config.tenant,
                &registration.package_id,
                &registration.registration_id,
            ),
            filter_subject: filter,
            ack_wait_ms: config.ack_wait_ms,
            // Router execution is bounded below. Transport redelivery remains
            // armed so a failed DLQ publication can retry without re-executing.
            max_deliver: 0,
        },
    ) {
        Ok(consumer) => consumer,
        Err(error) => {
            eprintln!(
                "wamn::materializer bind failed for registration {}: {error:?}",
                registration.registration_id
            );
            return;
        }
    };
    let messages = match consumer.fetch(config.batch, config.fetch_ms) {
        Ok(messages) => messages,
        Err(error) => {
            eprintln!(
                "wamn::materializer fetch failed for registration {}: {error:?}",
                registration.registration_id
            );
            return;
        }
    };
    let mut prepared = Vec::with_capacity(messages.len());
    for message in messages {
        match prepare_message(config, serving, known_packages, &message, counters) {
            Preparation::Deliver {
                payload,
                stream_seq,
                source_event_id,
                parent_causation,
            } => prepared.push(PreparedMessage {
                message,
                payload,
                stream_seq,
                source_event_id,
                parent_causation,
            }),
            Preparation::Ack => acknowledge(&message, counters),
            Preparation::Nack => nack(&message, config, counters),
            Preparation::DeadLetter(reason) => dead_letter(&message, reason, config, counters),
        }
    }
    let mut within_budget = Vec::with_capacity(prepared.len());
    for prepared in prepared {
        if execution_budget_exhausted_before_delivery(
            prepared.message.metadata().delivered,
            config.max_deliver,
        ) {
            dead_letter(
                &prepared.message,
                "redelivery-budget-exhausted",
                config,
                counters,
            );
        } else {
            within_budget.push(prepared);
        }
    }
    let prepared = within_budget;
    match registration.input {
        RegistrationInput::Event => {
            for prepared in prepared {
                let delivery_id = event_delivery_id(
                    &registration_identity,
                    prepared.stream_seq,
                    &prepared.source_event_id,
                );
                let result = deliver(
                    &registration_identity,
                    delivery_id,
                    &prepared.payload,
                    prepared.parent_causation.as_ref(),
                );
                settle_delivery(&result, &[&prepared.message], config, counters);
            }
        }
        RegistrationInput::Batch if !prepared.is_empty() => {
            counters.batches += 1;
            let delivery_id = batch_delivery_id(&registration_identity, &prepared);
            let payload = batch_payload(&prepared);
            let result = deliver(
                &registration_identity,
                delivery_id,
                &payload,
                batch_parent_causation(&prepared),
            );
            let messages = prepared
                .iter()
                .map(|prepared| &prepared.message)
                .collect::<Vec<_>>();
            settle_delivery(&result, &messages, config, counters);
        }
        RegistrationInput::Batch => {}
    }
}

fn write_report(config: &Config, counters: &Counters) {
    if let Some(path) = &config.report_path
        && let Err(error) = std::fs::write(path, counters.to_json())
    {
        eprintln!("wamn::materializer report write failed ({path}): {error}");
    }
}

fn main() {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("wamn::materializer config error: {error}");
            std::process::exit(1);
        }
    };
    println!(
        "wamn::materializer up: stream={} filter=evt.{}.{}.{}.*.* tenant={} batch={} fetch_ms={} max_deliver={} max_sweeps={}",
        config.stream,
        config.org,
        config.project,
        config.env,
        config.tenant,
        config.batch,
        config.fetch_ms,
        config.max_deliver,
        config.max_sweeps
    );
    let mut counters = Counters::default();
    loop {
        counters.sweeps += 1;
        match load_servings(&mut counters) {
            Ok(loaded) if loaded.servings.is_empty() => {
                std::thread::sleep(std::time::Duration::from_millis(config.sweep_ms));
            }
            Ok(loaded) => {
                for serving in &loaded.servings {
                    serve(&config, serving, &loaded.known_packages, &mut counters);
                }
            }
            Err(error) => {
                eprintln!(
                    "wamn::materializer sweep failed ({error}); retrying after {}ms",
                    config.sweep_ms
                );
                std::thread::sleep(std::time::Duration::from_millis(config.sweep_ms));
            }
        }
        write_report(&config, &counters);
        if config.max_sweeps > 0 && counters.sweeps >= config.max_sweeps {
            println!(
                "wamn::materializer done after {} sweeps: {}",
                counters.sweeps,
                counters.to_json()
            );
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn::router_delivery::delivery::{DeliveryFailure, FailureKind};
    use wamn_event_wire::{Causation, Op};

    fn serving() -> Serving {
        Serving {
            registration: EventRegistration {
                schema_version: wamn_event_reg::SCHEMA_VERSION.into(),
                registration_id: "receive-receipt".into(),
                package_id: "client_acme_receiving".into(),
                source_package_id: "receiving".into(),
                flow_id: "receive".into(),
                entity: "receipts".into(),
                ops: vec![Op::Insert],
                input: RegistrationInput::Event,
                condition: None,
            },
            condition: None,
        }
    }

    fn envelope() -> Envelope {
        serde_json::from_value(serde_json::json!({
            "op": "insert",
            "new": {"tenant_id": "tenant"},
            "package_id": "receiving",
            "entity": "receipts",
            "table": "receipts",
            "lsn": 42,
            "txid": 7,
            "commit_ts": "2026-08-15T12:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn source_event_id_requires_one_exact_header() {
        let matching = vec![Header {
            name: "nats-msg-id".into(),
            value: "app_dev:42".into(),
        }];
        let ids = nats_message_ids(&matching);
        assert_eq!(
            verified_source_event_id("app", "dev", &envelope(), &ids)
                .unwrap()
                .as_str(),
            "app_dev:42"
        );
        assert!(
            verified_source_event_id("app", "dev", &envelope(), &[]).is_none(),
            "a missing source id must remain poison"
        );
    }

    #[test]
    fn cdc_decode_refuses_either_half_of_package_entity_identity() {
        let complete = serde_json::json!({
            "op": "insert",
            "new": {"tenant_id": "tenant"},
            "package_id": "receiving",
            "entity": "receipts",
            "table": "receipts",
            "lsn": 42,
            "txid": 7,
            "commit_ts": "2026-08-15T12:00:00Z"
        });
        for missing in ["package_id", "entity"] {
            let mut half = complete.clone();
            half.as_object_mut().unwrap().remove(missing);
            assert_eq!(
                decode_source_event(&serde_json::to_vec(&half).unwrap()),
                Err("poison-invalid-envelope"),
                "missing {missing} must refuse at the wire boundary"
            );
        }
    }

    #[test]
    fn derived_origin_decodes_separately_from_the_cdc_envelope() {
        let event = DerivedEvent::new(
            "tenant",
            "app",
            "dev",
            "receiving",
            "receipts",
            Op::Delete,
            serde_json::json!(["arbitrary", {"id": 7}]),
            "author:receipt:7",
            Causation {
                run: "delivery-7".into(),
                root: "delivery-1".into(),
                depth: 2,
            },
        );
        let bytes = serde_json::to_vec(&event).unwrap();
        assert_eq!(
            decode_source_event(&bytes),
            Ok(SourceEvent::Derived(event.clone()))
        );
        assert!(serde_json::from_slice::<Envelope>(&bytes).is_err());

        let mut future = serde_json::to_value(event).unwrap();
        future["format-version"] = "0.2".into();
        assert_eq!(
            decode_source_event(&serde_json::to_vec(&future).unwrap()),
            Err("poison-invalid-derived-event")
        );
    }

    #[test]
    fn unknown_source_package_refusal_has_a_typed_literal_and_counter() {
        let mut counters = Counters::default();
        assert_eq!(
            record_refusal(
                &mut counters,
                &RefuseReason::SourcePackageIdentityUnknown {
                    source_package_id: "uninstalled".into(),
                },
            ),
            "source-package-identity-unknown"
        );
        assert_eq!(counters.refuse_source_package_identity_unknown, 1);
    }

    #[test]
    fn known_package_mismatch_is_counted_per_registration() {
        let serving = serving();
        let known_packages = BTreeSet::from(["receiving".to_owned(), "overlay".to_owned()]);
        let mut event = envelope();
        event.package_id = "overlay".into();
        let Verdict::Skip(reason) = decide(
            &serving.registration,
            None,
            &event,
            &known_packages,
            "tenant",
            MAX_CAUSATION_DEPTH,
        ) else {
            panic!("a known foreign package must be normal filtration");
        };
        assert_eq!(reason, SkipReason::SourcePackageMismatch);
        let mut counters = Counters::default();
        record_skip(&mut counters, &serving, reason);
        assert_eq!(
            counters
                .skip_package_by_registration
                .get("client_acme_receiving::receive-receipt"),
            Some(&1)
        );
        assert_eq!(counters.skip_entity, 0);
    }

    #[test]
    fn router_completion_matrix_separates_deterministic_poison_from_retry() {
        let failed = Ok(DeliveryOutcome::Failed(DeliveryFailure {
            kind: FailureKind::InvalidInput,
            code: None,
            message: "bad event".into(),
        }));
        assert_eq!(
            delivery_disposition(&failed),
            DeliveryDisposition::DeadLetter("router-invalid-input")
        );
        for result in [
            Ok(DeliveryOutcome::Cancelled),
            Err(DeliveryError::ExecutionFailed),
        ] {
            assert_eq!(delivery_disposition(&result), DeliveryDisposition::Retry);
        }
        for result in [
            Err(DeliveryError::SourceNotFound),
            Err(DeliveryError::InvalidRequest),
            Err(DeliveryError::InvalidPayload),
        ] {
            assert_eq!(
                delivery_disposition(&result),
                DeliveryDisposition::DeadLetter("router-deterministic-refusal")
            );
        }
        let permission_denied = Err(DeliveryError::PermissionDenied(
            delivery::PermissionDenial {
                operation: "wamn_receiving@1.0.0::receipt.get".into(),
            },
        ));
        assert_eq!(
            delivery_disposition(&permission_denied),
            DeliveryDisposition::DeadLetter("router-permission-denied")
        );
        for result in [
            Ok(DeliveryOutcome::Discard),
            Ok(DeliveryOutcome::Respond("{}".into())),
            Ok(DeliveryOutcome::Emit(delivery::Emission {
                event: "{}".into(),
                dedup_id: "author-key".into(),
            })),
        ] {
            assert_eq!(delivery_disposition(&result), DeliveryDisposition::Ack);
        }
    }

    #[test]
    fn batch_payload_preserves_fetch_order() {
        let inputs = [
            serde_json::json!({"event": "insert", "new": {"id": "first"}}),
            serde_json::json!({"event": "update", "new": {"id": "second"}}),
        ];
        assert_eq!(
            ordered_batch_payload(&inputs),
            serde_json::json!([inputs[0], inputs[1]])
        );
    }

    #[test]
    fn a_batch_propagates_the_deepest_parent_only_with_one_truthful_root() {
        let parents = [
            Causation {
                run: "run-a".into(),
                root: "root".into(),
                depth: 2,
            },
            Causation {
                run: "run-b".into(),
                root: "root".into(),
                depth: 5,
            },
            Causation {
                run: "run-c".into(),
                root: "root".into(),
                depth: 5,
            },
        ];
        assert_eq!(
            common_root_parent_causation(parents.iter().map(Some))
                .map(|parent| parent.run.as_str()),
            Some("run-b")
        );
    }

    #[test]
    fn a_batch_with_mixed_roots_mints_a_new_root() {
        let parents = [
            Causation {
                run: "run-a".into(),
                root: "root-a".into(),
                depth: 2,
            },
            Causation {
                run: "run-b".into(),
                root: "root-b".into(),
                depth: 5,
            },
        ];
        assert_eq!(common_root_parent_causation(parents.iter().map(Some)), None);
    }

    #[test]
    fn a_batch_with_any_missing_parent_mints_a_new_root() {
        let parent = Causation {
            run: "run-a".into(),
            root: "root".into(),
            depth: 2,
        };
        assert_eq!(common_root_parent_causation([Some(&parent), None]), None);
        assert_eq!(common_root_parent_causation([None]), None);
    }

    #[test]
    fn post_budget_redelivery_is_dlq_only() {
        assert!(!execution_budget_exhausted_before_delivery(5, 5));
        assert!(execution_budget_exhausted_after_failure(5, 5));
        assert!(execution_budget_exhausted_before_delivery(6, 5));
    }
}
