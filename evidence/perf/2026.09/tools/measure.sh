#!/usr/bin/env bash
set -uo pipefail
D=$1; TAG=$2; NS=wamn-receiving-journey
KC=""
until [ -n "$KC" ]; do
  for f in /tmp/tmp.*/kubeconfig; do [ -s "$f" ] || continue; grep -q "$NS" "$f" 2>/dev/null || continue
    kubectl --kubeconfig "$f" --context "kind-$NS" get ns "$NS" >/dev/null 2>&1 && { KC="$f"; break; }; done
  ps -eo cmd= | grep -q '^bash tools/receiving-cluster-journey-run' || { echo "JOURNEY GONE"; exit 1; }
  sleep 8
done
K=(kubectl --kubeconfig "$KC" --context "kind-$NS")
IMG=""; until [ -n "$IMG" ]; do IMG=$("${K[@]}" -n "$NS" get deploy hostgroup-default -o jsonpath='{.spec.template.spec.containers[0].image}' 2>/dev/null); sleep 5; done
printf 'apiVersion: v1\nkind: Pod\nmetadata: { name: warm-probe, namespace: %s }\nspec:\n  restartPolicy: Never\n  containers:\n    - name: probe\n      image: %s\n      imagePullPolicy: Never\n      env:\n        - name: PAT\n          valueFrom: { secretKeyRef: { name: wamn-pat-route-caller-acme--receiving--dev, key: token } }\n      command: ["/bin/sh","-c","sleep 3600"]\n' "$NS" "$IMG" > "$D/probe-$TAG.yaml"
"${K[@]}" apply -f "$D/probe-$TAG.yaml" >/dev/null 2>&1
"${K[@]}" -n "$NS" wait --for=condition=Ready pod/warm-probe --timeout=900s >/dev/null 2>&1
: > "$D/client-$TAG.log"
# A WRITE, to prove the other half of the rule: a transactional statement must
# still emit bind_claims and commit. Classification proven at build time with the
# runtime path unexercised is how a proof misses a lost transaction.
fire_write(){ local tid=$1 sid out
 sid=$(printf 'f0f0f0f0f0f0%04x' 1)
 out=$("${K[@]}" -n "$NS" exec pod/warm-probe -- /bin/sh -c "curl --silent --connect-timeout 5 --max-time 300 --output /tmp/bw --write-out '%{http_code} %{time_starttransfer} %{time_total}' --header 'Host: receiving.localhost' --header 'Content-Type: application/json' --header \"Authorization: Bearer \$PAT\" --header 'traceparent: 00-$tid-$sid-01' --data '[{\"request_id\":\"perf-write\",\"id\":\"00000000-0000-0000-0000-000000000301\",\"expected_row_version\":\"1\",\"change\":{\"supplier_id\":\"00000000-0000-0000-0000-000000000402\"}}]' 'http://flow-http.$NS.svc.cluster.local/purchase_order/update'" 2>/dev/null) || out='000 0 0'
 set -- $out; echo "$1 $2 $3"; }
fire(){ local n=$1 tid=$2 sid out; sid=$(printf 'e2e2e2e2e2e2%04x' "$n")
 out=$("${K[@]}" -n "$NS" exec pod/warm-probe -- /bin/sh -c "curl --silent --connect-timeout 5 --max-time 300 --output /tmp/b --write-out '%{http_code} %{time_starttransfer} %{time_total}' --header 'Host: receiving.localhost' --header 'Content-Type: application/json' --header \"Authorization: Bearer \$PAT\" --header 'traceparent: 00-$tid-$sid-01' --data '[{\"request_id\":\"perf\",\"id\":\"00000000-0000-0000-0000-000000000301\"}]' 'http://flow-http.$NS.svc.cluster.local/purchase_order/get'" 2>/dev/null) || out='000 0 0'
 set -- $out; echo "$1 $2 $3"; }
CT=$(printf 'c%031x' 0x11)
for i in $(seq 1 150); do r=$(fire 1 "$CT"); set -- $r; [ "$1" = 200 ] && { echo "REQ n=1 phase=COLD tid=$CT status=$1 ttfb_s=$2 total_s=$3" | tee -a "$D/client-$TAG.log"; break; }; sleep 3; done
for n in 2 3 4 5; do t=$(printf 'd2d2d2d2d2d2d2d2d2d2d2d2d2d2%04x' "$n"); r=$(fire "$n" "$t"); set -- $r; echo "REQ n=$n phase=HOT tid=$t status=$1 ttfb_s=$2 total_s=$3" | tee -a "$D/client-$TAG.log"; done
WT=$(printf 'a%031x' 0x77)
r=$(fire_write "$WT"); set -- $r
echo "REQ n=w phase=WRITE tid=$WT status=$1 ttfb_s=$2 total_s=$3" | tee -a "$D/client-$TAG.log"
for t in $(grep -o 'tid=[0-9a-f]*' "$D/client-$TAG.log" | sed 's/tid=//'); do
  case $t in c*) ph=cold;; a*) ph=write;; *) ph=hot;; esac
  for i in $(seq 1 15); do
    "${K[@]}" get --raw "/api/v1/namespaces/wamn-system/services/http:tempo:3200/proxy/api/traces/$t" > "$D/t.tmp" 2>/dev/null \
      && n=$(jq '[.batches[]?.scopeSpans[]?.spans[]?]|length' "$D/t.tmp" 2>/dev/null || echo 0) || n=0
    [ "${n:-0}" -ge 10 ] && { mv "$D/t.tmp" "$D/trace-$TAG-$ph-$t.json"; echo "trace $ph $t spans=$n"; break; }
    sleep 2
  done
done
rm -f "$D/t.tmp"
echo "=== $TAG DONE ==="; cat "$D/client-$TAG.log"
