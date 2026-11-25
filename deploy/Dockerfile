FROM rust:1.90-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY packages packages
COPY apps/core apps/core
COPY apps/server apps/server
RUN cargo build --locked --release -p fesen-server

FROM debian:bookworm-slim
RUN install -d -m 0700 -o 10001 -g 10001 /state
COPY --from=build /src/target/release/fesen-server /usr/local/bin/
USER 10001:10001
ENV FESEN_DB=/state/fesen.sqlite FESEN_LISTEN=0.0.0.0:8080
ENTRYPOINT ["/usr/local/bin/fesen-server"]
CMD ["serve"]
