At source `c4294de87aace635fb4418479386bac714ad410c`, the [focused command](command.json) exited 0 after [29.258 seconds](record.json).
The [output](cargo.log#L248) reports four passing tests, zero failures, zero ignored tests, and 61 filtered tests.
The two build-output cases check exact stdout and stderr bytes, recorded command and exit status, private file modes, omission of private environment values, and a recorded spawn failure, while the two retained resource cases check network addresses and PostgreSQL ports.
These ordinary tests do not execute Docker image builds or establish a successful Receiving postcommit run.
