   : "${CRD_EVIDENCE:?set a fresh repository docs/perf evidence directory}"
   mkdir -- "$CRD_EVIDENCE"
   CRD_CHART="$CRD_EVIDENCE/runtime-operator-2.9.0.tgz"
   capture_crd_command() {
     local name=$1 result=0
     shift
     printf '%q ' "$@" > "$CRD_EVIDENCE/$name.command"
     printf '\n' >> "$CRD_EVIDENCE/$name.command"
     "$@" > "$CRD_EVIDENCE/$name.stdout" 2> "$CRD_EVIDENCE/$name.stderr" || result=$?
     printf '%s\n' "$result" > "$CRD_EVIDENCE/$name.exit-code"
     return "$result"
   }
   capture_crd_command pull helm pull \
     oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.9.0 \
     --destination "$CRD_EVIDENCE"
   capture_crd_command chart-identity rg -Fqx \
     'Digest: sha256:d70b240cfc3c745f306fc6ebecebff4370e6c8c0568b55c1d02b1eda1716fd17' \
     "$CRD_EVIDENCE/pull.stdout" "$CRD_EVIDENCE/pull.stderr"
   capture_crd_command crds helm show crds "$CRD_CHART"
   capture_crd_command input-hashes sha256sum "$CRD_CHART" "$CRD_EVIDENCE/crds.stdout"
   capture_crd_command apply kubectl apply --server-side --field-manager=wamn-cutover \
     -f "$CRD_EVIDENCE/crds.stdout"
   capture_crd_command established kubectl wait --for=condition=Established --timeout=60s \
     -f "$CRD_EVIDENCE/crds.stdout"
   capture_crd_command installed kubectl get -f "$CRD_EVIDENCE/crds.stdout" -o json
   helm upgrade --install --create-namespace -n wamn-system wamn "$CRD_CHART" \
     -f deploy/infra/values-wamn.yaml --wait --timeout 5m
   sed 's/__ENVIRONMENT_NAMESPACE__/wamn-system/g' \
     deploy/platform/runtime-operator-events-rbac.example.yaml \
     | kubectl apply -f -
   