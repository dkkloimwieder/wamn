# Identity chart

Identity runs two replicas. Each replica uses eight pooled password connections and a dedicated connection for signing key rotation.
The issuer advisory lock serializes first key creation and rotation across replicas.
Each signature reads the active database key under the issuer row lock.
The HTTPS readiness probe requires a working database connection and an active signing key.

Set `namespace`, `issuer`, the public `database` connection, and `tlsSecret`.
The chart renders `settings.json` and mounts it for `wamn-identity serve --settings`.
The pod mounts the named database login and CA under `/etc/wamn/db`.

Set `oauthEncryption` to the envelope provider that the OAuth hosts use.
For local encryption, use `oauthEncryption: {provider: local}`.
Set `oauthSecret` to the Secret that contains the 32-byte `key`.
The pod mounts the key at `/etc/wamn/oauth/key`.
For Cloud KMS, use `{provider: gcp, project: <project>, region: <region>}` and authorize the pod to use the OAuth KMS key.

Set `localMail` for a local mail file. For Resend, set `resendSecret` and `resendFrom`.
The Resend Secret supplies the `api-key` file. Restart identity after changing the token.

Identity trusts one hop for the forwarding web proxy.
The web proxy sets and validates the browser cookies for tokens returned by the password routes.
