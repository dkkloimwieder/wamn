The second extraction test run used source `8b9025fa`. All 38 rendering and helper tests passed in 4.607 seconds. The tests include the database endpoint correction from `60e5e7cd`.

The application compile failed with five errors because the Receiving module lacked the existing `OPERATION` constant. The separate platform compile failed because its retained fixture lacked `BASE_COMPONENT`. These are extraction defects. The application command took 92.955 seconds. The platform command took 7.487 seconds. No application or platform test ran in either failed command.

`commands.json` contains the exact commands. `results.json` records each exit and duration. The raw logs retain all six compiler errors. The source hashes stayed unchanged throughout the run. No live fixture was armed. Both compile failures require a correction and another run. This is a preparation result, not the stage exit.
