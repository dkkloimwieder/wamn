The retained shell cases produced 183 successful assertions across six offline runs. Each captured script hash matches source `1e87eb4d`. `source-and-count-comparison.json` records the matching hashes and counts from the raw output. The temporary script copies only retain their output directories.

The case maps connect the existing cases to ordinary Rust tests. They also identify shell argument and variable-name cases that no longer apply to typed Rust inputs. The shell callers remain present until the application test runners replace them.

The existing database-host helper accepted a synthetic URL with the expected host text in a query parameter. Commit `60e5e7cd` adds a separate correction that parses the host and explicit port. The matching Rust case passes in `../app-owners-002/command-1.log`. The earlier agent records state that the Rust run was pending because they precede that integrated run.
