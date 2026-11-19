# Platform chart

This chart installs shared services in `platform` and the edge in `hosts`. Use the Helm release name `wamn` in namespace `platform` and install the infrastructure chart first.

Use `values-kind.yaml` or `values-gcp.yaml` for the installation. The schema validates WAMN inputs. Vendored charts accept their upstream values.

Each NATS pod reserves 320 MiB of JetStream storage per `global.maxProjects`. A project stream can use 256 MiB. The remaining 64 MiB per project covers metadata and filesystem overhead. The full resolver has a separate 1 GiB persistent volume claim. The NATS init container waits for identity to publish the operator JWT.

The SQL Job runs `provision-system` from the `wamn-control` image. Platform artifact stamping pins that image through `systemSql.image`.
It applies pending system migrations and records `platformDomain` for application hosts and platform principal email addresses.
Each Helm upgrade runs the Job again.

Identity serves the fixed `global.identityIssuer` address and uses the system PostgreSQL login in `platform`. The edge uses that issuer, and the runtime operator uses the shared broker address in `global.nats.schedulerUrl`.
The chart mounts the `identity-settings` ConfigMap as a JSON file.
The readiness probe checks the database and active signing key.
Both replicas use the same encryption provider and wrapping key as the OAuth hosts.

For local encryption, supply the existing `identity.oauthSecret` with a 32-byte `key`. For Cloud KMS, supply the matching project and region under `identity.oauthEncryption` and configure the pod identity. Identity stores signing keys in encrypted form.

For local mail, set `identity.localMail` to the outbox path.
For Resend, set `identity.resendSecret` to a Secret in `platform` with an `api-key` entry.
Set `identity.resendFrom` to the verified sender and `identity.inviteUrl` to the web client base.

The edge trusts the public identity CA. On kind, the edge certificate covers `*.<platformDomain>`. GCP terminates TLS at the external load balancer.
If the cluster Ingress uses ACME, set `edge.acme` for the `wamn-edge` Issuer in `hosts`.
