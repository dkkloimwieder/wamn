# Run with jq -n and the eleven trace-breakdown JSON files from each phase.
def spread:
  sort | {n: length, min: first, median: .[(length / 2 | floor)], max: last};
[
  inputs |
  . + {file: input_filename,
       run: (input_filename | capture("/(?<run>before-002|after-001)/journey/").run)} |
  . + {group: (if .phase == "restart-first" then "service-restart"
               elif (.phase | test("^steady(-[2-5])?$")) then "service-warm"
               elif (.phase | test("^human-[1-5]$")) then "human-warm"
               else error("unexpected trace phase") end)}
] |
if length != 22 then error("both phases require eleven traces") else . end |
group_by([.run, .group]) |
map(
  if length != (if .[0].group == "service-restart" then 1 else 5 end)
     or (map(.phase) | unique | length) != length then
    error("missing or repeated trace phase")
  else
    {run: .[0].run, group: .[0].group,
     authentication_ms: (map(.authentication_ms) | spread),
     identity_read_spans: (map(.identity_read_spans) | unique),
     permission_read_spans: (map(.permission_read_spans) | unique),
     samples: map({file, phase, authentication_ms, identity_read_spans, permission_read_spans})}
  end
)
