# Release vs debug guest artifacts: strip removes symbols, not imports

## Sizes (virtualized, as served)

artifact                            debug      release   factor
receiving.wasm                   20634368       669593    30.8x
client_acme_receiving.wasm       10019001       466014    21.5x
blob_put.wasm                     6812184       325357    20.9x

## Import sets, compared with wasm-tools component wit

### receiving (6 imports, debug and release IDENTICAL)
import wamn:node/types@0.1.0;
import wamn:postgres/statements@0.1.0;
import wamn:postgres/types@0.1.0;
import wasi:clocks/monotonic-clock@0.2.12;
import wasi:clocks/wall-clock@0.2.12;
import wasi:io/poll@0.2.12;

### client_acme_receiving (7 imports, debug and release IDENTICAL)
import wamn:node/types@0.1.0;
import wamn:postgres/statements@0.1.0;
import wamn:postgres/types@0.1.0;
import wamn-receiving:receiving/record-receipt@1.0.0;
import wasi:clocks/monotonic-clock@0.2.12;
import wasi:clocks/wall-clock@0.2.12;
import wasi:io/poll@0.2.12;

### blob_put (7 imports, debug and release IDENTICAL)
import wamn:node/types@0.1.0;
import wasi:clocks/monotonic-clock@0.2.12;
import wasi:clocks/wall-clock@0.2.12;
import wasi:io/poll@0.2.12;
import wasmcloud:blobstore/blobstore@0.1.0;
import wasmcloud:blobstore/container@0.1.0;
import wasmcloud:blobstore/types@0.1.0;

