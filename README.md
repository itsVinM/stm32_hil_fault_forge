# STM32 HIL Fault Forge

Hardware-in-the-Loop (HIL) fault injection for embedded resilience testing on an
STM32F401RE target. A `no_std` Embassy firmware injector walks a `probe_gpio`
SPI frame streamer and deliberately corrupts frames over UART; a Tokio-driven
host orchestrator runs the stream, records ground truth, and scores how well the
DUT detects every injected fault.

```
┌────────────────────────────────────────┐
│ Host PC (Tokio CLI)                   │
│  campaign.yaml ─► injector/simulator  │
│  DUT: sensor stream → detection logic │
│  score + CSV report in faultforge_out │
└──────────────────┬─────────────────────┘
                   │ USART2 (ST-LINK VCP, 115200 8N1)
                   │ framing: [0x7E] [len] [kind] [payload] [crc8]
┌──────────────────▼─────────────────────┐
│ STM32F401RE  (Embassy, thumbv7em-none) │
│  probe_gpio: SPI frame streamer +      │
│  fault injector (bitflip/byte/drop/    │
│  delay/replay/burst/duty)              │
└────────────────────────────────────────┘
```

## Wire protocol (`shared/`)

Both directions share one frame format: `0x7E` sync, `len`, `kind`, payload,
CRC-8/ATM (poly 0x07). Garbage resyncs the parser, so corrupted bytes are
observable — the host DUT relies on that to spot injections.

* Host → FW: `start` (seed, packets, cadence, weights), `ping`, `abort`, `glitch`.
* FW → Host: `sensor` (seq + value + fault_id), `fault` (ground-truth
  notice: fault_id, kind, at_seq), `end` (packets, faults injected), `pong`.

Firmware and the host simulator share an LCG PRNG, so a campaign is fully
reproducible from its seed.

## Firmware (`firmware/`)

Embassy async runtime on the STM32F401RE. The **wired** path is `main.rs`:
USART2 framing (`probe_gpio` SPI frame loader) plus the fault injector
(bit-flips, byte corruption, drops, delays, replays, bursts, and duty-cycle
glitches). Profiles pick the fault-weight vector.

Kept but **not yet wired** (candidates for follow-up work):
`mpu.rs` (MPU region isolation), `canary.rs` (stack canary), `digital.rs`
(timer-driven pulses on `PA6/8/9/10/11`), `analog.rs` (ADC1 telemetry loopback
under Host ADC overvoltage fault injection). `shared/src/probe.rs` /
`probe_gpio.rs` form the dormant probe subsystem.

## Host (`host/`)

Tokio CLI binary `faultforge`. Loads `campaign.yaml` (seed, packets, cadence,
profile, weights, port), connects to the serial port (auto-detected unless the
config sets one, `"simulate"` runs the injector in-process), then runs the
campaign: it fires the injector over `start`, feeds sensor frames through the
DUT, pairs each `fault` notice with the DUT's detections (greedy, time-ordered,
kind-aware), and reports detection rate / latency plus CSV rows.

Profiles: `clean`, `fuzz`, `emi`, `stress`, `delay` (per-kind weight vectors in
`crate::campaign`).

```bash
# offline validation, no hardware
cargo run -p faultforge-host -- --simulate --profile fuzz --packets 500

# on-target run
cargo run -p faultforge-host -- --config campaign.yaml --profile stress
```

`--help` lists all options (`--seed`, `--packets`, `--cadence-ms`, `--profile`,
`--report`, `--quiet`, `--list-ports`, `--simulate`).

## Building

```bash
rustup target add thumbv7em-none-eabihf

# firmware (needs ST-LINK via probe-rs)
cargo build -p faultforge-firmware --release

# host
cargo build -p faultforge-host --release
cargo test --workspace
```

Docker build (host + firmware cross-compiled) is in `Dockerfile`;
`init-docker.sh` wraps build/run/flash. `.gitignore` covers `target/`,
`firmware/target/`, `Cargo.lock`, `*.elf`, and `faultforge_out/`.