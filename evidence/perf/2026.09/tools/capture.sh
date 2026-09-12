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
    grep -q wamn-receiving-journey "$f" 2>/dev/null || continue
    kubectl --kubeconfig "$f" --context kind-wamn-receiving-journey get ns "$NS" >/dev/null 2>&1 && { KC="$f"; break; }
  done
  [ -n "$KC" ] && break; sleep 1
done
[ -n "$KC" ] || { log "no kubeconfig"; exit 1; }
K=(kubectl --kubeconfig "$KC" --context kind-wamn-receiving-journey)
log "kubeconfig=$KC"

IMG=$("${K[@]}" -n "$NS" get deploy hostgroup-default -o jsonpath='{.spec.template.spec.containers[0].image}' 2>/dev/null)
for i in $(seq 1 600); do
  [ -n "$IMG" ] && break
  sleep 1
  IMG=$("${K[@]}" -n "$NS" get deploy hostgroup-default -o jsonpath='{.spec.template.spec.containers[0].image}' 2>/dev/null)
done
log "image=$IMG"
cat >"$OUT/probe-pod.yaml" <<EOF
apiVersion: v1
kind: Pod
metadata: { name: warm-probe, namespace: $NS }
spec:
  restartPolicy: Never
  containers:
    - name: probe
      image: $IMG
      imagePullPolicy: Never
      env:
        - name: PAT
          valueFrom: { secretKeyRef: { name: wamn-pat-route-caller-acme--receiving--dev, key: token } }
      command: ["/bin/sh","-c","sleep 3600"]
EOF
"${K[@]}" apply -f "$OUT/probe-pod.yaml" >>"$OUT/run.log" 2>&1
"${K[@]}" -n "$NS" wait --for=condition=Ready pod/warm-probe --timeout=300s >>"$OUT/run.log" 2>&1
log "idle probe pod ready"

pull_trace(){ # tid outfile ; retry until the trace has spans
  local tid=$1 out=$2 t
  for t in $(seq 1 25); do
    "${K[@]}" get --raw "/api/v1/namespaces/$SYS/services/http:tempo:3200/proxy/api/traces/$tid" >"$out.tmp" 2>/dev/null || { sleep 1; continue; }
    if [ "$(jq '[.batches[]?.scopeSpans[]?.spans[]?]|length' "$out.tmp" 2>/dev/null || echo 0)" -ge 10 ]; then
      mv "$out.tmp" "$out"; return 0
    fi
    sleep 1
  done
  [ -s "$out.tmp" ] && mv "$out.tmp" "$out"; return 1
}

# Fire our OWN requests the moment the route answers. Request 1 is COLD (first
# request this host has ever served); 2..N are HOT. We do NOT wait on the
# journey's cold job: --measure-startup restarts the host seconds afterwards and
# the restart kills the route permanently, so nothing can be measured after it.
: >"$OUT/client.log"
fire(){ # n label tid
  local n=$1 lbl=$2 tid=$3 sid out
  sid=$(printf 'e0e0e0e0e0e0%04x' "$n")
  out=$("${K[@]}" -n "$NS" exec pod/warm-probe -- /bin/sh -c "curl --silent --connect-timeout 5 --max-time 180 --output /tmp/b --write-out '%{http_code} %{time_starttransfer} %{time_total}' --header 'Host: receiving.localhost' --header 'Content-Type: application/json' --header \"Authorization: Bearer \$PAT\" --header 'traceparent: 00-$tid-$sid-01' --data '[{\"request_id\":\"perf\",\"id\":\"00000000-0000-0000-0000-000000000301\"}]' 'http://flow-http.$NS.svc.cluster.local/purchase_order/get'" 2>>"$OUT/run.log") || out='000 0 0'
  set -- $out
  echo "REQ n=$n phase=$lbl tid=$tid status=$1 ttfb_s=$2 total_s=$3" | tee -a "$OUT/client.log"
}
fire 1 COLD "$(printf 'c01dc01dc01dc01dc01dc01dc01d0001')"
for n in $(seq 2 $((N+1))); do
  fire "$n" HOT "$(printf 'd0d0d0d0d0d0d0d0d0d0d0d0d0d0%04x' "$n")"
done
log "requests done: $(grep -c '^REQ' "$OUT/client.log" 2>/dev/null || echo 0)"
grep -o 'tid=[0-9a-f]*' "$OUT/client.log" 2>/dev/null | sed 's/tid=//' | while read -r t; do
  ph=$(grep -o "phase=[A-Z]* tid=$t" "$OUT/client.log" | head -1 | sed 's/phase=//;s/ tid=.*//')
  pull_trace "$t" "$OUT/trace-${ph:-X}-$t.json" && log "trace $ph $t ok" || log "trace $ph $t incomplete"
done
log "done: requests=$(grep -c '^REQ' "$OUT/client.log" 2>/dev/null || echo 0) traces=$(ls "$OUT"/trace-*.json 2>/dev/null | wc -l)"
