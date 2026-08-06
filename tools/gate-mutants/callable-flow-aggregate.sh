#!/usr/bin/env bash
set -euo pipefail
shopt -s inherit_errexit

readonly CAMPAIGN="callable-flow-aggregate"
readonly BEAD="wamn-2jdm.5.4"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name a unique debug target directory" >&2
  exit 2
fi

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT GATE
declare -a MANIFESTS JOB_NAMES CONTAINERS LOG_MARKERS IMAGE_TAGS PREPARED_TAGS

mutation_ids() {
  printf '%s\n' \
    schema-nullable-decision \
    cron-activation-digest-drift \
    f0-response-contract-wave1 \
    f1-direct-node-contract-wave1 \
    f2-direct-node-contract-wave2 \
    f3-cutoff-contract-wave1 \
    f4-callback-contract-wave2 \
    wave1-source-image-drift \
    wave2-mixed-image-id \
    f2invoke-wrong-recommendation \
    f3proof-wrong-cutoff \
    f4proof-wrong-delivery-count
}

load_mutation() {
  local id="$1"
  MANIFESTS=()
  JOB_NAMES=()
  CONTAINERS=()
  LOG_MARKERS=()
  IMAGE_TAGS=()
  case "$id" in
    schema-nullable-decision)
      TARGET="deploy/poc/poc-material-receiving.catalog.json"
      EXPECTED_SHA="f3bea131fcd1c828de20b546c0775eba3b8952ddbf9a01d7a5c567655a1adb2d"
      NEEDLE='{ "id": "decided_at", "name": "decided_at", "type": { "kind": "timestamptz" } }'
      REPLACEMENT='{ "id": "decided_at", "name": "decided_at", "type": { "kind": "timestamptz" }, "nullable": true }'
      GATE="callable-flow-schema"
      MANIFESTS=(deploy/gates/callable-flow-schema-job.yaml)
      JOB_NAMES=(callable-flow-schema)
      CONTAINERS=(callable-flow-schema)
      LOG_MARKERS=("dispositions.decided_at must be required")
      ;;
    cron-activation-digest-drift)
      TARGET="tests/integration/src/callable_cron.rs"
      EXPECTED_SHA="a7869d016f1f4b1ea418bb7d4c79c2a057f2a50e5c7f916d948bd32083560da8"
      NEEDLE="VALUES (\$1,\$2,'gate',\$3,'sha256:callable-cron',true)"
      REPLACEMENT="VALUES (\$1,\$2,'gate',\$3,'sha256:stale-cron',true)"
      GATE="callable-flow-cron"
      MANIFESTS=(deploy/gates/callable-flow-cron-job.yaml)
      JOB_NAMES=(callable-flow-cron)
      CONTAINERS=(proof)
      LOG_MARKERS=("attachment-definition-not-current")
      ;;
    f0-response-contract-wave1)
      TARGET="deploy/poc/f0-flow.json"
      EXPECTED_SHA="3c8d7ca1d3c8253a80c2f1563d80d751e2b9e254d95594ebd05e7f0421b64bd7"
      NEEDLE='{ "id": "respond", "type": "respond", "config": { "status": 200 } }'
      REPLACEMENT='{ "id": "respond", "type": "fail", "config": { "status": 500 } }'
      GATE="callable-flow-f0+callable-flow-wave1"
      MANIFESTS=(deploy/gates/callable-flow-f0-job.yaml deploy/gates/callable-flow-wave1-job.yaml)
      JOB_NAMES=(callable-flow-f0 callable-flow-wave1)
      CONTAINERS=(proof proof)
      LOG_MARKERS=("no-response-node" "no-response-node")
      IMAGE_TAGS=(f0 wave1)
      ;;
    f1-direct-node-contract-wave1)
      TARGET="deploy/poc/f1-flow.json"
      EXPECTED_SHA="df38e2044ff7cc652bac142ce9ca772d71e572f6137e2712abbc138c55eb3432"
      NEEDLE=$'"id": "normalize-receipt",\n      "type": "normalize-receipt"'
      REPLACEMENT=$'"id": "normalize-receipt",\n      "type": "transform"'
      GATE="callable-flow-f1+callable-flow-wave1"
      MANIFESTS=(deploy/gates/callable-flow-f1-job.yaml deploy/gates/callable-flow-wave1-job.yaml)
      JOB_NAMES=(callable-flow-f1 callable-flow-wave1)
      CONTAINERS=(proof proof)
      LOG_MARKERS=("F1 direct component types drift" "F1 direct component types drift")
      IMAGE_TAGS=(f1 wave1)
      ;;
    f2-direct-node-contract-wave2)
      TARGET="deploy/poc/f2-flow.json"
      EXPECTED_SHA="b478d9d87337888357968c7c19c042a88dfc7c035739a57508289875065ccca0"
      NEEDLE=$'"id": "recommend-disposition",\n      "type": "disposition-recommendation"'
      REPLACEMENT=$'"id": "recommend-disposition",\n      "type": "transform"'
      GATE="callable-flow-f2+callable-flow-wave2"
      MANIFESTS=(deploy/gates/callable-flow-f2-job.yaml deploy/gates/callable-flow-wave2-job.yaml)
      JOB_NAMES=(callable-flow-f2 callable-flow-wave2)
      CONTAINERS=(proof proof)
      LOG_MARKERS=("F2 recommendation node type drift" "F2 recommendation node type drift")
      IMAGE_TAGS=(f2 wave2)
      ;;
    f3-cutoff-contract-wave1)
      TARGET="deploy/poc/f3-flow.json"
      EXPECTED_SHA="30f067e87ff1cfc5e75d9b19b94e8acc166f1fd69c19f8cec7b6e7a2567b16e7"
      NEEDLE='"offset-ms": -172800000'
      REPLACEMENT='"offset-ms": -86400000'
      GATE="callable-flow-f3+callable-flow-wave1"
      MANIFESTS=(deploy/gates/callable-flow-f3-job.yaml deploy/gates/callable-flow-wave1-job.yaml)
      JOB_NAMES=(callable-flow-f3 callable-flow-wave1)
      CONTAINERS=(proof proof)
      LOG_MARKERS=("cutoff-at-48h config offset-ms drift" "cutoff-at-48h config offset-ms drift")
      IMAGE_TAGS=(f3 wave1)
      ;;
    f4-callback-contract-wave2)
      TARGET="deploy/poc/f4-flow.json"
      EXPECTED_SHA="ed2fc81fe68e5e87b32cbf188604796b0e47b7e3e9c83d7dbf82b36f5fbb5344"
      NEEDLE='"connection": "erp-callback"'
      REPLACEMENT='"connection": "missing-callback"'
      GATE="callable-flow-f4+callable-flow-wave2"
      MANIFESTS=(deploy/gates/callable-flow-f4-job.yaml deploy/gates/callable-flow-wave2-job.yaml)
      JOB_NAMES=(callable-flow-f4 callable-flow-wave2)
      CONTAINERS=(proof proof)
      LOG_MARKERS=("unknown-connection-requirement" "unknown-connection-requirement")
      IMAGE_TAGS=(f4 wave2)
      ;;
    wave1-source-image-drift)
      TARGET="deploy/gates/callable-flow-wave1-job.yaml"
      EXPECTED_SHA="e6d298aeb24c49e52e072608fff6e4cfb9ddd7e762e11c71efb03da37ac9d549"
      NEEDLE='"--source-identity", "ISSUE"'
      REPLACEMENT='"--source-identity", "0000000000000000000000000000000000000000"'
      GATE="callable-flow-wave1-identity"
      MANIFESTS=(deploy/gates/callable-flow-wave1-job.yaml)
      JOB_NAMES=(callable-flow-wave1)
      CONTAINERS=(proof)
      LOG_MARKERS=("image identity is not bound to the source commit")
      IMAGE_TAGS=(wave1)
      ;;
    wave2-mixed-image-id)
      TARGET="deploy/gates/callable-flow-wave2-job.yaml"
      EXPECTED_SHA="2238eb362838a43cf4ecce7d567102470593828d1310b23d4aa01dd47ed64942"
      NEEDLE='"--image-id", "IMAGE_ID"'
      REPLACEMENT='"--image-id", "sha256:0000000000000000000000000000000000000000000000000000000000000000"'
      GATE="callable-flow-wave2-mixed-image"
      MANIFESTS=(deploy/gates/callable-flow-wave2-job.yaml)
      JOB_NAMES=(callable-flow-wave2)
      CONTAINERS=(proof)
      LOG_MARKERS=("callable-flow-wave2 PASS")
      IMAGE_TAGS=(wave2)
      ;;
    f2invoke-wrong-recommendation)
      TARGET="tests/integration/src/f2invoke.rs"
      EXPECTED_SHA="897c0c10f3c11d8252d7697d52253746287eb424d1c093655182cc7382fe0881"
      NEEDLE=$'r#"{"hold":{"material":"resin-A","moisture_pct":"12.00","moisture_max_pct":"5.00"},"decision":"reject"}"#,\n            "reject",'
      REPLACEMENT=$'r#"{"hold":{"material":"resin-A","moisture_pct":"12.00","moisture_max_pct":"5.00"},"decision":"reject"}"#,\n            "accept",'
      GATE="f2invoke"
      MANIFESTS=(deploy/gates/f2invoke-job.yaml)
      JOB_NAMES=(f2invoke)
      CONTAINERS=(f2invoke)
      LOG_MARKERS=("overall PASS: false")
      ;;
    f3proof-wrong-cutoff)
      TARGET="deploy/gates/f3proof-job.yaml"
      EXPECTED_SHA="528aedd2fb7acaf4a7933dcd3ba53f5a5b38835554cbd546228de81a7d26dffc"
      NEEDLE='"--offset-ms=-60000"'
      REPLACEMENT='"--offset-ms=-1"'
      GATE="f3proof"
      MANIFESTS=(deploy/gates/f3proof-job.yaml)
      JOB_NAMES=(f3proof)
      CONTAINERS=(f3proof)
      LOG_MARKERS=("overall PASS: false")
      ;;
    f4proof-wrong-delivery-count)
      TARGET="tests/integration/src/f4proof.rs"
      EXPECTED_SHA="55b72655b6abb96f1ee7d71cb39831a8aedff71b31d1a797fc0dc22b66b02ada"
      NEEDLE='&& rec.requests == u64::from(args.fail_first_n) + 1,'
      REPLACEMENT='&& rec.requests == u64::from(args.fail_first_n) + 2,'
      GATE="f4proof"
      MANIFESTS=(deploy/gates/f4proof-job.yaml)
      JOB_NAMES=(f4proof)
      CONTAINERS=(f4proof)
      LOG_MARKERS=("overall PASS: false")
      ;;
    *) echo "unknown mutant: $id" >&2; return 2 ;;
  esac
}

