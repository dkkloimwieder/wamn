FROM rust:1.99-trixie AS build
WORKDIR /fixture
COPY server.rs .
RUN rustc --edition=2024 -C debuginfo=2 server.rs -o /http-fixture

FROM debian:trixie-slim
COPY --from=build /http-fixture /http-fixture
USER 65534:65534
ENTRYPOINT ["/http-fixture"]
