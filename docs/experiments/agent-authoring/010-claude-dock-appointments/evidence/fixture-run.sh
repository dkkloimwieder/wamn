#!/usr/bin/env bash
# Drive the fixture's own steps against the held release, in fixture order.
set -euo pipefail
call() { "$WAMN_PILOT_RUN_DIR/evidence/call.sh" "$1" "$2"; }

echo "### create-carrier"
CARRIER_OUT="$(call /carrier/create '[{"request_id":"create-carrier","name":"Northbound Freight"}]')"
echo "$CARRIER_OUT"
CARRIER="$(head -1 <<<"$CARRIER_OUT" | jq -r '.[0].value.carrier_id')"

echo "### create-dock"
DOCK_OUT="$(call /dock/create '[{"request_id":"create-dock","name":"Door 7"}]')"
echo "$DOCK_OUT"
DOCK="$(head -1 <<<"$DOCK_OUT" | jq -r '.[0].value.dock_id')"

echo "### book-first"
call /appointment/book "[{\"request_id\":\"book-first\",\"idempotency_key\":\"dock-gate-book-1\",\"slot_start\":\"2026-10-01T09:00:00Z\",\"slot_end\":\"2026-10-01T10:00:00Z\",\"carrier_id\":\"$CARRIER\",\"dock_id\":\"$DOCK\"}]"

echo "### book-replay"
call /appointment/book "[{\"request_id\":\"book-replay\",\"idempotency_key\":\"dock-gate-book-1\",\"slot_start\":\"2026-10-01T09:00:00Z\",\"slot_end\":\"2026-10-01T10:00:00Z\",\"carrier_id\":\"$CARRIER\",\"dock_id\":\"$DOCK\"}]"

echo "### book-changed-body"
call /appointment/book "[{\"request_id\":\"book-changed-body\",\"idempotency_key\":\"dock-gate-book-1\",\"slot_start\":\"2026-10-01T14:00:00Z\",\"slot_end\":\"2026-10-01T15:00:00Z\",\"carrier_id\":\"$CARRIER\",\"dock_id\":\"$DOCK\"}]"

echo "### overlap-refuses-exactly-one (overlap-a and overlap-b fired together)"
call /appointment/book "[{\"request_id\":\"overlap-a\",\"idempotency_key\":\"dock-gate-overlap-a\",\"slot_start\":\"2026-10-01T11:00:00Z\",\"slot_end\":\"2026-10-01T12:00:00Z\",\"carrier_id\":\"$CARRIER\",\"dock_id\":\"$DOCK\"}]" > "$WAMN_PILOT_RUN_DIR/evidence/fixture-overlap-a.out" 2>&1 &
call /appointment/book "[{\"request_id\":\"overlap-b\",\"idempotency_key\":\"dock-gate-overlap-b\",\"slot_start\":\"2026-10-01T11:30:00Z\",\"slot_end\":\"2026-10-01T12:30:00Z\",\"carrier_id\":\"$CARRIER\",\"dock_id\":\"$DOCK\"}]" > "$WAMN_PILOT_RUN_DIR/evidence/fixture-overlap-b.out" 2>&1 &
wait
cat "$WAMN_PILOT_RUN_DIR/evidence/fixture-overlap-a.out"
cat "$WAMN_PILOT_RUN_DIR/evidence/fixture-overlap-b.out"
printf 'slot_unavailable refusals: %s (expected 1)\n' \
  "$(grep -lc slot_unavailable "$WAMN_PILOT_RUN_DIR/evidence/fixture-overlap-a.out" "$WAMN_PILOT_RUN_DIR/evidence/fixture-overlap-b.out" 2>/dev/null | wc -l)"

APPOINTMENT="$(psql "$(jq -r '.target_database_url' "$WAMN_PILOT_RUN_DIR/env/dev.json")" -Atqc \
  "select appointment_id from receiving.appointment_book_command where idempotency_key = 'dock-gate-book-1'")"

echo "### check-in"
call /appointment/check_in "[{\"request_id\":\"check-in\",\"arrived_at\":\"2026-10-01T09:07:00Z\",\"appointment_id\":\"$APPOINTMENT\"}]"

echo "### check-in-unknown"
call /appointment/check_in '[{"request_id":"check-in-unknown","appointment_id":"00000000-0000-0000-0000-000000000000","arrived_at":"2026-10-01T09:07:00Z"}]'

echo "### list-one-dock-one-day"
call /appointment/query "[{\"request_id\":\"list-one-dock-one-day\",\"day\":\"2026-10-01\",\"status\":\"scheduled\",\"dock_id\":\"$DOCK\"}]"
