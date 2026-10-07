This directory contains the released pg_walstream 0.9.0 crate.
The upstream source commit is 0c2b01c6a4c0d676dd77c112737bd2210c4a517c.

The local patch carries the typed keepalive event from fork commit a0df8cee8727abee763f9715436bfc41d348b01e.
The raw event API still consumes keepalives internally.
The upstream release already fixes the FAILOVER slot syntax.

The local copy keeps this update usable without publishing a fork or changing the GCP deployment.

The local copy includes the public test certificate from the same upstream commit.
The released crate omits this file but its unit tests require it.
Imported files use normalized trailing whitespace.
