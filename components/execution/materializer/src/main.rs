//! Durable JetStream registration delivery through the shared router bridge.

wit_bindgen::generate!({
    world: "materializer",
    path: "wit",
    generate_all,
});

use std::fmt::Write as _;

use serde_json::Value;
use wamn_event_reg::{EventRegistration, RegistrationInput};
use wamn_event_wire::Envelope;
use wamn_materializer::{
    DecideError, MAX_CAUSATION_DEPTH, RefuseReason, SkipReason, Verdict, decide, serviceable,
    sql::select_registrations_sql, verified_source_event_id,
};

use wamn::jetstream::consumer::{self, ConsumerConfig, Message};
use wamn::jetstream::types::Header;
use wamn::postgres::client;
use wamn::postgres::types::{PgError, SqlValue};
use wamn::router_delivery::delivery::{
    self, DeliveryError, DeliveryOutcome, DeliveryRequest, Source,
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
    skip_op: u64,
    skip_foreign_tenant: u64,
    skip_condition_false: u64,
    refuse_depth: u64,
    refuse_tenant_unscopable: u64,
    refuse_old_image_absent: u64,
    refuse_condition_error: u64,
    held_registrations: u64,
    poison: u64,
    retry: u64,
    emit_blocked: u64,
}

impl Counters {
    fn to_json(&self) -> String {
        serde_json::json!({
            "sweeps": self.sweeps,
            "deliveries": self.deliveries,
            "batches": self.batches,
            "acked": self.acked,
            "skip-entity": self.skip_entity,
            "skip-op": self.skip_op,
            "skip-foreign-tenant": self.skip_foreign_tenant,
            "skip-condition-false": self.skip_condition_false,
            "refuse-depth": self.refuse_depth,
            "refuse-tenant-unscopable": self.refuse_tenant_unscopable,
            "refuse-old-image-absent": self.refuse_old_image_absent,
            "refuse-condition-error": self.refuse_condition_error,
            "held-registrations": self.held_registrations,
            "poison": self.poison,
            "retry": self.retry,
            "emit-blocked": self.emit_blocked,
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

fn durable_name(tenant: &str, catalog_id: &str, registration_id: &str) -> String {
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
        sanitize(catalog_id),
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

fn load_servings(counters: &mut Counters) -> Result<Vec<Serving>, String> {
    let rows = client::query(&select_registrations_sql(), &[])
        .map_err(|error| pg_name(&error))?
        .rows;
    let mut servings = Vec::with_capacity(rows.len());
    for row in rows {
        let (
            Some(SqlValue::Text(registration_id)),
            Some(SqlValue::Text(catalog_id)),
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
            || registration.catalog_id != *catalog_id
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
    Ok(servings)
}

struct PreparedMessage {
    message: Message,
    payload: Value,
    stream_seq: u64,
    source_event_id: String,
}

enum Preparation {
    Deliver {
        payload: Value,
        stream_seq: u64,
        source_event_id: String,
    },
    Ack,
    Nack,
    Term,
}

fn prepare_message(
    config: &Config,
    serving: &Serving,
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
    let envelope: Envelope = match serde_json::from_slice(&body) {
        Ok(envelope) => envelope,
        Err(error) => {
            counters.poison += 1;
            eprintln!(
                "wamn::materializer REFUSED poison stream_seq={}: envelope parse failed ({error})",
                metadata.stream_seq
            );
            return Preparation::Term;
        }
    };
    let headers = message.headers();
    let message_ids = nats_message_ids(&headers);
    let Some(source_event_id) =
        verified_source_event_id(&config.project, &config.env, &envelope, &message_ids)
    else {
        counters.poison += 1;
        eprintln!(
            "wamn::materializer REFUSED poison stream_seq={}: Nats-Msg-Id is missing, duplicated, or inconsistent",
            metadata.stream_seq
        );
        return Preparation::Term;
    };
    match decide(
        &serving.registration,
        serving.condition.as_ref(),
        &envelope,
        &config.tenant,
        config.max_depth,
    ) {
        Verdict::Deliver(payload) => Preparation::Deliver {
            payload,
            stream_seq: metadata.stream_seq,
            source_event_id: source_event_id.as_str().to_string(),
        },
        Verdict::Skip(reason) => {
            match reason {
                SkipReason::EntityMismatch => counters.skip_entity += 1,
                SkipReason::OpMismatch => counters.skip_op += 1,
                SkipReason::ForeignTenant => counters.skip_foreign_tenant += 1,
                SkipReason::ConditionFalse => counters.skip_condition_false += 1,
            }
            Preparation::Ack
        }
        Verdict::Refuse(reason) => {
            match reason {
                RefuseReason::DepthExceeded { .. } => counters.refuse_depth += 1,
                RefuseReason::TenantUnscopable => counters.refuse_tenant_unscopable += 1,
                RefuseReason::OldImageAbsent => counters.refuse_old_image_absent += 1,
                RefuseReason::ConditionError(_) => counters.refuse_condition_error += 1,
            }
            eprintln!(
                "wamn::materializer REFUSED registration={} stream_seq={} reason={reason:?}",
                serving.registration.registration_id, metadata.stream_seq
            );
            Preparation::Ack
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryDisposition {
    Ack,
    Retry,
}

fn delivery_disposition(result: &Result<DeliveryOutcome, DeliveryError>) -> DeliveryDisposition {
    match result {
        Ok(DeliveryOutcome::Respond(_) | DeliveryOutcome::Discard) => DeliveryDisposition::Ack,
        Ok(DeliveryOutcome::Emit(_) | DeliveryOutcome::Failed(_) | DeliveryOutcome::Cancelled)
        | Err(_) => DeliveryDisposition::Retry,
    }
}

fn deliver(
    registration_id: &str,
    delivery_id: String,
    payload: &Value,
) -> Result<DeliveryOutcome, DeliveryError> {
    let payload = serde_json::to_string(payload).map_err(|_| DeliveryError::InvalidPayload)?;
    delivery::deliver(&DeliveryRequest {
        source: Source::Registration(registration_id.to_string()),
        delivery_id,
        payload,
        caller: None,
        trace: None,
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
            if matches!(result, Ok(DeliveryOutcome::Emit(_))) {
                counters.emit_blocked += 1;
                eprintln!(
                    "wamn::materializer emit has no explicit subject selector; nack without inferring entity/op from guest JSON"
                );
            } else {
                eprintln!("wamn::materializer router delivery did not settle: {result:?}");
            }
            for message in messages {
                nack(message, config, counters);
            }
        }
    }
}

fn serve(config: &Config, serving: &Serving, counters: &mut Counters) {
    let registration = &serving.registration;
    let filter = format!(
        "evt.{}.{}.{}.{}.>",
        config.org,
        config.project,
        config.env,
        wamn_event_wire::subject_token(registration.entity.as_str())
    );
    let consumer = match consumer::bind(&ConsumerConfig {
        stream_name: config.stream.clone(),
        durable: durable_name(
            &config.tenant,
            &registration.catalog_id,
            &registration.registration_id,
        ),
        filter_subject: filter,
        ack_wait_ms: config.ack_wait_ms,
        max_deliver: 0,
    }) {
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
        match prepare_message(config, serving, &message, counters) {
            Preparation::Deliver {
                payload,
                stream_seq,
                source_event_id,
            } => prepared.push(PreparedMessage {
                message,
                payload,
                stream_seq,
                source_event_id,
            }),
            Preparation::Ack => acknowledge(&message, counters),
            Preparation::Nack => nack(&message, config, counters),
            Preparation::Term => term(&message),
        }
    }
    match registration.input {
        RegistrationInput::Event => {
            for prepared in prepared {
                let delivery_id = event_delivery_id(
                    &registration.registration_id,
                    prepared.stream_seq,
                    &prepared.source_event_id,
                );
                let result = deliver(
                    &registration.registration_id,
                    delivery_id,
                    &prepared.payload,
                );
                settle_delivery(&result, &[&prepared.message], config, counters);
            }
        }
        RegistrationInput::Batch if !prepared.is_empty() => {
            counters.batches += 1;
            let delivery_id = batch_delivery_id(&registration.registration_id, &prepared);
            let payload = batch_payload(&prepared);
            let result = deliver(&registration.registration_id, delivery_id, &payload);
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
        "wamn::materializer up: stream={} filter=evt.{}.{}.{}.*.* tenant={} batch={} fetch_ms={} max_sweeps={}",
        config.stream,
        config.org,
        config.project,
        config.env,
        config.tenant,
        config.batch,
        config.fetch_ms,
        config.max_sweeps
    );
    let mut counters = Counters::default();
    loop {
        counters.sweeps += 1;
        match load_servings(&mut counters) {
            Ok(servings) if servings.is_empty() => {
                std::thread::sleep(std::time::Duration::from_millis(config.sweep_ms));
            }
            Ok(servings) => {
                for serving in &servings {
                    serve(&config, serving, &mut counters);
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

    fn envelope() -> Envelope {
        serde_json::from_value(serde_json::json!({
            "op": "insert",
            "new": {"tenant_id": "tenant"},
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
    fn router_completion_matrix_never_acks_failure_cancel_error_or_emit() {
        let failed = Ok(DeliveryOutcome::Failed(DeliveryFailure {
            kind: FailureKind::InvalidInput,
            code: None,
            message: "bad event".into(),
        }));
        for result in [
            failed,
            Ok(DeliveryOutcome::Cancelled),
            Ok(DeliveryOutcome::Emit(delivery::Emission {
                event: "{}".into(),
                dedup_id: "author-key".into(),
            })),
            Err(DeliveryError::ExecutionFailed),
        ] {
            assert_eq!(delivery_disposition(&result), DeliveryDisposition::Retry);
        }
        for result in [
            Ok(DeliveryOutcome::Discard),
            Ok(DeliveryOutcome::Respond("{}".into())),
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
}
