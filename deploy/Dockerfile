FROM --platform=$BUILDPLATFORM rust:1.90-bookworm AS build
ARG TARGETARCH
ARG BUILDARCH
WORKDIR /src
RUN if [ "$TARGETARCH" != "$BUILDARCH" ]; then \
      apt-get update && apt-get install -y --no-install-recommends \
        $([ "$TARGETARCH" = "arm64" ] && echo "gcc-aarch64-linux-gnu libc6-dev-arm64-cross" || echo "gcc-x86-64-linux-gnu libc6-dev-amd64-cross") \
      && rm -rf /var/lib/apt/lists/*; \
      rustup target add $([ "$TARGETARCH" = "arm64" ] && echo "aarch64-unknown-linux-gnu" || echo "x86_64-unknown-linux-gnu"); \
    fi
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY packages packages
COPY apps/core apps/core
COPY apps/server apps/server
RUN --mount=type=cache,id=cargo-registry-${TARGETARCH},sharing=locked,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-git-${TARGETARCH},sharing=locked,target=/usr/local/cargo/git \
    if [ "$TARGETARCH" = "$BUILDARCH" ]; then \
      cargo build --locked --release -p fesen-server && \
      cp target/release/fesen-server /tmp/fesen-server; \
    elif [ "$TARGETARCH" = "arm64" ]; then \
      CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
      CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
      AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
      cargo build --locked --release -p fesen-server --target aarch64-unknown-linux-gnu && \
      cp target/aarch64-unknown-linux-gnu/release/fesen-server /tmp/fesen-server; \
    else \
      CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
      CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
      AR_x86_64_unknown_linux_gnu=x86_64-linux-gnu-ar \
      cargo build --locked --release -p fesen-server --target x86_64-unknown-linux-gnu && \
      cp target/x86_64-unknown-linux-gnu/release/fesen-server /tmp/fesen-server; \
    fi && \
    install -d -m 0700 -o 10001 -g 10001 /tmp/state

FROM gcr.io/distroless/cc-debian12
COPY --from=build --chown=10001:10001 /tmp/state /state
COPY --from=build /tmp/fesen-server /usr/local/bin/fesen-server
USER 10001:10001
ENV FESEN_DB=/state/fesen.sqlite FESEN_LISTEN=0.0.0.0:8080
ENTRYPOINT ["/usr/local/bin/fesen-server"]
CMD ["serve"]
