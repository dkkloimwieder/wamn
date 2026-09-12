# 1. MINT — freezes release identity and prints THE DIGEST AS THE WHOLE OF
#    STDOUT. The deployment-attestation coordinate is a tracing record and goes
#    to stderr, so `$(...)` around this captures the digest and nothing else.
wamn-ctl publish-release \
  --database-url "$OWNER_URL" --control-database-url "$CONTROL_URL" \
  --org "$ORG" --project "$PROJECT" \
  --tenant "$TENANT" --effective-release-id "$EFFECTIVE_RELEASE_ID" \
  --environment "$ENVIRONMENT" \
  --verified-publisher-principal "$PUBLISHER_PRINCIPAL" \
  --run-schema "$RUN_SCHEMA" \
  --package "$PACKAGE_ID@$PACKAGE_VERSION" \
  --wiring "$PACKAGE_ID@$PACKAGE_VERSION::$WIRING_ID=$WIRING_VERSION" \
  --attachments attachments.json --route-host "$ROUTE_HOST" \
  --package-manifest ../../packages/receiving/wamn.json

# 2. PUSH the frozen bytes as an OCI artifact, read back from the snapshot the
#    mint wrote rather than from a file, and re-print the same digest.
wamn-ctl push-release-manifest \
  --database-url "$OWNER_URL" --control-database-url "$CONTROL_URL" \
  --tenant "$TENANT" \
  --effective-release-id "$EFFECTIVE_RELEASE_ID" \
  --org "$ORG" --project "$PROJECT" \
  --artifact-base "$PUSH_BASE" --registry-auth-file "$PUSH_DOCKERCONFIG"

# 3. READ the six lines the templates take — both carriers, each labelled with
#    the file it belongs in (wamn-duyl). --artifact-base here is the base THE
#    PODS read, which is not necessarily the one step 2 pushed to.
wamn-ctl print-release-env \
  --database-url "$OWNER_URL" --tenant "$TENANT" \
  --effective-release-id "$EFFECTIVE_RELEASE_ID" \
  --artifact-base registry.wamn-system.svc.cluster.local:5000/wamn/releases

# 4. HAND-EDIT both files with those lines, in one commit. There is no verb that
#    writes them; see the ruling below.

# 5. APPLY. No ordering between these two is recorded anywhere in deploy/, and
#    each refuses to serve a release it cannot verify, so either order is fine.
kubectl -n wamn-system apply -f deploy/platform/executor.yaml
kubectl -n wamn-system rollout status deploy/executor --timeout=300s
helm upgrade --install -n wamn-system wamn-host \
  oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.9.0 \
  -f deploy/platform/values-host-default.yaml \
  -f deploy/platform/values-host-receiving-pat.yaml
kubectl -n wamn-system rollout status deploy/hostgroup-default --timeout=150s
