# Platform chart

This chart installs shared services in `platform` and the edge in `hosts`. Use the Helm release name `wamn` in namespace `platform` and install the infrastructure chart first.

Use `values-kind.yaml` or `values-gcp.yaml` for the installation. The schema validates WAMN inputs. Vendored charts accept their upstream values.

Set `registryEgress` to a list of `cidr` and `port` pairs for registry access from org pods.
If the list is empty, `wamn platform deploy` derives one entry from `registryEndpoint` using `/32` for IPv4 or `/128` for IPv6.
The command requires one of these inputs. The chart writes `registryEgress` as JSON in `wamn-org-inputs`.

Each NATS pod reserves 320 MiB of JetStream storage per `global.maxProjects`. A project stream can use 256 MiB. The remaining 64 MiB per project covers metadata and filesystem overhead. The full resolver has a separate 1 GiB persistent volume claim. The NATS init container waits for identity to publish the operator JWT.

The SQL Job runs `provision-system` from the `wamn-control` image. Platform artifact stamping pins that image through `systemSql.image`.
It applies pending system migrations and records `platformDomain` for application hosts and platform principal email addresses.
Each `wamn platform deploy` waits for the previous SQL Job, removes it, and runs a new Job through Helm.

Identity serves the fixed `global.identityIssuer` address and uses the system PostgreSQL login in `platform`. The edge uses that issuer, and the runtime operator uses the shared broker address in `global.nats.schedulerUrl`.
The chart mounts the `identity-settings` ConfigMap as a JSON file.
The readiness probe checks the database and active signing key.
Both identity replicas share a wrapping key. With local encryption, each org has its own OAuth wrapping key.

For local encryption, supply the existing `identity.oauthSecret` with a 32-byte `key`. For Cloud KMS, supply the matching project and region under `identity.oauthEncryption` and configure the pod identity. Identity stores signing keys in encrypted form.

For local mail, set `identity.localMail` to the outbox path.
For Resend, set `identity.resendSecret` to a Secret in `platform` with an `api-key` entry.
Set `identity.resendFrom` to the verified sender and `identity.inviteUrl` to the web client base.

The edge trusts the public identity CA. On kind, the edge certificate covers `*.<platformDomain>`. GCP terminates TLS at the external load balancer.
If the cluster Ingress uses ACME, set `edge.acme` for the `wamn-edge` Issuer in `hosts`.
