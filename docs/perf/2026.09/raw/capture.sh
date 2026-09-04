#!/usr/bin/env bash
# Per-run capture: the journey's cold trace + N warm requests with their traces.
set -uo pipefail
OUT=${1:?out}; N=${2:-4}; mkdir -p "$OUT"
NS=wamn-receiving-journey; SYS=wamn-system
COLD_TID=11111111111111111111111111111111
log(){ printf '[cap] %s %s\n' "$(date +%H:%M:%S)" "$*"; }

KC=""
for i in $(seq 1 2400); do
  for f in /tmp/tmp.*/kubeconfig; do
    [ -s "$f" ] || continue
    grep -q wamn-receiving-journey "$f" 2>/dev/null && { KC="$f"; break; }
  done
  [ -n "$KC" ] && break; sleep 1
done
[ -n "$KC" ] || { log "no kubeconfig"; exit 1; }
K=(kubectl --kubeconfig "$KC" --context kind-wamn-receiving-journey)
log "kubeconfig=$KC"

# Wait for the journey's own COLD job to finish first — firing early would warm
# the host and destroy the cold measurement.
for i in $(seq 1 2400); do
  "${K[@]}" -n "$NS" get job startup-request-cold -o jsonpath='{.status.succeeded}' 2>/dev/null | grep -q 1 && break
  sleep 1
done
log "cold job done at ${i}s"

pull_trace(){ # tid outfile ; retry until spans appear
  local tid=$1 out=$2 t
  for t in $(seq 1 20); do
    "${K[@]}" get --raw "/api/v1/namespaces/$SYS/services/http:tempo:3200/proxy/api/traces/$tid" >"$out.tmp" 2>/dev/null || { sleep 1; continue; }
    if [ "$(jq '[.batches[]?.scopeSpans[]?.spans[]?]|length' "$out.tmp" 2>/dev/null || echo 0)" -ge 10 ]; then
      mv "$out.tmp" "$out"; return 0
    fi
    sleep 1
  done
  [ -s "$out.tmp" ] && mv "$out.tmp" "$out"; return 1
}

pull_trace "$COLD_TID" "$OUT/trace-cold.json" && log "cold trace captured ($(jq '[.batches[]?.scopeSpans[]?.spans[]?]|length' "$OUT/trace-cold.json") spans)" || log "cold trace INCOMPLETE"
"${K[@]}" -n "$NS" logs job/startup-request-cold >"$OUT/cold-client.log" 2>/dev/null

IMG=$("${K[@]}" -n "$NS" get deploy hostgroup-default -o jsonpath='{.spec.template.spec.containers[0].image}' 2>/dev/null)
RUNTAG=$(basename "$OUT")
cat >"$OUT/warm.yaml" <<EOF
apiVersion: batch/v1
kind: Job
metadata: { name: warm-probe, namespace: $NS }
spec:
  activeDeadlineSeconds: 300
  backoffLimit: 0
  template:
    spec:
      restartPolicy: Never
      containers:
        - name: probe
          image: $IMG
          imagePullPolicy: Never
          env:
            - name: PAT
              valueFrom: { secretKeyRef: { name: wamn-pat-route-caller-acme--receiving--dev, key: token } }
          command: ["/bin/sh","-ec"]
          args:
            - |
              i=0
              while [ \$i -lt $N ]; do
                i=\$((i+1))
                tid=\$(printf 'dd${RUNTAG#run}%030d' \$i | cut -c1-32)
                sid=\$(printf 'ee%014d' \$i | cut -c1-16)
                m=\$(curl --silent --connect-timeout 3 --max-time 120 --output /tmp/b \\
                  --write-out '%{http_code} %{time_starttransfer} %{time_total}' \\
                  --header 'Host: receiving.localhost' --header 'Content-Type: application/json' \\
                  --header "Authorization: Bearer \$PAT" --header "traceparent: 00-\$tid-\$sid-01" \\
                  --data '[{"request_id":"warm","id":"00000000-0000-0000-0000-000000000301"}]' \\
                  "http://flow-http.$NS.svc.cluster.local/purchase_order/get" 2>/dev/null) || m='000 0 0'
                set -- \$m
                echo "WARM n=\$i tid=\$tid status=\$1 ttfb_s=\$2 total_s=\$3"
              done
EOF
"${K[@]}" apply -f "$OUT/warm.yaml" >>"$OUT/run.log" 2>&1
log "warm probe applied"
for i in $(seq 1 120); do
  "${K[@]}" -n "$NS" logs job/warm-probe --tail=-1 >"$OUT/warm-client.log.t" 2>/dev/null && mv "$OUT/warm-client.log.t" "$OUT/warm-client.log"
  [ "$(grep -c '^WARM' "$OUT/warm-client.log" 2>/dev/null || echo 0)" -ge "$N" ] && break
  sleep 1
done
log "warm requests: $(grep -c '^WARM' "$OUT/warm-client.log" 2>/dev/null || echo 0)"
grep -o 'tid=[0-9a-f]*' "$OUT/warm-client.log" 2>/dev/null | sed 's/tid=//' | while read -r t; do
  pull_trace "$t" "$OUT/trace-warm-$t.json" && log "warm trace $t ok" || log "warm trace $t incomplete"
done
log "done: cold=$([ -s "$OUT/trace-cold.json" ] && echo yes || echo no) warm_traces=$(ls "$OUT"/trace-warm-*.json 2>/dev/null|wc -l)"
