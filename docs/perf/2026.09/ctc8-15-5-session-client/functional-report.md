# Session client functional boundary

Source: `499fe3a10496fc0d3c9b85e8c1d5b546e4c57ac4`.
The provider and Receiving changes are in parent commit `1e473f59ab1876d9e7f04d01c8cd9c71306499fc`.
The second commit changes only the Rust client generator.
Both commits descend from `c32aeed9569a28b60fb42fe5ae51b8f220be8334`.

The client and Receiving tests passed: 82 passed, zero failed, zero ignored.
The four focused generator tests also passed.
Clippy exited zero with warnings treated as errors for the client and Receiving packages.
The generator run reports an existing unused function in `wamn-catalog`.
This work does not change that function.

The tests cover startup login, concurrent exchange sharing, cached sessions, expiry renewal, and failed renewal.
They cover explicit PAT selection before login and after login.
They also cover request redaction, malformed exchanges, redirect refusal, and non-Unicode PAT input.
The Receiving screen shows the nested fresh-credential refusal without an automatic retry.
Renewal tests use a controlled clock, not elapsed wall time.

Receiving calls the generated operation functions for all four operations.
The generator chooses `invoke_fresh` for an operation with `fresh_only: true`.
False and omitted values retain byte-identical generated output.
No checked-in package output changed.

Set both `WAMN_SESSION_ISSUER` and `WAMN_SESSION_AUDIENCE` to enable session login.
The issuer must be an HTTPS URL.
Its path prefix stays intact before the client appends `/session`.
The audience names the intended project-environment.
The server, not the client, decides whether the principal can access that audience.
`WAMN_TOKEN` remains the PAT source.
If both session variables are absent, Receiving keeps PAT-only startup.
An incomplete pair or a failed login refuses startup before terminal entry.
Tokens stay in memory.

The actual issuer and production-route client proof did not run in this command chain.
The issue remains open for that proof and its existing benchmark acceptance.
No benchmark result or deployment activation is claimed.

The following command chain produced `functional-gates.log` and exited zero:

```bash
RUSTC_WRAPPER= cargo test --locked --offline -p wamn-client -p wamn-receiving-tui --all-targets
RUSTC_WRAPPER= cargo clippy --locked --offline -p wamn-client -p wamn-receiving-tui --all-targets --no-deps -- -D warnings
RUSTC_WRAPPER= cargo test --locked --offline -p wamn-schema-generator --lib client_rust::tests
```
