# Summarize repeated sweeps without treating requests as independent trials.
# Run with jq -n -f summarize.jq and all six summary.json files from each phase.
def spread:
  sort | {n: length, min: first, median: .[(length / 2 | floor)], max: last};

[
  inputs |
  (input_filename | capture("/(?<phase>before|after)-(?<run>[0-9]{3})/journey/throughput/(?<credential>service|human)-(?<repetition>[123])/summary.json$")) as $sample |
  .index.source as $source |
  .results[] |
  . + $sample + {source: $source}
] |
if length == 0 then error("no sweep summaries were supplied") else . end |
group_by([.phase, .run, .credential, .layer, .concurrency]) |
map(
  if length != 3 or (map(.repetition) | unique | length) != 3 or (map(.source) | unique | length) != 1 then
    error("each phase, credential, layer, and concurrency requires three distinct repetitions at one source")
  else
    {
      phase: .[0].phase,
      run: .[0].run,
      credential: .[0].credential,
      source: .[0].source,
      layer: .[0].layer,
      concurrency: .[0].concurrency,
      requests_per_second: (map(.requests_per_second) | spread),
      p50_ms: (map(.p50_ms) | spread),
      p99_ms: (map(.p99_ms) | spread),
      host_cpu_ms_per_request: (map(.server.host_cpu_ms_per_request) | spread),
      host_cpu_cores: (map(.server.host_cpu_cores) | spread),
      pg_cpu_cores: (map(.server.pg_cpu_cores) | spread),
      host_throttled_share: (map(.server.host_throttled_share) | if all(. == null) then null else spread end),
      sample_window_seconds: (map(.server.sample_window_seconds) | spread),
      errors: map({repetition, errors, cut_off, total_requests, status_distribution})
    }
  end
)
