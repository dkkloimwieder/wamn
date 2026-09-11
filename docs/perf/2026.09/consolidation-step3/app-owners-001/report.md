The first application test compile failed at source `086918f5`. The Receiving test crate omitted `wamn-schema-generator` and `wamn-schema-control` from its dependencies. No application test result counts as a pass. The later extraction commit adds both dependencies.

The separate platform command passed 9 cases and ignored 13 cluster cases. It preserved the generated test input schema bytes. The command took 70.301 seconds. The failed application command took 147.301 seconds.

`commands.json` contains both exact commands. `results.json` records their exits and durations. The raw logs retain compiler errors, warnings, test names, and ignored cases. No live fixture was armed. This is a preparation result, not the stage exit.
