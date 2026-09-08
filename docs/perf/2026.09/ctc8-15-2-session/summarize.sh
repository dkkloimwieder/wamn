#!/usr/bin/env bash
# Read retained public evidence only; emit JSON to stdout without writing files.
set -euo pipefail
[[ $# == 0 ]] || { printf 'usage: bash summarize.sh\n' >&2; exit 2; }
evidence_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
shopt -s nullglob
for directory in "$evidence_root"/{targeted,live}-*; do
    [[ -d "$directory" && ! -L "$directory" ]] || continue
    run=${directory##*/}
    [[ "$run" =~ ^(targeted-[0-9]+|live-[a-z-]+-[0-9]+)$ ]] || continue
    arguments=(--arg run "$run")
    for file in "$directory"/{exit,source-base,source.base,start-source.base,after-source.base,receipt,result,test-binary.sha256,ctl-binary.sha256} "$directory"/*.command "$directory"/*.exit; do
        [[ -f "$file" && ! -L "$file" ]] || continue
        arguments+=(--rawfile "${file##*/}" "$file")
    done
    for file in "$directory"/*.command; do
        [[ -f "$file" && ! -L "$file" ]] || continue
        stage=${file##*/}; stage=${stage%.command}
        if [[ -f "$directory/$stage.log" && ! -L "$directory/$stage.log" ]]; then
            arguments+=(--argjson "$stage.log_present" true)
            # Cargo metadata contains no test output and can be very large.
            [[ "$stage" == metadata ]] || arguments+=(--rawfile "$stage.log" "$directory/$stage.log")
        fi
    done
    jq -n "${arguments[@]}" '
      def code: if type == "string" and test("^[0-9]+\\n?$") then rtrimstr("\n") | tonumber else null end;
      def summaries: split("\n") | to_entries | map(.key as $line | .value |
        capture("^test result: (?<result>ok|FAILED)\\. (?<passed>[0-9]+) passed; (?<failed>[0-9]+) failed; (?<ignored>[0-9]+) ignored; (?<measured>[0-9]+) measured; (?<filtered>[0-9]+) filtered out;") |
        {line: ($line + 1), result, passed: (.passed|tonumber), failed: (.failed|tonumber),
         ignored: (.ignored|tonumber), measured: (.measured|tonumber), filtered: (.filtered|tonumber)});
      def failures: [scan("(?m)^test ([A-Za-z_][A-Za-z0-9_:]*) \\.\\.\\. FAILED$") | .[0]] +
        [scan("(?m)^failures:\\n((?:[ \\t]+[A-Za-z_][A-Za-z0-9_:]*\\n)+)\\n?test result:") | .[0] |
         split("\n")[] | gsub("^[ \\t]+"; "") | select(length > 0)] | unique;
      $ARGS.named as $a | ($a.exit | code) as $exit |
      [$a | keys[] | select(endswith(".command")) | rtrimstr(".command") | . as $name |
        ($a[$name + ".log"] // "") as $log | ($a[$name + ".exit"] | code) as $code |
        {name: $name, command: ($run + "/" + $name + ".command"), exit: $code,
         log: (if $a[$name + ".log_present"] then $run + "/" + $name + ".log" else null end),
         compile_failure: ($log | test("(?m)^error(\\[E[0-9]+\\]:|: could not compile|: linking with)")),
         compiler_error_codes: ([$log | scan("(?m)^error\\[(E[0-9]+)\\]:") | .[0]] | unique),
         test_summaries: ($log | summaries), failed_tests: ($log | failures)} |
        .status = (if .exit == null then "incomplete" elif .compile_failure then "compile_failure"
          elif (.failed_tests|length) > 0 or any(.test_summaries[]; .failed > 0) then "test_failure"
          elif .exit != 0 then "command_failure" elif .log == null then "incomplete"
          elif ($a[$name + ".command"] | contains("cargo test")) and (.test_summaries|length) == 0 then "incomplete"
          else "passed" end)] as $steps |
      ([$a.receipt // "" | capture("^PASS suite=(?<suite>[a-z-]+) tests=(?<tests>[0-9]+) ignored=0 named-live-witness=(?<witness>[A-Za-z_][A-Za-z0-9_]*)\\n?$") |
        .tests |= tonumber] | first // null) as $receipt |
      {run: $run, exit: $exit, steps: $steps, receipt: $receipt,
       command_exits: ([$a | to_entries[] | select(.key | endswith(".exit")) | .value |= code] | from_entries),
       receipt_file: (if $a.receipt then $run + "/receipt" else null end),
       sources: ([$a | to_entries[] | select(.key | test("^(source-base|((start-|after-)?source)\\.base)$")) |
         select(.value | test("^[0-9a-f]{40}\\n?$") ) | .value |= rtrimstr("\n")] | from_entries),
       binary_hashes: ([$a | to_entries[] | select(.key | test("^(test|ctl)-binary\\.sha256$")) |
         {key: .key, value: [.value | split("\n")[] | capture("^(?<sha256>[0-9a-f]{64}) [ *](?<path>.+)$")]}] | from_entries),
       counts: {passed: ([$steps[].test_summaries[].passed] | add // 0),
                failed: ([$steps[].test_summaries[].failed] | add // 0)},
       status: (if $exit == null then "incomplete"
         elif ($run | startswith("live-")) and ($a["cleanup.exit"] | code) == null then "incomplete"
         elif $a.result == "abandoned-before-test\n" then "incomplete"
         elif any($steps[]; .status == "compile_failure") then "compile_failure"
         elif any($steps[]; .status == "test_failure") then "test_failure"
         elif $exit != 0 or any($steps[]; .exit != null and .exit != 0) then "command_failure"
         elif ($steps|length) == 0 or any($steps[]; .status == "incomplete") then "incomplete"
         elif ($run | startswith("live-")) and ($receipt == null or ($a["test.exit"] | code) != 0 or
           ($a["readiness.exit"] | code) != 0 or ($a["start.exit"] | code) != 0) then "incomplete"
         else "passed" end)}'
done | jq -s '{scope: "Recorded commands only; counts include repeated increments, not unique coverage.", runs: .}'
