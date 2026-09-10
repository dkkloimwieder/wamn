`wamn-10yt.68` documents why cursor numeric values retain strict spellings.
The change adds six comment lines beside the numeric validator and its existing refusal test.
All executable code and accepted or refused values remain unchanged.

The [owner ruling](../../../poc/wamn_receiving_layered_application_poc_scenario.md) defines an opaque v1 cursor that clients preserve verbatim.
The cursor contains canonical compact JSON encoded as unpadded base64url.
`decode_cursor` compares the exact canonical bytes.
The numeric validator therefore preserves the PostgreSQL spelling and scale, including `12.3400`.
User-entered numbers pass through normalization before serialization, so that boundary can accept spellings such as `01.0` and `1.`.

The existing cursor target passed all five tests with zero ignored or filtered tests.
The command took 20.471 seconds, including compilation.
Both focused formatting commands and `git diff --check` passed.
The test build reported one existing dead-code warning in `wamn-catalog` for `canonical_serialized`.
No new tests were added.
No full workspace run or cluster gate ran for this comment-only change.

The [capture helper](../effects-response/tools/capture.py) recorded each command, source hashes, output, duration, and exit status.
The `cursor-001`, `format-source-001`, `format-test-001`, and `diff-001` directories contain those receipts.
`SHA256SUMS` records the receipt and report hashes.

Run the existing target with:

```sh
cargo test --locked --offline -p wamn-schema-generator --test cursor -- --include-ignored
```