sha256() { sha256sum "$1" | cut -d ' ' -f 1; }

assert_clean_target() {
  git diff --quiet -- "$TARGET" || { echo "$TARGET has unstaged changes" >&2; return 2; }
  git diff --cached --quiet -- "$TARGET" || { echo "$TARGET has staged changes" >&2; return 2; }
}

assert_precondition() {
  local actual
  actual="$(sha256 "$TARGET")"
  [[ "$actual" == "$EXPECTED_SHA" ]] || {
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  }
  TARGET="$TARGET" NEEDLE="$NEEDLE" python3 -c \
    'import os,pathlib,sys; data=pathlib.Path(os.environ["TARGET"]).read_text(); sys.exit(0 if data.count(os.environ["NEEDLE"]) == 1 else 1)' || {
      echo "$TARGET must contain the mutation anchor exactly once" >&2
      return 2
    }
}

replace_once() {
  TARGET="$TARGET" NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os,pathlib; p=pathlib.Path(os.environ["TARGET"]); s=p.read_text(); p.write_text(s.replace(os.environ["NEEDLE"], os.environ["REPLACEMENT"], 1))'
}

base_image() {
  printf 'wamn-gates:callable-flow-base-%s' "$(git rev-parse HEAD)"
}

ensure_base_image() {
  local image
  image="$(base_image)"
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    docker build --target gates -t "$image" . >&2
  fi
}

