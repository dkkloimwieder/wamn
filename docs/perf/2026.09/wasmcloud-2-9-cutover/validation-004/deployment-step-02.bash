   kubectl -n wamn-system scale deployment \
     -l wasmcloud.com/name=hostgroup --replicas=0
   kubectl -n wamn-system scale deployment/executor --replicas=0
   kubectl -n wamn-system wait --for=delete pod \
     -l wasmcloud.com/name=hostgroup --timeout=90s
   kubectl -n wamn-system wait --for=delete pod -l app=executor --timeout=90s
   