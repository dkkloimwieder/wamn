#!/usr/bin/env bash
set -uo pipefail
OUT=${1:?out}; mkdir -p "$OUT"
NS=wamn-receiving-journey; SYS=wamn-system
log(){ printf '[s] %s %s\n' "$(date +%H:%M:%S)" "$*"; }

# 1. discover the journey's private kubeconfig
KC=""
for i in $(seq 1 1800); do
  for f in /tmp/tmp.*/kubeconfig; do
    [ -s "$f" ] || continue
    grep -q "wamn-receiving-journey" "$f" 2>/dev/null && { KC="$f"; break; }
  done
  [ -n "$KC" ] && break
  sleep 1
done
[ -n "$KC" ] || { log "no kubeconfig found"; exit 1; }
log "kubeconfig=$KC"
K=(kubectl --kubeconfig "$KC" --context kind-wamn-receiving-journey)

# 2. wait for the flow-http service to have a ready endpoint
for i in $(seq 1 1800); do
  n=$("${K[@]}" -n "$NS" get endpointslice -l kubernetes.io/service-name=flow-http \
        -o jsonpath='{.items[*].endpoints[*].conditions.ready}' 2>/dev/null | grep -o true | wc -l)
  [ "${n:-0}" -ge 1 ] && break
  sleep 1
done
log "flow-http has $n ready endpoint(s) after ${i}s"
IMG=$("${K[@]}" -n "$NS" get deploy hostgroup-default -o jsonpath='{.spec.template.spec.containers[0].image}' 2>/dev/null)
log "image=$IMG"
[ -n "$IMG" ] || { log "no image"; exit 1; }

# 3. sampler job: 150 requests, 1s apart, each with its own trace id
cat >"$OUT/sampler.yaml" <<EOF
apiVersion: batch/v1
kind: Job
metadata: { name: wamn-sampler, namespace: $NS }
spec:
  activeDeadlineSeconds: 900
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
              while [ \$i -lt 150 ]; do
                i=\$((i+1))
                tid=\$(printf 'cafe%028d' \$i)
                sid=\$(printf 'feed%012d' \$i)
                m=\$(curl --silent --show-error --connect-timeout 3 --max-time 120 \\
                  --output /tmp/b --write-out '%{http_code} %{time_starttransfer} %{time_total}' \\
                  --header 'Host: receiving.localhost' --header 'Content-Type: application/json' \\
                  --header "Authorization: Bearer \$PAT" \\
                  --header "traceparent: 00-\$tid-\$sid-01" \\
                  --data '[{"request_id":"sampler","id":"00000000-0000-0000-0000-000000000301"}]' \\
                  "http://flow-http.$NS.svc.cluster.local/purchase_order/get" 2>/dev/null) || m='000 0 0'
                set -- \$m
                echo "SAMPLE n=\$i tid=\$tid status=\$1 ttfb_s=\$2 total_s=\$3"
                sleep 1
              done
EOF
"${K[@]}" apply -f "$OUT/sampler.yaml" >>"$OUT/run.log" 2>&1
log "sampler applied"

# 4. persist logs continuously; teardown must not take the data
for i in $(seq 1 900); do
  "${K[@]}" -n "$NS" logs job/wamn-sampler --tail=-1 >"$OUT/samples.log.tmp" 2>/dev/null \
    && mv "$OUT/samples.log.tmp" "$OUT/samples.log"
  # pull traces for samples that already returned 200
  grep -h 'status=200' "$OUT/samples.log" 2>/dev/null | sed 's/.*tid=\([0-9a-f]*\) .*/\1/' | while read -r t; do
    [ -s "$OUT/trace-$t.json" ] && continue
    "${K[@]}" get --raw "/api/v1/namespaces/$SYS/services/http:tempo:3200/proxy/api/traces/$t" \
      >"$OUT/trace-$t.json" 2>/dev/null || rm -f "$OUT/trace-$t.json"
  done
  "${K[@]}" -n "$NS" get pods >/dev/null 2>&1 || { log "cluster gone at ${i}s"; break; }
  sleep 2
done
log "capture ended; samples=$(grep -c SAMPLE "$OUT/samples.log" 2>/dev/null) traces=$(ls "$OUT"/trace-*.json 2>/dev/null | wc -l)"
