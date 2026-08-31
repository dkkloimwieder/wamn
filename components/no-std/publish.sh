#!/bin/sh
set -eu

usage() {
  echo "usage: $0 TENANT_ID PACKAGE_ID PACKAGE_VERSION PACKAGE_DIRECTORY ARTIFACT_BASE REGISTRY_AUTH_FILE PROJECT_DATABASE_URL CONTROL_DATABASE_URL [--insecure-registry]" >&2
  exit 64
}

[ "$#" -ge 8 ] && [ "$#" -le 9 ] || usage
tenant_id=$1
package_id=$2
package_version=$3
package_directory=$4
artifact_base=$5
registry_auth_file=$6
project_database_url=$7
control_database_url=$8
registry_mode=${9-}

case "$tenant_id" in
  '' | *[!A-Za-z0-9._-]*) usage ;;
esac
case "$package_id" in
  '' | [!a-z]* | *_ | *__* | *[!a-z0-9_]*) usage ;;
esac
case "$package_version" in
  '' | *[!A-Za-z0-9._+-]*) usage ;;
esac
case "$registry_mode" in
  '' | --insecure-registry) ;;
  *) usage ;;
esac

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
component_manifest="$repo_root/components/no-std/Cargo.toml"
component_target="$repo_root/components/no-std/target/wasm32-wasip2/release"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/wamn-palette.XXXXXX")
trap 'rm -rf "$scratch"' EXIT HUP INT TERM

render_declaration() {
  component=$1
  sed \
    -e "s/__TENANT_ID__/$tenant_id/g" \
    -e "s/__PACKAGE_ID__/$package_id/g" \
    -e "s/__PACKAGE_VERSION__/$package_version/g" \
    "$script_dir/$component/declaration.json.in" >"$scratch/$component.json"
}

cargo build \
  --manifest-path "$component_manifest" \
  --locked \
  --release \
  --target wasm32-wasip2 \
  -p transform \
  -p http-request

render_declaration transform
render_declaration http-request

set --
if [ "$registry_mode" = "--insecure-registry" ]; then
  set -- --insecure-registry
fi

cargo run --manifest-path "$repo_root/Cargo.toml" --locked -p wamn-ctl -- \
  push-component \
  --package "$package_directory" \
  --component-bytes "$component_target/transform.wasm" \
  --declaration "$scratch/transform.json" \
  --artifact-base "$artifact_base" \
  --registry-auth-file "$registry_auth_file" \
  --project-database-url "$project_database_url" \
  --control-database-url "$control_database_url" \
  --admit-platform-package wamn:node \
  "$@"

cargo run --manifest-path "$repo_root/Cargo.toml" --locked -p wamn-ctl -- \
  push-component \
  --package "$package_directory" \
  --component-bytes "$component_target/http_request.wasm" \
  --declaration "$scratch/http-request.json" \
  --artifact-base "$artifact_base" \
  --registry-auth-file "$registry_auth_file" \
  --project-database-url "$project_database_url" \
  --control-database-url "$control_database_url" \
  --admit-platform-package wamn:node \
  --admit-platform-package wamn:connection \
  "$@"
