#!/usr/bin/env bash
set -uo pipefail
W=/home/kaalin/.cache/wamn-pilot/runs/012-claude-dock-appointments/work
source "$W/drive.sh"
: > "$W/http.log"
K="$1"   # key namespace, so a re-run never replays a previous run's claims

CARRIER=$(jq -r '.[0].value.carrier_id' <<< "$(call "create-carrier (DOCK-0)" /carrier/create \
  '[{"request_id":"r-create-carrier","name":"Northbound Freight"}]')")
DOCK=$(jq -r '.[0].value.dock_id' <<< "$(call "create-dock (DOCK-0)" /dock/create \
  '[{"request_id":"r-create-dock","name":"Door 7"}]')")

book() { # book <label> <request_id> <key> <carrier> <dock> <start> <end>
  call "$1" /appointment/book "$(printf '[{"request_id":"%s","idempotency_key":"%s","carrier_id":"%s","dock_id":"%s","slot_start":"%s","slot_end":"%s"}]' "$2" "$3" "$4" "$5" "$6" "$7")"
}

FIRST=$(book "book-first (DOCK-2)" r-book-1 "$K-book-1" "$CARRIER" "$DOCK" 2026-10-01T09:00:00Z 2026-10-01T10:00:00Z)
APPT=$(jq -r '.[0].value.appointment_id' <<< "$FIRST")
book "book-replay (DOCK-2)" r-book-replay "$K-book-1" "$CARRIER" "$DOCK" 2026-10-01T09:00:00Z 2026-10-01T10:00:00Z > /dev/null
book "book-changed-body (DOCK-3)" r-book-changed "$K-book-1" "$CARRIER" "$DOCK" 2026-10-01T14:00:00Z 2026-10-01T15:00:00Z > /dev/null

book "overlap-a (DOCK-1)" r-overlap-a "$K-overlap-a" "$CARRIER" "$DOCK" 2026-10-01T11:00:00Z 2026-10-01T12:00:00Z > /dev/null
book "overlap-b (DOCK-1)" r-overlap-b "$K-overlap-b" "$CARRIER" "$DOCK" 2026-10-01T11:30:00Z 2026-10-01T12:30:00Z > /dev/null

# The two concurrent bookings of the gate step, fired together.
( book "overlap-refuses-exactly-one/a (DOCK-1)" r-overlap-a "$K-overlap-a" "$CARRIER" "$DOCK" 2026-10-01T11:00:00Z 2026-10-01T12:00:00Z > "$W/ca.json" ) &
( book "overlap-refuses-exactly-one/b (DOCK-1)" r-overlap-b "$K-overlap-b" "$CARRIER" "$DOCK" 2026-10-01T11:30:00Z 2026-10-01T12:30:00Z > "$W/cb.json" ) &
wait

# Two overlapping bookings under two FRESH keys, fired together: the case the
# replay path cannot decide for us.
( book "race/a (DOCK-1)" r-race-a "$K-race-a" "$CARRIER" "$DOCK" 2026-10-01T20:00:00Z 2026-10-01T21:00:00Z > "$W/ra.json" ) &
( book "race/b (DOCK-1)" r-race-b "$K-race-b" "$CARRIER" "$DOCK" 2026-10-01T20:30:00Z 2026-10-01T21:30:00Z > "$W/rb.json" ) &
wait

call "check-in (DOCK-4)" /appointment/check_in \
  "$(printf '[{"request_id":"r-check-in","appointment_id":"%s","arrived_at":"2026-10-01T09:07:00Z"}]' "$APPT")" > /dev/null
call "check-in-unknown (DOCK-5)" /appointment/check_in \
  '[{"request_id":"r-check-in-unknown","appointment_id":"00000000-0000-0000-0000-000000000000","arrived_at":"2026-10-01T09:07:00Z"}]' > /dev/null
call "check-in-twice (status moves forward only)" /appointment/check_in \
  "$(printf '[{"request_id":"r-check-in-twice","appointment_id":"%s","arrived_at":"2026-10-01T09:09:00Z"}]' "$APPT")" > /dev/null

call "list-one-dock-one-day (DOCK-6)" /appointment/query \
  "$(printf '[{"request_id":"r-query","dock_id":"%s","day":"2026-10-01","status":"scheduled"}]' "$DOCK")" > "$W/day.json"
call "list-arrived (DOCK-6 status filter)" /appointment/query \
  "$(printf '[{"request_id":"r-query-arrived","dock_id":"%s","day":"2026-10-01","status":"arrived"}]' "$DOCK")" > /dev/null
call "list-other-day (DOCK-6 day filter)" /appointment/query \
  "$(printf '[{"request_id":"r-query-other-day","dock_id":"%s","day":"2026-10-02","status":"scheduled"}]' "$DOCK")" > /dev/null

book "book-unknown-carrier" r-unknown-carrier "$K-unknown-carrier" 00000000-0000-0000-0000-000000000000 "$DOCK" 2026-10-03T09:00:00Z 2026-10-03T10:00:00Z > /dev/null
book "book-unknown-dock" r-unknown-dock "$K-unknown-dock" "$CARRIER" 00000000-0000-0000-0000-000000000000 2026-10-03T09:00:00Z 2026-10-03T10:00:00Z > /dev/null
book "book-backwards-slot" r-backwards "$K-backwards" "$CARRIER" "$DOCK" 2026-10-03T10:00:00Z 2026-10-03T09:00:00Z > /dev/null

printf 'carrier=%s\ndock=%s\nappointment=%s\n' "$CARRIER" "$DOCK" "$APPT" > "$W/ids.txt"
echo "concurrent gate pair    slot_unavailable=$(cat "$W/ca.json" "$W/cb.json" | grep -o slot_unavailable | wc -l) booked=$(cat "$W/ca.json" "$W/cb.json" | grep -o '"appointment_id"' | wc -l)"
echo "concurrent fresh pair   slot_unavailable=$(cat "$W/ra.json" "$W/rb.json" | grep -o slot_unavailable | wc -l) booked=$(cat "$W/ra.json" "$W/rb.json" | grep -o '"appointment_id"' | wc -l)"
echo "day list sorted by slot_start ascending: $(jq -r '.[0].value | (.appointments | map(.slot_start)) as $k | ($k == ($k|sort) | tostring)' "$W/day.json")"
