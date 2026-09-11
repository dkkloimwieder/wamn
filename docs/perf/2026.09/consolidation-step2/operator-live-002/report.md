The live test failed at `d57bc393` after 139.612 seconds.
It completed the native rebuild and opened a replacement operator with a fresh draft.
The old operator, host, and socket stopped before their replacements started.
Both host logs stayed private, and their output did not enter the operator screens.

The test then waited for exit while the composed app requested confirmation to discard its draft.
The retained terminal output contains the complete confirmation prompt.
The next test change answers that prompt before waiting for shutdown.
This run did not reach the Tempo assertions.
All seven cleanup conditions passed, and source bytes stayed unchanged.
