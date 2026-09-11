# App Cargo workspace focused tests

All five commands passed on `c4ef9db4a9214e44a18aefc0b69d520952b52cb0`: 47 passing executions cover 40 distinct tests, with no failures or skips.
The retained `ui::tests::` substring filter repeats the seven native UI cases.

| Command selection | Passed | Seconds |
| --- | ---: | ---: |
| Generator `tui_emitter` | 15 | 4.410 |
| Generator `materialize::tests::` | 2 | 4.246 |
| Control `dev::native_tui::tests::` | 7 | 26.225 |
| Control `ui::tests::` | 22 | 1.648 |
| Control watch semantic owner | 1 | 0.636 |

The cases cover Cargo target selection for Receiving, Acme, and WMS. An independent app workspace builds and launches its small UI fixture and supplies its watch inputs.
Generation, inherited dependency selection, and existing generated-file equality also pass.
This batch does not execute a deployed WMS operator or the full integrated workspace run.

The head and all 26,613 tracked files outside Beads retain identical bytes and modes.
Cargo and the source freeze are released.
See [the exact commands](commands.json), [all case results](results.json), [the environment](environment.json), and [the source comparison](source-stability.json). Each command record names its full stdout and stderr logs.
The complete source maps use gzip archives. The [archive record](mapping-archives.json) records each original and compressed digest.
