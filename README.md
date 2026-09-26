# STM32 HIL Fault Forge

An asynchronous Hardware-in-the-Loop (HIL) fault injection and telemetry framework built for low-level embedded security analysis and target resilience testing on the STM32F401RE.

The system couples a bare-metal `no_std` Rust firmware target with a high-throughput, Tokio-driven host orchestrator to execute automated fault campaigns, perform side-channel power analysis, and validate target recovery state in real time.

---

## System Architecture

```
┌──────────────────────────────────────────────────┐
│             Host PC (Orchestration)              │
│  ┌─────────────────┐       ┌──────────────────┐  │
│  │ Tokio Async CLI │ ◄───► │ Campaign Engine  │  │
│  └────────┬────────┘       └────────┬─────────┘  │
└───────────┼─────────────────────────┼────────────┘
            │ USB-UART Stream         │
            │ COBS Frame Protocol     │ Hard Triggers
            ▼                         ▼
┌──────────────────────────────────────────────────┐
│            STM32 Target Board (Target)           │
│  ┌─────────────────┐       ┌──────────────────┐  │
│  │ Embassy Async   │       │ Trigger / Power  │  │
│  │ Firmware Core   │       │ Capture (PA0/PB0)│  │
│  └─────────────────┘       └──────────────────┘  │
└──────────────────────────────────────────────────┘

```

---

## Core Features

### Firmware Engine (`firmware/`)

Runs bare-metal on the STM32F4 target leveraging **Embassy** (async Rust framework):

* **Hardened Execution Environment:** Configures the Memory Protection Unit (MPU) for strict region isolation alongside runtime stack-canary checks.
* **Side-Channel Analysis (SCA):** Synchronized high-speed ADC sampling on pin `PA0` captures power profiles during execution of cryptographic operations.
* **Deterministic Fault Injection:** Toggles low-latency digital signals (e.g., `PB0`) for microsecond-precise voltage glitches and crowbar triggers.
* **Security & Entropy Primitives:** Implements software/hardware AES operations and feeds a True Random Number Generator (TRNG) with continuous online health checking (Monobit tests).

### Host Orchestrator (`host/`)

An asynchronous, CLI-driven testing harness built on **Tokio**:

* **Automated Campaigns:** Executes structured test profiles—`clean`, `fuzz`, `emi`, `stress`, and `delay`—across hundreds of automated iterations.
* **Robust Packet Transport:** Uses framed binary streaming over serial with CRC verification to guarantee telemetry integrity under noisy hardware fault conditions.
* **Offline Emulation Mode:** Features a `--simulate` driver to validate campaign scripts, payload parsing, and logging pipelines without connected hardware.
* **Telemetry Export:** Generates structured CSV logs and statistical summaries for post-test resilience profiling.

---

## Repository Structure

```text
stm32_hil_fault_forge/
├── firmware/            # Bare-metal Embassy Rust target firmware
│   ├── src/             # MPU, TRNG, AES, SCA ADC routines, and triggers
│   └── Memory.x         # Linker script for memory layout and canary placement
├── host/                # Async Tokio CLI application
│   └── src/             # Campaign engines, serial framing, and CSV reporting
└── README.md

```

---

## Getting Started

### Hardware Prerequisites

* **Target:** STM32F401RE NUCLEO-64 board
* **Debug Probe:** On-board ST-LINK/V2-1 or external `probe-rs` compatible tool
* **Connections:** PA0 (Power Trace ADC), PB0 (Fault Injection Trigger), USB-UART Serial interface

### Software Toolchain

* **Target Triple:** `thumbv7em-none-eabihf`
* **Flashing Utility:** `probe-rs-tools` (`cargo-embed` or `probe-rs run`)

```bash
rustup target add thumbv7em-none-eabihf
cargo install probe-rs-tools

```

---

## Quickstart

### 1. Build and Flash Firmware

Connect the STM32 board and flash the target binary:

```bash
cd firmware
cargo run --release

```

### 2. Launch Host Campaign

Execute an automated stress campaign over the board's serial port:

```bash
cd host
cargo run -- \
  --port /dev/ttyACM0 \
  --baud 115200 \
  --profile stress \
  --scenarios 500 \
  --output campaign_report.csv

```

### 3. Run Simulation Mode (No Hardware Required)

Test host-side processing logic locally:

```bash
cd host
cargo run -- --simulate --profile fuzz --scenarios 100

```