# wamn architecture review: full export, 2026-11-12 17:55 ET

All 91 tabs of the Claude Doc "wamn architecture review", as Markdown. Each file was checked byte for byte (SHA-256) against a fresh export of its tab.

- `01-summary.md`: decisions by area and every open point.
- `02-architecture.md`: current and proposed system; its two diagrams are in `images/`.
- `03-crate-review.md`: the index of rulings per tab.
- `crate-review/`: tabs 01 to 88.
- Date chips and tab links are kept as plain text.

| File | Bytes | SHA-256 (first 16) |
| --- | ---: | --- |
| 01-summary.md | 9274 | 127279ef8e229fbf |
| 02-architecture.md | 15362 | 125202a6b6176f89 |
| 03-crate-review.md | 28776 | f5e9709fda4f51c0 |
| crate-review/01-wamn-execution-contract.md | 19436 | b6b9b1ce0a314b37 |
| crate-review/02-wamn-control-registry.md | 12105 | 2ebc54a50d382e5a |
| crate-review/03-wamn-event-wire.md | 9258 | 188fcacac26ced81 |
| crate-review/04-wamn-postgres-statements.md | 13736 | 25ba259015060c91 |
| crate-review/05-wamn-session.md | 11146 | 793d1a7a0f87ff09 |
| crate-review/06-wamn-pg-core.md | 7866 | 2f7a8e49138a142a |
| crate-review/07-wamn-component-policy.md | 11867 | fced47e1f027cb9b |
| crate-review/08-wamn-number-pattern.md | 2261 | 0883254c76a9c04b |
| crate-review/09-wamn-run-state.md | 13676 | c5362c5332ead274 |
| crate-review/10-wamn-runtime-part-1.md | 7935 | 2db60a89d3fcf86c |
| crate-review/11-wamn-runtime-part-2.md | 20998 | 1d5b4dfdc33633bc |
| crate-review/12-wamn-runtime-part-3.md | 15322 | e51793ce142a62cd |
| crate-review/13-wamn-runtime-part-4.md | 12138 | 5b62653fd4a8be24 |
| crate-review/14-wamn-runtime-part-5.md | 11527 | b674f7adbb8b9807 |
| crate-review/15-wamn-runtime-part-6.md | 8776 | 5e633c4c2ce7c3f3 |
| crate-review/16-wamn-runtime-part-7.md | 10806 | a58c08f4a761daee |
| crate-review/17-wamn-runtime-part-8.md | 15076 | d32a62fc6ee826ad |
| crate-review/18-wamn-runtime-part-9.md | 12544 | d00f5fa36682cc50 |
| crate-review/19-wamn-runtime-part-10.md | 10235 | 003e7da0c00d7eb6 |
| crate-review/20-wamn-runtime-part-11.md | 11901 | d4fb80aceff48af9 |
| crate-review/21-wamn-engine-part-1.md | 8948 | 2aa74fc9849abb86 |
| crate-review/22-wamn-engine-part-2.md | 10175 | bdfb64fd0233b643 |
| crate-review/23-wamn-engine-part-3.md | 9303 | 7bb95ec64e9a5ea2 |
| crate-review/24-wamn-engine-part-4.md | 10688 | c3142dbd21def35b |
| crate-review/25-wamn-execution-host-part-1.md | 17291 | 53888e194d077a90 |
| crate-review/26-wamn-execution-host-part-2.md | 13426 | a43873901a7b68e1 |
| crate-review/27-wamn-execution-host-part-3.md | 9513 | 881c2dd4002f2b4b |
| crate-review/28-wamn-router.md | 6337 | a4a43ea51793bf32 |
| crate-review/29-wamn-workflow-part-1.md | 7864 | a1ed79c2c1a0d120 |
| crate-review/30-wamn-workflow-part-2.md | 6363 | c1bc406149871ba9 |
| crate-review/31-http-route-guest.md | 16423 | ff9f14f204b5322d |
| crate-review/32-wamn-schema-generator-part-1.md | 3603 | 0d2b449deea49119 |
| crate-review/33-wamn-schema-generator-part-2.md | 4802 | 3221f60dcb0f9298 |
| crate-review/34-wamn-schema-generator-part-3.md | 7373 | 032fb035493e39eb |
| crate-review/35-wamn-schema-generator-part-4.md | 4906 | 3a3109069abe910c |
| crate-review/36-wamn-schema-generator-part-5.md | 3378 | 86d55a08d9d0ff55 |
| crate-review/37-wamn-schema-generator-part-6.md | 8827 | 9ffca02dfa7961de |
| crate-review/38-wamn-schema-generator-part-7.md | 12198 | c990b9811198cb7a |
| crate-review/39-wamn-schema-generator-part-8.md | 8645 | 55f235528982b163 |
| crate-review/40-wamn-schema-generator-part-9.md | 4182 | 6780a0b1edf24347 |
| crate-review/41-wamn-schema-generator-part-10.md | 3590 | 698caf1a472f133d |
| crate-review/42-wamn-schema-generator-part-11.md | 5013 | 243a20428fc808d8 |
| crate-review/43-wamn-schema-generator-part-12.md | 5695 | b62870168f7474fc |
| crate-review/44-build-tooling-and-embedded-kcl.md | 4749 | bd5d6c9f9ee7851b |
| crate-review/45-wamn-schema-generator-part-13.md | 3886 | b09d8368bf998066 |
| crate-review/46-wamn-schema-generator-part-14.md | 3660 | d3712ab3b4d5f5a1 |
| crate-review/47-wamn-schema-generator-part-15.md | 4361 | 5d406027c582f418 |
| crate-review/48-wamn-schema-generator-part-16.md | 3444 | c10ac9ba968b3b23 |
| crate-review/49-wamn-schema-generator-part-17.md | 2633 | 3fe8f3fb45d2305a |
| crate-review/50-wamn-schema-generator-part-18.md | 4497 | b83f7be66fb64595 |
| crate-review/51-wamn-schema-generator-part-19.md | 5288 | d01e5de647fe0490 |
| crate-review/52-wamn-schema-generator-part-20.md | 5626 | 931bccc029d1b3e0 |
| crate-review/53-wamn-schema-generator-value-per-output.md | 13150 | f18e1254d80d2b17 |
| crate-review/54-identity-part-1.md | 7515 | ff9b9a10a3be97d5 |
| crate-review/55-identity-part-2.md | 8231 | 211ac4f5a5139b3c |
| crate-review/56-identity-part-3.md | 7642 | ac727ea8475aa700 |
| crate-review/57-identity-service.md | 6041 | 110519c4ea457f4a |
| crate-review/58-edge.md | 7051 | 10681723b22d8021 |
| crate-review/59-wamn-control-provision.md | 6994 | 917dc5cbee258943 |
| crate-review/60-wamn-package-control.md | 19673 | 834b70d9a9d99598 |
| crate-review/61-wamn-release-control.md | 5140 | b1fd079d6d8f7f3c |
| crate-review/62-wamn-environment-control-part-1.md | 26099 | 4bab834a33f0e1f0 |
| crate-review/63-wamn-environment-control-part-2.md | 8042 | bbf4ba364ab63cdc |
| crate-review/64-wamn-environment-control-part-3.md | 7699 | 2aaa01325215e689 |
| crate-review/65-wamn-environment-control-part-4.md | 7927 | efe027ed1482cc80 |
| crate-review/66-wamn-environment-control-part-5.md | 6979 | a2420988473c2953 |
| crate-review/67-wamn-environment-control-part-6.md | 3814 | e7f04f5c34715ae1 |
| crate-review/68-wamn-environment-control-part-7.md | 6349 | 752a1a121a48d64b |
| crate-review/69-wamn-dev-control-part-1.md | 5060 | bd981ca09b3f1fd6 |
| crate-review/70-wamn-dev-control-part-2.md | 4874 | d279d2e412aa93b6 |
| crate-review/71-wamn-ctl.md | 12412 | afbc7e9ad3cd7db1 |
| crate-review/72-wamn-host-service.md | 7309 | f711bc30c831b2c8 |
| crate-review/73-composer-and-virtualizer.md | 7677 | 0386e45b558e2c53 |
| crate-review/74-wamn-catalog-part-1.md | 6859 | 3f1379d97c3b9df2 |
| crate-review/75-wamn-catalog-part-2.md | 5415 | bdd00a59f22e49b5 |
| crate-review/76-wamn-catalog-part-3.md | 4233 | ac615abbf278a851 |
| crate-review/77-schema-control-introspection-package-artifact.md | 8429 | bb95c059a1524660 |
| crate-review/78-postgres-client-after-u2.md | 3972 | 45a36d078d6c64c0 |
| crate-review/79-materializer.md | 4889 | d54bac361eb96bc0 |
| crate-review/80-apps.md | 6134 | 3560435488f00536 |
| crate-review/81-scripts-and-tooling.md | 4347 | 10b4aa9870451e88 |
| crate-review/82-registry-hierarchy-order.md | 4367 | abae2fe81c55a605 |
| crate-review/83-cdc-reader.md | 4591 | 5ad519ded2aa6735 |
| crate-review/84-scenario-worker-and-authoring-model.md | 4630 | 95e719006c5cf820 |
| crate-review/85-client-core-and-run-state-sqlite.md | 3081 | 383bb44e2da56d59 |
| crate-review/86-test-trees.md | 16840 | 3b8c911843e8e8e4 |
| crate-review/87-guest-data-libraries-and-pg_walstream.md | 7719 | bcc58d53dd1e1c37 |
| crate-review/88-open-items-after-the-crate-review.md | 10399 | 159270de5de5eb42 |
| images/architecture-current.png | 109817 | 2402cabab21a951b |
| images/architecture-proposed.png | 134249 | c62a5a4f9d75d851 |
