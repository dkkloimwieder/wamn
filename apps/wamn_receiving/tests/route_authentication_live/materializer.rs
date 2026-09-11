//! Receiving materializer tests and helpers.

use super::*;


pub(super) async fn connect_event_proof_client(
    url: &str,
    username_key: &str,
    password_file_key: &str,
) -> anyhow::Result<async_nats::Client> {
    let username = std::env::var(username_key)
        .with_context(|| format!("the event proof requires {username_key}"))?;
    let password_file = std::env::var_os(password_file_key)
        .with_context(|| format!("the event proof requires {password_file_key}"))?;
    anyhow::ensure!(
        !username.is_empty()
            && username.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
            }),
        "the event proof username must contain only ASCII letters, digits, underscore or hyphen"
    );
    let password = tokio::fs::read_to_string(password_file)
        .await
        .context("read the private event proof password file")?;
    anyhow::ensure!(!password.is_empty(), "the event proof password is empty");
    async_nats::ConnectOptions::new()
        .custom_inbox_prefix(format!("_INBOX_{username}"))
        .user_and_password(username, password)
        .connect(url)
        .await
        .context("connect to the disposable event plane with the scoped proof role")
}

#[tokio::test]
#[ignore = "requires the disposable Receiving journey after its production materializer settles"]
async fn production_materializer_consumes_the_causal_receipt_exactly_once() -> anyhow::Result<()> {
    let document = JourneyDocument::required()?;
    let MaterializerPhase {
        project_pg_url: project_url,
        nats_url,
        receipt_id,
    } = document.materializer.context(
        "the journey document carries no materializer phase: the route phase must \
         provision the project environment and the trigger must produce a receipt \
         before this test runs",
    )?;
    let (project, project_task) = connect(&project_url).await?;

    let registrations = project
        .query(
            "SELECT package_id, entity_id, registration::text FROM catalog.event_registrations \
             WHERE tenant_id = $1 ORDER BY package_id COLLATE \"C\", registration_id COLLATE \"C\"",
            &[&TENANT],
        )
        .await
        .context("read the installed event-registration set")?;
    anyhow::ensure!(
        registrations.len() == 1,
        "Receiving release installed {} registrations instead of one",
        registrations.len()
    );
    let registration = &registrations[0];
    let registration_document: Value = serde_json::from_str(&registration.get::<_, String>(2))
        .context("parse the installed event registration")?;
    anyhow::ensure!(
        registration.get::<_, String>(0) == OVERLAY_PACKAGE_ID
            && registration.get::<_, String>(1) == "receipt"
            && registration_document["registration-id"] == "quality.create_inspection"
            && registration_document["package-id"] == OVERLAY_PACKAGE_ID
            && registration_document["source-package-id"] == BASE_PACKAGE_ID
            && registration_document["entity"] == "receipt"
            && registration_document["ops"] == serde_json::json!(["insert"]),
        "installed event registration is not the exact Acme receipt binding: {registration_document}"
    );

    let jetstream = async_nats::jetstream::new(
        connect_event_proof_client(
            &nats_url,
            "WAMN_EVT_NATS_USERNAME",
            "WAMN_EVT_NATS_PASSWORD_FILE",
        ).await?,
    );
    let mut stream = jetstream
        .get_stream(MATERIALIZER_STREAM)
        .await
        .context("read the production reader's event stream")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let (receipt_sequence, receipt_causation, inspection_causation) = loop {
        let info = stream.info().await.context("read event-stream state")?;
        let mut receipt = None;
        let mut inspection = None;
        if info.state.messages > 0 {
            for sequence in info.state.first_sequence..=info.state.last_sequence {
                let message = stream
                    .get_raw_message(sequence)
                    .await
                    .with_context(|| format!("read stored event sequence {sequence}"))?;
                let Ok(envelope) =
                    serde_json::from_slice::<wamn_event_wire::Envelope>(&message.payload)
                else {
                    continue;
                };
                if envelope.op == wamn_event_wire::Op::Insert
                    && envelope.package_id == BASE_PACKAGE_ID
                    && envelope.entity == "receipt"
                    && envelope
                        .new
                        .as_ref()
                        .and_then(|row| row.get("id"))
                        .and_then(Value::as_str)
                        == Some(receipt_id.as_str())
                {
                    receipt = envelope.causation.map(|causation| (sequence, causation));
                } else if envelope.op == wamn_event_wire::Op::Insert
                    && envelope.package_id == OVERLAY_PACKAGE_ID
                    && envelope.entity == "quality_inspection"
                    && envelope
                        .new
                        .as_ref()
                        .and_then(|row| row.get("receipt_id"))
                        .and_then(Value::as_str)
                        == Some(receipt_id.as_str())
                {
                    inspection = envelope.causation;
                }
            }
        }
        if let (Some((sequence, receipt)), Some(inspection)) = (receipt, inspection) {
            break (sequence, receipt, inspection);
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "causal receipt and inspection events did not both reach {MATERIALIZER_STREAM}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    anyhow::ensure!(
        receipt_causation.run == receipt_causation.root && receipt_causation.depth == 0,
        "route-origin receipt causation is not a root delivery: {receipt_causation:?}"
    );
    anyhow::ensure!(
        inspection_causation.run != receipt_causation.run
            && inspection_causation.root == receipt_causation.root
            && inspection_causation.depth == receipt_causation.depth + 1,
        "materializer-to-handler causation did not preserve root and advance depth: \
         receipt={receipt_causation:?} inspection={inspection_causation:?}"
    );

    let inspection_rows = project
        .query(
            "SELECT status, row_version FROM receiving.quality_inspection WHERE receipt_id = $1::text::uuid",
            &[&receipt_id],
        )
        .await
        .context("read the materialized quality inspection")?;
    anyhow::ensure!(
        inspection_rows.len() == 1
            && inspection_rows[0].get::<_, String>(0) == "pending"
            && inspection_rows[0].get::<_, i64>(1) == 1,
        "receipt {receipt_id} did not materialize to exactly one pending revision-1 inspection"
    );

    let settled_deadline = std::time::Instant::now() + Duration::from_secs(30);
    let consumer = loop {
        let consumer = stream
            .consumer_info(MATERIALIZER_DURABLE)
            .await
            .context("read the exact materializer durable")?;
        if consumer.name == MATERIALIZER_DURABLE
            && consumer.delivered.consumer_sequence == 1
            && consumer.delivered.stream_sequence == receipt_sequence
            && consumer.ack_floor.consumer_sequence == 1
            && consumer.ack_floor.stream_sequence == receipt_sequence
            && consumer.num_ack_pending == 0
            && consumer.num_pending == 0
            && consumer.num_redelivered == 0
        {
            break consumer;
        }
        anyhow::ensure!(
            std::time::Instant::now() < settled_deadline,
            "materializer durable did not settle exactly once: {consumer:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let mut delivery_advisories = jetstream
        .get_stream(wamn_event_wire::DELIVERY_ADVISORY_STREAM)
        .await
        .context("read the production reader's broker advisory stream")?;
    anyhow::ensure!(
        delivery_advisories.info().await?.state.messages == 0,
        "successful materialization emitted a delivery advisory"
    );

    println!(
        "RECEIVING_MATERIALIZER_PASS receipt_id={receipt_id} source_sequence={receipt_sequence} \
         consumer_sequence={} root={} depth={}",
        consumer.ack_floor.consumer_sequence, receipt_causation.root, inspection_causation.depth
    );
    drop(project);
    project_task.abort();
    Ok(())
}
