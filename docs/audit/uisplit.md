# UI split

This report covers epic `wamn-lbps4`, Track 1 of [developer experience](developer-experience.md). A UI change now ships as a web artifact, with no package version, release or host restart.

| Issue | Commit | Result |
| --- | --- | --- |
| `.1` Web artifacts | `6f5985967`, `c34cdca38` | `wamn web build` writes `web.json`, and `wamn web push` pushes an OCI artifact tagged `<package id>-web-<web version>`. The web version comes from `web/package.json` (owner ruling 2026-11-03). Different bytes under an existing tag refuse. |
| `.2` No web layer in packages | `fe7ae7da1`, `951ead2c6` | `wamn build` runs no Vite. The Receiving and WMS digest records change with no version bump. Bead `wamn-3umnd` clears the two stale GKE tags. |
| `.3` to `.6` Pin, edge, upload, rollback | `3bbac9d60`, `ae097ba9f`, `be8439e22`, `347e1597a` | `client = {package, web}` refuses a package outside the release. The edge serves `clients/<pkg>/<web hex>`. The upload is create-only. Rollback restores the previous web pin. The web digest is the chart value `webDigests` of the helm release, and helm renders it into the web release ConfigMap, so no second writer exists. A web-only change alters only that ConfigMap. |

Tests: the selection on `9e287fe1d`, before the rebase onto `da3b41b11`, ran without the full workspace suites, and `package_digest` ran by name. All passed except `jetstream` (`wamn-6oi30`) and `wms-local` (`wamn-fszfy`), which also fail on main. The owner waived the kind run, and the next shared install run covers env apply.
