#!/usr/bin/env bash
# One markdown row per trace for the interior of wamn.route.authenticate.
# The residue is authenticate minus its three legs, so time the legs do not
# cover (weld reads, header parse, scheduler hops) is reported, not hidden.
# acquire is scoped to the permission read, so the guest path own acquire is
# not folded into this row.
set -uo pipefail
jq -r --arg l "${2:-$(basename "$1" .json)}" '
  [.batches[]?.scopeSpans[]?.spans[]?] as $s |
  def ms($x): (($x.endTimeUnixNano|tonumber)-($x.startTimeUnixNano|tonumber))/1000000;
  def n($x): ([$s[]|select(.name==$x)|ms(.)]|add // 0);
  def r($v): (($v*1000|round)/1000|tostring);
  ([$s[]|select(.name=="wamn.auth.permissions")|.spanId]) as $perm |
  def under($x): ([$s[]|select(.name==$x and (.parentSpanId as $p | ($perm|index($p))) != null)|ms(.)]|add // 0);
  n("wamn.route.authenticate") as $auth |
  ($auth - n("wamn.auth.pat") - n("wamn.auth.roles") - n("wamn.auth.permissions")) as $res |
  "| " + $l
  + " | " + r(n("wamn.auth.pat"))
  + " | " + r(n("wamn.auth.roles"))
  + " | " + r(n("wamn.auth.permissions"))
  + " | " + r(under("wamn.postgres.acquire"))
  + " | " + r(n("wamn.auth.perm.begin"))
  + " | " + r(n("wamn.auth.perm.timeout"))
  + " | " + r(n("wamn.auth.perm.query"))
  + " | " + r(n("wamn.auth.perm.commit"))
  + " | " + r($res)
  + " | **" + r($auth) + "** |"
' "$1"