build_debug_image() {
  local id="$1" container image
  image="wamn-gates:callable-flow-debug-$(git rev-parse HEAD)-$id"
  if docker image inspect "$image" >/dev/null 2>&1; then
    printf '%s' "$image"
    return
  fi
  cargo build --locked -p wamn-gates
  ensure_base_image
  container="wamn-cf-mutant-${BASHPID}"
  docker create --name "$container" "$(base_image)" >/dev/null
  trap 'docker rm -f "$container" >/dev/null 2>&1 || true' RETURN
  docker cp "$CARGO_TARGET_DIR/debug/wamn-gates" \
    "$container:/usr/local/bin/wamn-gates"
  docker commit "$container" "$image" >/dev/null
  docker rm -f "$container" >/dev/null
  trap - RETURN
  printf '%s' "$image"
}

prepare_tags() {
  local image="$1" id="$2" suffix tag commit
  commit="$(git rev-parse HEAD)"
  if ((${#IMAGE_TAGS[@]} == 0)); then
    IMAGE_TAGS=("mutation-$id")
  fi
  PREPARED_TAGS=()
  for suffix in "${IMAGE_TAGS[@]}"; do
    tag="wamn-gates:cf-${suffix}-${commit}"
    docker tag "$image" "$tag"
    PREPARED_TAGS+=("$tag")
  done
  if ! kind load docker-image "${PREPARED_TAGS[@]}" --name wamn; then
    echo "kind image load returned nonzero; verifying the completed imports once" >&2
    kind load docker-image "${PREPARED_TAGS[@]}" --name wamn
  fi
}

cleanup_jobs_and_images() {
  local image="$1" image_id="$2" name node reference tag
  for name in "${JOB_NAMES[@]}"; do
    kubectl -n wamn-system delete job "$name" --ignore-not-found=true \
      --wait=true --timeout=120s >/dev/null 2>&1 || true
  done
  if [[ -n "$image" ]] && ((${#PREPARED_TAGS[@]} > 0)); then
    while IFS= read -r node; do
      for tag in "${PREPARED_TAGS[@]}"; do
        docker exec "$node" crictl rmi "$tag" >/dev/null 2>&1 || true
      done
      if [[ -n "$image_id" ]]; then
        while IFS= read -r reference; do
          [[ "$reference" == *@"$image_id" ]] || continue
          docker exec "$node" ctr -n k8s.io images rm "$reference" \
            >/dev/null 2>&1 || true
        done < <(docker exec "$node" ctr -n k8s.io images ls 2>/dev/null | awk 'NR > 1 {print $1}')
      fi
    done < <(kind get nodes --name wamn 2>/dev/null || true)
    docker image rm "${PREPARED_TAGS[@]}" "$image" >/dev/null 2>&1 || true
  fi
}

kind_image_id() {
  local tag="$1" node record candidate image_id=
  while IFS= read -r node; do
    record="$(docker exec "$node" crictl inspecti "$tag")"
    candidate="$(jq -r '
      [.status.repoDigests[]?
       | select(test("@sha256:[0-9a-f]{64}$"))
       | split("@")[-1]] | first // ""
    ' <<<"$record")"
    [[ "$candidate" =~ ^sha256:[0-9a-f]{64}$ ]] || {
      echo "kind node $node lacks an observed repo digest for $tag" >&2
      return 1
    }
    if [[ -z "$image_id" ]]; then
      image_id="$candidate"
    elif [[ "$candidate" != "$image_id" ]]; then
      echo "kind nodes disagree on observed image ID for $tag" >&2
      return 1
    fi
  done < <(kind get nodes --name wamn)
  [[ -n "$image_id" ]] || { echo "kind cluster wamn has no nodes" >&2; return 1; }
  printf '%s' "$image_id"
}

tag_for_manifest() {
  local manifest="$1" id="$2" commit
  commit="$(git rev-parse HEAD)"
  case "$manifest" in
    *callable-flow-f0-job.yaml) printf 'wamn-gates:cf-f0-%s' "$commit" ;;
    *callable-flow-f1-job.yaml) printf 'wamn-gates:cf-f1-%s' "$commit" ;;
    *callable-flow-f2-job.yaml) printf 'wamn-gates:cf-f2-%s' "$commit" ;;
    *callable-flow-f3-job.yaml) printf 'wamn-gates:cf-f3-%s' "$commit" ;;
    *callable-flow-f4-job.yaml) printf 'wamn-gates:cf-f4-%s' "$commit" ;;
    *callable-flow-wave1-job.yaml) printf 'wamn-gates:cf-wave1-%s' "$commit" ;;
    *callable-flow-wave2-job.yaml) printf 'wamn-gates:cf-wave2-%s' "$commit" ;;
    *) printf 'wamn-gates:cf-mutation-%s-%s' "$id" "$commit" ;;
  esac
}

