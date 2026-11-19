# Platform chart

This chart installs shared services in `platform` and the edge in `hosts`. Use the Helm release name `wamn` in namespace `platform` and install the infrastructure chart first.

Use `values-kind.yaml` or `values-gcp.yaml` for the installation. The schema validates WAMN inputs. Vendored charts accept their upstream values.

Each NATS pod reserves 320 MiB of JetStream storage per `global.maxProjects`. A project stream can use 256 MiB. The remaining 64 MiB per project covers metadata and filesystem overhead. The resolver has its own persistent volume claim when enabled. Lane 10.2 replaces the legacy file-based org account provisioning.

Identity serves `https://identity.platform.svc.cluster.local` and uses the system PostgreSQL login in `platform`. Its two replicas require the same encryption provider and wrapping key as the OAuth hosts.

For local encryption, supply the existing `identity.oauthSecret` with a 32-byte `key`. For Cloud KMS, supply the matching project and region under `identity.oauthEncryption` and configure the pod identity. Identity stores signing keys in encrypted form.

The edge trusts the public identity CA. The kind values set wildcard TLS coverage for `*.wamn.localhost`. GCP terminates TLS at the external load balancer.
If the cluster Ingress uses ACME, set `edge.acme` for the `wamn-edge` Issuer in `hosts`.
