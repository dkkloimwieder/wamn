# Identity chart

Identity runs two replicas. Each replica uses eight pooled password connections and a dedicated connection for signing-key rotation.
The issuer advisory lock serializes rotation across replicas. Each signature reads the active database key under the issuer lock.

Set `issuer`, `databaseSecret` and `tlsSecret` to the existing authority and credential resources.

For user management, set `operatorCaSecret` to the CA that signs the control client certificate.
Set `controlSubject` to the exact certificate subject `CN=control-<org>`.
The chart sets `WAMN_IDENTITY_CONTROL_SUBJECT` from this value. An empty value disables user management.
Provide the corresponding `control-<org>` Secret for the control release, with `tls.crt` and `tls.key`.

Set `oauthEncryption` to exactly the provider configuration used by the OAuth hosts. Identity requires encrypted signing keys and has no plaintext fallback.
For local encryption, use `oauthEncryption: {provider: local}` and set `oauthSecret` to the existing OAuth Secret containing the 32-byte `key`.
The chart mounts that key at `/etc/wamn/oauth/key` through `WAMN_OAUTH_KEY_FILE`.
For Cloud KMS, supply `{provider: gcp, project: <project>, region: <region>}` and the pod identity authorized for the same OAuth KMS key.
The chart renders the configuration as `WAMN_OAUTH_ENCRYPTION`. Installation supplies these resources separately.

`trustedProxyHops` defaults to one for the forwarding web proxy. The service defaults to zero when run without the chart.
Zero uses the TLS peer address. A positive value counts backward through all `x-forwarded-for` header values and comma-separated addresses.
Restrict direct access to the service when trusting forwarded addresses.

The database stores the packed JSON envelope in `identity.session_keys.private_key_envelope` as bytes.
The envelope contains `ciphertext`, `nonce`, `wrapped_key` and `key_id`. Retirement erases the entire column.
Both replicas require the same provider and wrapping key. Restart them after changing that configuration.
