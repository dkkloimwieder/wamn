# Receiving pin refresh: wamn-10yt.35

The failed `before-001` run stopped during release admission, before timed traffic.
Commit `fe0928c7` changed the Receiving component, but the overlay still referenced its previous digest.
The old digest was `sha256:43fcb1f18e8e9d6be3ea0a84b838a400e5a179665dc4019914a5076d14e03b7d`.
The recorded current artifact has digest `sha256:8057a076d949d21effa45dfff27812f995f5ac101c38f52ac6d66108f1b15b60`.
The owner approved this separate prerequisite on 2026-09-08.

The refresh changes the overlay manifest, publication declaration, and two existing test constants.
The normal generator changed three dependent JSON artifacts and the manifest hash in the generated policy.
The package schema snapshot stayed unchanged.
No authentication query, permission read, schema, grant, or admission rule changed.

`regenerate.sh` used a fresh PostgreSQL 18 container with the base and overlay migrations.
It regenerated only the overlay, then derived the output twice more in check mode.
Both derivations matched the generated files.
The complete command record is `commands.log`, and `run.log` contains the output.

| Existing test selection | Passed | Failed |
| --- | ---: | ---: |
| `acme_overlay_publication` | 2 | 0 |
| `component_dependency_closure_is_exact_and_acyclic` | 1 | 0 |
| `component_dependencies_expand_the_exact_release_closure_and_refuse_cycles` | 1 | 0 |

All commands exited zero, and the owned container was removed.
`exit` and `cleanup.log` retain those results.
The live Receiving journey still belongs to the next benchmark run under `wamn-ctc8.12`.
This report does not claim a completed before/after measurement.
