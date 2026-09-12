   set -euo pipefail
   : "${HOST_TAG:?set the built 2.9 WAMN host image tag}"
   : "${EXECUTOR_TAG:?set the built 2.9 WAMN executor image tag}"
   HOST_OVERLAY=deploy/platform/values-host-receiving-pat.yaml
   