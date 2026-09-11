The first integrated rendering run passed all 25 tests on 1e87eb4d. The run took 23.438 seconds and skipped no test. It includes 16 typed rendering cases, seven trace cases, and two existing process-adapter cases. [Command](command.json), [result](result.json), [full output](output.log).

The earlier shell cases passed 123 assertions before extraction. Their mapping names the retained semantic properties and the obsolete Bash-specific cases. [Case map](case-map.json).

Rust now renders host values, HTTP workloads, materializer workloads, and the temporary kind configuration from typed YAML fields. The identity assertions refuse missing, repeated, or wrong application claims. The old shell callers still run until the application setup extraction replaces them. This is a tested preparation commit, and the script cleanup remains in progress.
