The focused Rust tests pass at a0eb680d.

The run reports 99 passes: 55 shared infrastructure tests, 3 event declaration tests, 26 control tests, 13 runtime tests, and 2 CDC tests. No test fails. The ordinary control run ignores the schema generation command, which passes separately in activation-config-002. The runtime filter excludes its live broker case, which still requires a separate run.

All source bytes and file modes remain unchanged. These results do not replace the complete app cluster runs or the retained workspace sweep.
