#!/usr/bin/env bash
set -euo pipefail
proof_output=$1
source tools/journey-document.sh
work_dir=$(mktemp -d /tmp/wamn-effects-document.XXXXXX)
trap 'rm -rf -- "$work_dir"' EXIT
journey_schema=$PWD/tests/integration/schema/wamn-journey.schema.json
example=$PWD/tests/integration/schema/wamn-journey.example.json
declare -A journey_spec=()
while IFS=$'\t' read -r key value; do
  journey_spec[$key]=$value
done < <(jq -r 'to_entries[] | select(.value | type == "string") | [.key,.value] | @tsv' "$example")
journey_document=$work_dir/journey.json
write_journey_document journey_spec "$journey_schema" "$journey_document"
amend_journey_document "$journey_schema" "$journey_document" runtime "$(jq -c .runtime "$example")"
cp "$journey_document" "$work_dir/original.json"
runtime_node_ip=10.0.0.2
runtime_node_port=30999
app_fixture_pallet_id=00000000-0000-0000-0000-000000000301
app_fixture_location_a_id=00000000-0000-0000-0000-000000000201
python3 - "$work_dir/setup.sh" <<'PY'
from pathlib import Path
import sys
source = Path('tools/wms-cluster-journey-run').read_text()
start = source.index('  partial_journey_document=$work_dir/journey-partial.json\n')
end = source.index("  # The normal object proof finished above.", start)
Path(sys.argv[1]).write_text(source[start:end])
PY
source "$work_dir/setup.sh"
cmp "$work_dir/original.json" "$journey_document"
jq -S --arg target "$app_fixture_location_a_id" '.runtime.to_location_id = $target' \
  "$journey_document" > "$work_dir/expected.json"
cmp "$work_dir/expected.json" "$partial_journey_document"
cp "$work_dir/original.json" "$proof_output/original.json"
cp "$partial_journey_document" "$proof_output/partial.json"
cp "$work_dir/setup.sh" "$proof_output/executed-setup.sh"
printf 'The original document is unchanged and the new document selects the return location.\n'
