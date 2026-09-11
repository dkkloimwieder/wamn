The six extracted platform cases pass at source `3b858fbe`. The command takes 2.945 seconds and ignores no cases. It filters the 38 earlier infrastructure cases, which already pass in `../app-owners-002/command-1.log`.

These cases cover operator identity, environment-scoped event permissions, certificate rendering, and the CA copy. The CA copy changes only metadata. The source hashes remain unchanged during the command.

The shared installation function retains the existing readiness limits and takes an explicit lifecycle entrypoint. The app runners still need to call it. No cluster or service command ran. `extraction-source.json` records the original shell blocks and their assertions.
