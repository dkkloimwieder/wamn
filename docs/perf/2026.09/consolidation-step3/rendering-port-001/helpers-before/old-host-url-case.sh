#!/usr/bin/env bash
set -euo pipefail
source "/home/kaalin/.cache/wamn-lanes/consolidation-rendering-20260911/tools/journey-host-secrets.sh"
declare -A input=(
  [secret_directory]="/tmp/consolidation-helper-before-20260911/host-url-case"
  [role_families]="executor-platform identity-reader http-admitter event-materializer"
  [guest_secret_file]=guest-sql.json
  [namespace]=warehouse-eu-3
  [database_host]=10.9.8.7
)
declare -A output=()
derive_host_secrets input output
