# faultforge — multi-stage Docker build
#
# Stage 1: the bench sender
# Stage 2: the firmware (ARM cross-compile)
# Stage 3: runtime image (sender binary + prebuilt firmware.elf + probe-rs)
FROM rust:1.96-slim AS host-builder

WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libudev-dev clang lld \
    && rm -rf /var/lib/apt/lists/*

# Pre-fetch deps with a stub source so the registry layer caches.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release 2>/dev/null || true

# Real sources. firmware/src is needed here too: the host compiles
# firmware/src/protocol.rs into its binary via #[path], so that file is a build
# input for the sender as well as for the firmware.
COPY src ./src
COPY firmware/src ./firmware/src
RUN cargo build --release --locked && cargo test --locked

# ---------------------------------------------------------------------------
FROM rust:1.96-slim AS firmware-builder

WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
    gcc-arm-none-eabi \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add thumbv7em-none-eabihf \
    && cargo install flip-link --locked

COPY firmware/Cargo.toml firmware/Cargo.lock firmware/build.rs firmware/memory.x ./firmware/
COPY firmware/.cargo ./firmware/.cargo
RUN mkdir -p firmware/src \
    && echo '#![no_std]' > firmware/src/main.rs

# The protocol and probe suites are testable on the host, so run them here
# rather than trusting a cross-build alone.
WORKDIR /app/firmware
RUN cargo test --locked --lib

# Every cargo invocation below must run with the CWD inside firmware/. Cargo
# discovers .cargo/config.toml by walking up from the *current directory*, not
# from --manifest-path, and that file is what supplies the thumbv7em target, the
# flip-link linker, and the -Tlink.x/-Tdefmt.x linker args. Building from /app
# with --manifest-path silently cross-compiles for the host instead.
WORKDIR /app/firmware
RUN cargo build --release 2>/dev/null || true

COPY src ./src
RUN cargo build --release --locked

# ---------------------------------------------------------------------------
FROM rust:1.96-slim AS runtime

WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
    libudev1 ca-certificates gcc-arm-none-eabi \
    && rm -rf /var/lib/apt/lists/* \
    && cargo install probe-rs --locked

COPY --from=host-builder /app/target/release/faultforge /usr/local/bin/faultforge
COPY --from=firmware-builder /app/firmware/target/thumbv7em-none-eabihf/release/faultforge-firmware /app/firmware.elf

ENTRYPOINT ["faultforge"]
CMD ["--help"]