render_manifest() {
  local source="$1" output="$2" tag="$3" image_id="$4" commit
  commit="$(git rev-parse HEAD)"
  SOURCE="$source" OUTPUT="$output" TAG="$tag" COMMIT="$commit" IMAGE_ID_VALUE="$image_id" \
    python3 -c 'import os,pathlib; s=pathlib.Path(os.environ["SOURCE"]).read_text(); s=s.replace("wamn-gates:dev", os.environ["TAG"]); s=s.replace("wamn-gates:cf-f0-ISSUE", os.environ["TAG"]); s=s.replace("wamn-gates:cf-f1-ISSUE", os.environ["TAG"]); s=s.replace("wamn-gates:cf-f2-ISSUE", os.environ["TAG"]); s=s.replace("wamn-gates:cf-f3-ISSUE", os.environ["TAG"]); s=s.replace("wamn-gates:cf-f4-ISSUE", os.environ["TAG"]); s=s.replace("wamn-gates:cf-wave1-ISSUE", os.environ["TAG"]); s=s.replace("wamn-gates:cf-wave2-ISSUE", os.environ["TAG"]); s=s.replace("ISSUE", os.environ["COMMIT"]); s=s.replace("IMAGE_ID", os.environ["IMAGE_ID_VALUE"]); pathlib.Path(os.environ["OUTPUT"]).write_text(s)'
}

