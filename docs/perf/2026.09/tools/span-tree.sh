#!/usr/bin/env bash
# Render one trace file as a span tree with durations.
set -uo pipefail
f=$1; label=${2:-$(basename "$f")}
printf '\n===== %s =====\n' "$label"
[ -s "$f" ] || { echo "  (no trace)"; exit 0; }
jq -r '
  [.batches[]?.scopeSpans[]?.spans[]?] as $sp |
  ($sp | map({(.spanId): .}) | add // {}) as $by |
  def ms($s): (($s.endTimeUnixNano|tonumber)-($s.startTimeUnixNano|tonumber))/1000000;
  def depth($s): [ $s ] | until(
      (.[-1].parentSpanId // "") == "" or ($by[.[-1].parentSpanId] == null);
      . + [ $by[.[-1].parentSpanId] ]) | length - 1;
  ($sp | map({start: (.startTimeUnixNano|tonumber)}) | map(.start) | min) as $t0 |
  "  spans=\($sp|length)",
  ( $sp
    | sort_by(.startTimeUnixNano|tonumber)
    | .[]
    | "  " + ("  " * depth(.)) + (.name)
      + "  " + (ms(.)|.*1000|round|./1000|tostring) + " ms"
      + "  @+" + (((.startTimeUnixNano|tonumber) - $t0)/1000000 | .*10|round|./10 | tostring) + " ms"
  )
' "$f"
