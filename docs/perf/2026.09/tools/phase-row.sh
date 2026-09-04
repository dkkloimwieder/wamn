#!/usr/bin/env bash
# One markdown table row per trace, including the unspanned residue:
# handle_http_request minus the sum of its DIRECT children, so time between
# spans is reported rather than hidden by a flat sum of nested spans.
set -uo pipefail
jq -r --arg l "${2:-$(basename "$1" .json)}" '
  [.batches[]?.scopeSpans[]?.spans[]?] as $s |
  ($s | map({(.spanId): .}) | add // {}) as $by |
  def ms($x): (($x.endTimeUnixNano|tonumber)-($x.startTimeUnixNano|tonumber))/1000000;
  def n($x): ([$s[]|select(.name==$x)|ms(.)]|add // 0);
  def r($v): (($v*1000|round)/1000|tostring);
  ([$s[] | select(.name=="handle_http_request")] | .[0]) as $root |
  (if $root == null then 0 else ms($root) end) as $total |
  # Residue = the root span minus the named phases that PARTITION the work.
  # These are siblings in wall-clock terms (db contains sql and acquire, so db
  # is counted and sql is not). What is left is time inside handle_http_request
  # that no span covers.
  ( n("wamn.route.authenticate") + n("wamn.router.resolve") + n("wamn.component.pull")
    + n("wamn.component.compile") + n("wamn.component.linker_setup") + n("wamn.component.link")
    + n("wamn.component.instantiate") + n("wamn.postgres") ) as $covered |
  ($total - $covered) as $residue |
  "| " + $l
  + " | " + r(n("wamn.route.authenticate"))
  + " | " + r(n("wamn.router.resolve"))
  + " | " + r(n("wamn.component.pull"))
  + " | " + r(n("wamn.component.compile"))
  + " | " + r(n("wamn.component.linker_setup"))
  + " | " + r(n("wamn.component.link"))
  + " | " + r(n("wamn.component.instantiate"))
  + " | " + r(n("wamn.postgres"))
  + " | " + r(n("wamn.postgres.statement"))
  + " | " + r($residue)
  + " | **" + r($total) + "**"
  + " | " + (if (n("wamn.postgres.statement") + n("wamn.component.instantiate")) > 0
             then r($total / (n("wamn.postgres.statement") + n("wamn.component.instantiate")))
             else "n/a" end) + " |"
' "$1"
