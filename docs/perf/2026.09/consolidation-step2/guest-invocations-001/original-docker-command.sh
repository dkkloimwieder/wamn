cargo +1.97.0 build --locked --release --target wasm32-wasip2 \
      -p http-route \
 && cargo +1.97.0 build --locked --release --target wasm32-wasip2 \
      -p materializer -p busyloop -p connection-http-standard -p sockprobe \
 && install -d /component-output \
 && for artifact in \
      http_route materializer \
      busyloop connection_http_standard sockprobe; do \
      install -m 0644 "target/wasm32-wasip2/release/${artifact}.wasm" \
        "/component-output/${artifact}.wasm"; \
    done