run_jobs() (
  local expectation="$1" id="$2" work image image_id index manifest tag rendered verdict spec
  work="$(mktemp -d)"
  image=
  image_id=
  PREPARED_TAGS=()
  cleanup() {
    local status=$?
    trap - EXIT INT TERM
    cleanup_jobs_and_images "$image" "$image_id"
    rm -rf -- "$work"
    exit "$status"
  }
  trap cleanup EXIT INT TERM
  image="$(build_debug_image "$id")"
  prepare_tags "$image" "$id"
  if ! image_id="$(kind_image_id "$(tag_for_manifest "${MANIFESTS[0]}" "$id")")"; then
    prepare_tags "$image" "$id"
    image_id="$(kind_image_id "$(tag_for_manifest "${MANIFESTS[0]}" "$id")")"
  fi
  for index in "${!MANIFESTS[@]}"; do
    manifest="${MANIFESTS[$index]}"
    tag="$(tag_for_manifest "$manifest" "$id")"
    rendered="$work/job-$index.yaml"
    verdict="$work/verdict-$index.json"
    render_manifest "$manifest" "$rendered" "$tag" "$image_id"
    if [[ "$expectation" == expected-negative && "$id" == f3proof-wrong-cutoff ]]; then
      # The green and mutant F3 definitions are distinct immutable artifacts;
      # give the deliberate break its own release version so the deployed proof
      # reaches the wrong-cutoff assertion rather than a publication conflict.
      RENDERED="$rendered" python3 -c '
import os, pathlib, sys
p = pathlib.Path(os.environ["RENDERED"])
s = p.read_text()
needle = "\"--flow-version\", \"2\""
if s.count(needle) != 1:
    sys.exit("rendered f3proof manifest must contain flow version 2 exactly once")
p.write_text(s.replace(needle, "\"--flow-version\", \"3\"", 1))
'
    fi
    if [[ "$expectation" == positive ]]; then
      spec="$(jq -cn --arg name "${JOB_NAMES[$index]}" --arg container "${CONTAINERS[$index]}" --arg image "$tag" --arg log "${LOG_MARKERS[$index]}" '{name:$name,container:$container,expectation:"positive",exit_code:0,image:$image,log_contains:$log}')"
      if [[ "$manifest" == *callable-flow-wave2-job.yaml ]]; then
        spec="$(jq -c --arg image_id "$image_id" '. + {claimed_image_id:$image_id,claim_log_prefix:"claimed-image-id="}' <<<"$spec")"
      fi
      tools/kubernetes-gate-run --manifest "$rendered" --verdict-record "$verdict" \
        --namespace wamn-system --timeout-secs 900 --job "$spec"
    else
      if [[ "$id" == wave2-mixed-image-id ]]; then
        local before after runner_exit
        before="$work/state-before"
        after="$work/state-after"
        tools/gate-mutants/callable-flow-state-probe.sh --namespace wamn-system >"$before"
        spec="$(jq -cn --arg name "${JOB_NAMES[$index]}" --arg container "${CONTAINERS[$index]}" --arg image "$tag" --arg log "${LOG_MARKERS[$index]}" --arg image_id "$image_id" '{name:$name,container:$container,expectation:"positive",exit_code:0,image:$image,log_contains:$log,claimed_image_id:$image_id,claim_log_prefix:"claimed-image-id="}')"
        set +e
        tools/kubernetes-gate-run --manifest "$rendered" --verdict-record "$verdict" \
          --namespace wamn-system --timeout-secs 900 --job "$spec"
        runner_exit=$?
        set -e
        tools/gate-mutants/callable-flow-state-probe.sh --namespace wamn-system >"$after"
        [[ $runner_exit -ne 0 ]] || { echo "mixed image identity survived" >&2; return 1; }
        cmp -s "$before" "$after" || { echo "mixed image identity changed state" >&2; return 1; }
        jq -e '.verdict == "fail" and any(.failure_classes[]; contains("image-id-mismatch"))' "$verdict" >/dev/null || {
          echo "mixed image identity lacked the exact mismatch verdict" >&2
          return 1
        }
        jq -c . "$verdict"
        continue
      fi
      spec="$(jq -cn --arg name "${JOB_NAMES[$index]}" --arg container "${CONTAINERS[$index]}" --arg image "$tag" --arg log "${LOG_MARKERS[$index]}" '{name:$name,container:$container,expectation:"expected-negative",exit_code:1,image:$image,log_contains:$log}')"
      if [[ "$manifest" == *callable-flow-wave2-job.yaml ]]; then
        spec="$(jq -c --arg image_id "$image_id" '. + {claimed_image_id:$image_id,claim_log_prefix:"claimed-image-id="}' <<<"$spec")"
      fi
      tools/kubernetes-gate-run --manifest "$rendered" --verdict-record "$verdict" \
        --namespace wamn-system --timeout-secs 900 --job "$spec" \
        --snapshot-executable tools/gate-mutants/callable-flow-state-probe.sh \
        --snapshot-arg --namespace --snapshot-arg wamn-system
    fi
    jq -c . "$verdict"
  done
)

