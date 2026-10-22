FROM rust:1.99-trixie AS build
WORKDIR /fixture
COPY Cargo.toml Cargo.lock server.rs netsuite.rs ./
RUN cargo build --locked

FROM debian:trixie-slim
COPY --from=build /fixture/target/debug/http-fixture /http-fixture
USER 65534:65534
ENTRYPOINT ["/http-fixture"]
