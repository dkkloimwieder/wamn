The composed Receiving operator passed all five terminal sessions in 4.807 seconds.
The [result](result.json) records each completed assertion and unchanged input bytes.
The binary came from `52ac90a6`, and the test used the pending Python change recorded in [the command](command.json).

The test covers the initial query, authenticated requests, and refusal to send duplicate requests.
It also covers terminal restoration, shutdown during a held request, and isolation from a late response.
The final two sessions cover SIGINT and Ctrl-C.
A disposable HTTP server supplied the declared query response.
This run did not start the platform or use PostgreSQL.

[Attempt 001](../operator-pty-001/result.json) expected the retired empty-screen text and failed.
[Attempt 002](../operator-pty-002/result.json) stopped at the sandbox socket restriction.
[Attempt 003](../operator-pty-003/result.json) retained the actual empty query frame and failed on the same text.
The final test expects that observed empty query table and retains all request and shutdown assertions.