run_green() {
  local id="$1"
  load_mutation "$id"
  assert_clean_target
  assert_precondition
  echo "GREEN id=$id gate=$GATE target=$TARGET command=$0 green $id"
  case "$id" in
    schema-nullable-decision) LOG_MARKERS=("callable-flow-schema PASS") ;;
    cron-activation-digest-drift) LOG_MARKERS=("callable cron attachment/admission proof PASS") ;;
    f0-response-contract-wave1) LOG_MARKERS=("callable-flow-f0 PASS" "callable-flow-wave1 PASS") ;;
    f1-direct-node-contract-wave1) LOG_MARKERS=("callable-flow-f1 PASS" "callable-flow-wave1 PASS") ;;
    f2-direct-node-contract-wave2) LOG_MARKERS=("callable-flow-f2 PASS" "callable-flow-wave2 PASS") ;;
    f3-cutoff-contract-wave1) LOG_MARKERS=("callable-flow-f3 PASS" "callable-flow-wave1 PASS") ;;
    f4-callback-contract-wave2) LOG_MARKERS=("callable-flow-f4 PASS" "callable-flow-wave2 PASS") ;;
    wave1-source-image-drift) LOG_MARKERS=("callable-flow-wave1 PASS") ;;
    wave2-mixed-image-id) LOG_MARKERS=("callable-flow-wave2 PASS") ;;
    f2invoke-wrong-recommendation) LOG_MARKERS=("overall PASS: true") ;;
    f3proof-wrong-cutoff) LOG_MARKERS=("overall PASS: true") ;;
    f4proof-wrong-delivery-count) LOG_MARKERS=("overall PASS: true") ;;
  esac
  run_jobs positive "$id-green"
}

