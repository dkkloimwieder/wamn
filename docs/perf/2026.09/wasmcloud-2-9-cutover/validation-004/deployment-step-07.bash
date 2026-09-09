kubectl apply -f deploy/infra/cert-manager.yaml
kubectl -n cert-manager wait --for=condition=Available deploy --all --timeout=180s
