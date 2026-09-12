# WMS startup result

The corrected WMS startup case passed at `fb6c2b2e28fbcb90a8a373672a60e2397488cae5`.
Its [final result](result.json) records completed assertions, unchanged source, successful resource cleanup, and removal of the private directory.

Cold, restarted, and steady requests each returned HTTP 200 with the expected pallet row.
The cold request passed on its first attempt in 112.405 milliseconds.
The restarted request used 75 attempts and recovered within 75 seconds, below the retained 120-second limit.
Its final request took 153.219 milliseconds.
The steady request passed on its first attempt in 17.189 milliseconds.
The request records retain every response and retry result.

Both request traces passed the retained statement and component-loading assertions.
The [restart trace](trace-breakdown-restart-first.json) recorded 2.048867 milliseconds of SQL time.
The [steady trace](trace-breakdown-steady.json) recorded 0.693356 milliseconds of SQL time.
Actual HTTP, request, authentication, acquisition, and SQL timings remain available.

The host kept the same Pod identity and passed the native readiness assertions after one restart.
The [startup result](runtime-startup.json) records 12,654 milliseconds for cold startup and 701 milliseconds after restart.
All four compiled cache entries retained their hashes, paths, sizes, timestamps, and inode numbers.
The cold and warm cache records match byte for byte.

The owner approved removal of the obsolete overhead ratio in `63345829d95063549d044088fa5b6090267d12bb`.
Native B removed the sole instantiation span that supplied part of its denominator.
The [source review](../wms-startup-review-001/README.md) and [earlier failed run](../wms-startup-live-001/README.md) remain unchanged.
This run makes no replacement overhead or comparative performance claim.

The [controller records](../wms-startup-cargo-002/README.md) include exact commands, build results, binary hashes, source comparisons, and independent cleanup observations.
