The mechanical move puts app sources under `apps/<package>/` and shared guests under `apps/platform/`.
The native workspace stays at the repository root.
The standard guest workspace moves to `apps/Cargo.toml`, and no_std guests keep their separate workspace.
This move belongs to `wamn-47wm.3.6`, `wamn-47wm.3.2`, `wamn-47wm.3.3`, and `wamn-47wm.3.4`.

The [metadata comparison](app-home-metadata-002/results.json) matches the original tree after path mapping.
Workspace membership remains at 40 native, 20 standard guest, and 4 no_std packages.
All local guest dependencies stay inside their owning workspace.

The builds used source base `411ab2c4`.
The [source record](guest-bytes-001/source.json.gz) identifies frozen patch `13271dbc676fd2c02ef0009b453197dd9bc2091cca41c6e56e8c108ec675bff9` and index tree `1c653a1fc68d59c66f9021cab862d13e97007e4b`.
Two independent checkouts used separate fresh targets.
Their source bytes and modes stayed unchanged during the builds.

The [byte comparisons](guest-bytes-001/comparison-result.json) passed for all 14 raw Wasm outputs, including 11 standard and 3 no_std guests.
All 4 normalized outputs also matched across checkouts.
All 3 app outputs matched between app and full workspace selections in both checkouts.
Each of the [54 Cargo build calls](guest-bytes-001/build-results.json) selected one package.
All [3 retained test runs](guest-bytes-001/native-test-results.json) passed, with zero failures or skips.

Moving the workspace and source files changes Cargo package paths and the remapped source paths embedded in guest bytes.
The Receiving pin therefore uses the final layout.
Its old value was `sha256:a092149c1c8df8b4f74babf64122f9747d15d7b1a78b82b35cb6e496f98fd9a1`.
Its new value is `sha256:b85757dc167ad385804fbc6e670a47ca0c66bc272860142b6faed11f2e93c436`.

A fresh PostgreSQL 18.6 database supported generation before and after the pin update.
The [pin update result](pin-remint-001/run-001/result.json) records successful generation and completed container removal.
The update changed the authored Acme manifest and 4 generated JSON files.
All 77 generated Rust files kept the same bytes, as the [file results](pin-remint-001/run-001/verification.json) record.

The full integrated stage 2 test run remains pending until launcher retirement is complete.
Live native C correctness, authority, and pressure tests remain with `wamn-0ct2.7`.

Large raw records use gzip archives without changing their contents.
The [archive map](measurement-archives.json) records the original filenames, sizes, and hashes.
