# Guest build invocations

`wamn-47wm.3.7` splits the Docker and palette build commands into one Cargo invocation per guest.
The selected packages, toolchains, build arguments, and publication arguments stay the same.
The guest digest finding requires this invocation shape for every caller.

The test replaces Cargo with an argument recorder and exercises both command paths.
Both original commands fail the one-package assertion.
Both fixed commands pass it.
Docker selects five guests in five invocations, and the palette publisher selects three guests in three invocations.
The [result](result.json) contains every argument list and source hash.
The test does not compile or compare guest artifacts.
The workspace move owns the two-checkout and app/proof artifact comparisons under `wamn-47wm.3.6`.
The retained workspace sweep runs at the end of step 2 under `wamn-47wm.3`.
