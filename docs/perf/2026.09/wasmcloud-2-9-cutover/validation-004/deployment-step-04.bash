   helm upgrade --install -n wamn-system wamn-host "$CRD_CHART" \
     -f deploy/platform/values-host-default.yaml -f "$HOST_OVERLAY" \
     --set-string runtime.image.tag="$HOST_TAG" --wait --timeout 5m
   kubectl set image --local -f deploy/platform/executor.yaml \
     executor="wamn-executor:$EXECUTOR_TAG" -o yaml | kubectl apply -f -
   kubectl -n wamn-system rollout status deployment/hostgroup-default --timeout=300s
   kubectl -n wamn-system rollout status deployment/executor --timeout=300s
   