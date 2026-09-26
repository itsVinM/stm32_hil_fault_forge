# faultforge — multi-stage Docker build
# Stage 1: Build host (x86_64 Linux)
FROM rust:1.96-slim AS host-builder

WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libudev-dev clang lld \
    && rm -rf /var/lib/apt/lists/*

COPY host/Cargo.toml host/Cargo.lock* ./host/
COPY shared/Cargo.toml shared/Cargo.lock* ./shared/
# Pre-fetch deps
RUN mkdir -p host/src && echo "fn main() {}" > host/src/main.rs \
    && mkdir -p shared/src && echo "// dummy" > shared/src/lib.rs \
    && cargo build --release --manifest-path host/Cargo.toml 2>/dev/null || true

# Copy real source
COPY host/src ./host/src
COPY shared/src ./shared/src
COPY campaign.yaml ./
RUN cargo build --release --manifest-path host/Cargo.toml

# Stage 2: Firmware build (ARM cross-compile)
FROM rust:1.96-slim AS firmware-builder

WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
    gcc-arm-none-eabi \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add thumbv7em-none-eabihf

COPY firmware/Cargo.toml firmware/build.rs firmware/memory.x ./firmware/
COPY firmware/.cargo ./firmware/.cargo
COPY shared/Cargo.toml ./shared/
# Pre-fetch deps
RUN mkdir -p firmware/src && echo "#![no_std]\n#[embassy_executor::main]\nasync fn main(_s: embassy_executor::Spawner) {}" > firmware/src/main.rs \
    && mkdir -p shared/src && echo "// dummy" > shared/src/lib.rs \
    && cargo build --release --manifest-path firmware/Cargo.toml 2>/dev/null || true

# Copy real source
COPY firmware/src ./firmware/src
COPY shared/src ./shared/src
RUN cargo build --release --manifest-path firmware/Cargo.toml

# Stage 3: Runtime image (host only, for --simulate or with USB passthrough)
FROM debian:bookworm-slim AS runtime

WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
    libudev1 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy host binary
COPY --from=host-builder /app/target/release/faultforge /usr/local/bin/faultforge

# Copy firmware artifact
COPY --from=firmware-builder /app/firmware/target/thumbv7em-none-eabihf/release/faultforge-firmware /app/firmware.elf

# Copy campaign config
COPY campaign.yaml /app/campaign.yaml

ENTRYPOINT ["faultforge"]
CMD ["run", "--config", "/app/campaign.yaml", "--simulate"]