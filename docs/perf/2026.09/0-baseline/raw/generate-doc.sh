#!/usr/bin/env bash
# Regenerate docs/perf/2026.09/cold-v-hot.md from the traces on disk.
set -uo pipefail
REPO=/home/kaalin/dev/wamn
D=$REPO/docs/perf/2026.09
T=$D/traces
tree_of(){ jq -r '
  [.batches[]?.scopeSpans[]?.spans[]?] as $sp |
  ($sp | map({(.spanId): .}) | add // {}) as $by |
  def ms($s): (($s.endTimeUnixNano|tonumber)-($s.startTimeUnixNano|tonumber))/1000000;
  def depth($s): [ $s ] | until((.[-1].parentSpanId // "")=="" or ($by[.[-1].parentSpanId]==null); . + [ $by[.[-1].parentSpanId] ]) | length - 1;
  ($sp | map(.startTimeUnixNano|tonumber) | min) as $t0 |
  ( $sp | sort_by(.startTimeUnixNano|tonumber) | .[]
    | ("  " * depth(.)) + .name
      + "  " + ((ms(.)*1000|round)/1000|tostring) + " ms"
      + "  @+" + ((((.startTimeUnixNano|tonumber)-$t0)/100000|round)/10|tostring) + " ms" )
' "$1" 2>/dev/null; }
row(){ jq -r --arg lbl "$2" '
  [.batches[]?.scopeSpans[]?.spans[]?] as $sp |
  def ms($s): (($s.endTimeUnixNano|tonumber)-($s.startTimeUnixNano|tonumber))/1000000;
  def n($x): ([$sp[]|select(.name==$x)|ms(.)]|add // 0);
  def r($v): (($v*1000|round)/1000|tostring);
  "| " + $lbl + " | " + ($sp|length|tostring) + " | " + r(n("wamn.route.authenticate")) + " | " + r(n("wamn.router.resolve"))
   + " | **" + r(n("wamn.component.pull")) + "** | **" + r(n("wamn.component.compile")) + "** | "
   + r(n("wamn.component.linker_setup")) + " | " + r(n("wamn.component.link")) + " | " + r(n("wamn.component.instantiate"))
   + " | " + r(n("wamn.postgres")) + " | " + r(n("wamn.postgres.statement")) + " | " + r(n("handle_http_request")) + " |"
' "$1" 2>/dev/null; }
{
cat <<'HDR'
# Request path: cold versus hot

Measured latency of one released HTTP route, end to end, with the OTLP span
breakdown for every phase. Filed because the steady-state request path measures
in seconds where the target is milliseconds.

HDR
printf '**Source commit:** `%s`  \n' "$(git -C $REPO rev-parse --short HEAD)"
printf '**Measured:** %s  \n' "$(date -Is)"
printf '**Raw traces:** `docs/perf/2026.09/traces/`\n\n'
cat <<'MET'
## Method

`tools/receiving-cluster-journey-run --apply --measure-startup`, which builds the
release and stands up its own disposable kind cluster (`wamn-receiving-journey`).
The frozen `kind-wamn` cluster is never a target.

- **Route:** `POST /purchase_order/get`, `Host: receiving.localhost`, one SQL statement.
- **Caller:** a Job pod inside the cluster, curling the `flow-http` ClusterIP
  Service directly. There is no Ingress in the path — pod to Service to host pod.
- **COLD** is the journey's own first request against a freshly deployed host.
- **HOT** requests were fired only *after* the cold job completed, so the cold
  measurement is unperturbed by the sampler.
- Traces were pulled from the in-cluster Tempo through the API-server proxy.

## Client-side totals

MET
printf '| source | phase | ttfb ms | total ms |\n|---|---|---:|---:|\n'
for d in "$HOME/.cache/wamn-perf-results/journey-run-A" "$HOME/.cache/wamn-perf-results/journey-run-B"; do
  [ -f "$d/first-request-cold.receipt" ] || continue
  awk -v s="$(basename $d)" '{for(i=1;i<=NF;i++){split($i,p,"=");if(p[1]=="first_byte_ms")t=p[2];if(p[1]=="total_ms")o=p[2]}printf "| %s | **COLD** | %.3f | **%.3f** |\n",s,t,o}' "$d/first-request-cold.receipt"
done
awk '/^SAMPLE/{for(i=1;i<=NF;i++){split($i,p,"=");if(p[1]=="n")n=p[2];if(p[1]=="status")s=p[2];if(p[1]=="ttfb_s")t=p[2];if(p[1]=="total_s")o=p[2]}if(s=="200")printf "| sampler on run B | hot %s | %.3f | %.3f |\n",n,t*1000,o*1000}' "$HOME/.cache/wamn-perf-results/earlier-warm/samples.log" 2>/dev/null
printf '\n## Phase breakdown (ms)\n\n'
printf '| trace | spans | auth | resolve | PULL | COMPILE | linker | link | inst | db | sql | handle_http |\n'
printf '|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n'
for f in "$T"/cold-*.json; do [ -s "$f" ] && row "$f" "COLD $(basename $f .json)"; done
for f in "$T"/warm-*.json; do [ -s "$f" ] && row "$f" "hot $(basename $f .json | sed 's/warm-//')"; done
cat <<'NOTE'

`warm-001` carries only 6 spans: its trace was still flushing when fetched. It is
listed for completeness and is **not** a data point — its auth and resolve figures
are an artifact of the partial trace.

## What this shows

NOTE
cat <<'SHOW'
Pull and compile are the request. Everything else, including the work the request
exists to do, is noise beside them.

- **`wamn.component.pull` ~1.0 s** and **`wamn.component.compile` ~0.38 s** together
  account for **97.9 %** of a hot request.
- **The SQL statement is 0.43 ms.** The whole database interaction is 3.7 ms.
- **Nothing hides in an uninstrumented phase.** `handle_http_request` is 1380.0 ms
  against a client-measured 1381.5 ms, so ingress, kube-proxy, TCP, HTTP framing,
  respond and teardown together are **1.5 ms**.
- **The caching that exists works.** `wamn.router.resolve` is 0.06 ms — the wiring
  cache is doing its job. The component artifact is what nothing caches.
- Compile runs on every request despite a populated wasmtime cache directory that
  the journey itself snapshots and verifies unchanged.

Against upstream wasmCloud's topology bench (20k–60k req/s per host on trivial
work, ~0.05 ms/request) this path is roughly **28 000x** slower. Removing pull and
compile would leave ~29 ms, still ~580x, but an ordinary performance problem
rather than this one.

## Two findings outside the latency question

**The gate cannot fail on latency.** `run_startup_request` asserts `status == 200`
and that the two timings parse as floats. Nothing more. A tree-wide search for any
latency bound returns nothing — every hit is a CDC stall threshold or a curl
timeout. Both cold requests above were recorded `verdict=pass`. That is why a
three-order-of-magnitude gap survived without a red gate.

**The restart arm is broken, three runs out of three.** After the host restarts,
the probe gets 46 consecutive `curl: (7) Could not connect` over 45 s — never a
404, never a 200 — and `--measure-startup` dies there without reaching its own
steady phase.

## Reproducibility

Cold reproduces to within 0.5 % across two independent clusters measured at load
average ~11 and ~1.5, so none of this is host contention. The hot samples are four
consecutive requests against one host, so they are one independent observation with
four repeats, not four independent points.

## Full span trees

SHOW
for f in "$T"/cold-*.json "$T"/warm-*.json; do
  [ -s "$f" ] || continue
  printf '\n### %s\n\n```\n' "$(basename "$f" .json)"; tree_of "$f"; printf '```\n'
done
} > "$D/cold-v-hot.md"
echo "wrote $D/cold-v-hot.md ($(wc -l <"$D/cold-v-hot.md") lines)"
