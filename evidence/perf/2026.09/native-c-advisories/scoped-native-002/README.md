# Scoped native broker test

Source: `a986790f`.

The native test compiled and failed at its 90-second deadline. The broker refused the requested subjects, but the client waited ten seconds for each refused request.

The command returned 101 after 110.44 seconds. Source files stayed unchanged during the run.

`record.json` names the exact command and native binary. `cargo.log` contains the actual output.