run_mutant() (
  local id="$1" backup_dir backup restored_sha mutant_sha
  load_mutation "$id"
  assert_clean_target
  assert_precondition
  backup_dir="$(mktemp -d)"
  backup="$backup_dir/original"
  cp "$TARGET" "$backup"
  restore() {
    cp "$backup" "$TARGET"
    restored_sha="$(sha256 "$TARGET")"
    rm -f "$backup"
    rmdir "$backup_dir"
    [[ "$restored_sha" == "$EXPECTED_SHA" ]] || {
      echo "restore failed for $TARGET: expected $EXPECTED_SHA, got $restored_sha" >&2
      exit 3
    }
  }
  trap restore EXIT INT TERM
  replace_once
  mutant_sha="$(sha256 "$TARGET")"
  [[ "$mutant_sha" != "$EXPECTED_SHA" ]] || { echo "mutation did not change $TARGET" >&2; exit 3; }
  echo "MUTANT id=$id gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=$0 run $id"
  run_jobs expected-negative "$id"
  echo "KILLED id=$id gate=$GATE exit_code=1"
)

check_campaign() {
  local id
  while IFS= read -r id; do
    load_mutation "$id"
    assert_clean_target
    assert_precondition
    printf 'CHECKED id=%s gate=%s target=%s sha256=%s\n' "$id" "$GATE" "$TARGET" "$EXPECTED_SHA"
  done < <(mutation_ids)
}

usage() { echo "usage: $0 list | check | green MUTANT | green-all | run MUTANT | run-all" >&2; }
case "${1:-}" in
  list) mutation_ids ;;
  check) check_campaign ;;
  green) [[ $# -eq 2 ]] || { usage; exit 2; }; run_green "$2" ;;
  green-all) [[ $# -eq 1 ]] || { usage; exit 2; }; while IFS= read -r id; do run_green "$id"; done < <(mutation_ids) ;;
  run) [[ $# -eq 2 ]] || { usage; exit 2; }; run_mutant "$2" ;;
  run-all) [[ $# -eq 1 ]] || { usage; exit 2; }; while IFS= read -r id; do run_mutant "$id"; done < <(mutation_ids) ;;
  *) usage; exit 2 ;;
esac
