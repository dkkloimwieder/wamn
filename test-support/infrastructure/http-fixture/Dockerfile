FROM rust:1.99-trixie AS build
RUN apt-get update && apt-get install -y --no-install-recommends clang mold protobuf-compiler libprotobuf-dev git && rm -rf /var/lib/apt/lists/*
WORKDIR /fixture
COPY . .
RUN cargo +1.99.0 build --locked -p wamn-test-infrastructure --bin http-fixture

FROM debian:trixie-slim
COPY --from=build /fixture/target/debug/http-fixture /http-fixture
USER 65534:65534
ENTRYPOINT ["/http-fixture"]
