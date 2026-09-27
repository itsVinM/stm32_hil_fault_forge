# STM32 HIL Fault Forge

Hardware-in-the-Loop fault injection for embedded resilience testing, targeting
an STM32F401RE (Nucleo-F401RE).

The instrument is the firmware. A `no_std` Embassy binary owns fault selection,
injection, the sensor stream, and the ground-truth notices. The host is two small
Rust files whose entire job is to send one control frame over USB serial and keep
the injector's UART drained.

```
┌──────────────────────────────────────┐
│ Host — src/main.rs (Tokio, 1 file) │
│   START frame  ──────────────────┐   │
│   drain thread ◀─── byte count    │   │
└───────────────────────────────────┼───┘
                    USART2 / ST-LINK VCP, 115200 8N1
                    [0x7E] [len] [kind] [payload] [crc8]
┌───────────────────────────────────▼───┐
│ STM32F401RE — firmware/ (Embassy)    │
│   rx: START / ABORT → Signal         │
│   tx: campaign engine, cadence loop  │
│   lib: protocol + bit-bang exercisers│
└───────────────────────────────────────┘
```

## Layout

Two packages, two workspaces, and **no shared crate**. The split is driven by the
target: the sender is native, the firmware is `thumbv7em-none-eabihf` and pulls
git-sourced Embassy, so it keeps its own workspace.

| path | package | why it exists |
|---|---|---|
| `src/main.rs` | `faultforge` | the bench sender: serial I/O, drain thread. |
| `firmware/src/protocol.rs` | `faultforge-firmware` | the wire format. **Shared by source, not by crate.** |
| `firmware/src/probe.rs` | `faultforge-firmware` | bit-bang SPI / I2C / UART exercisers. |
| `firmware/src/main.rs` | `faultforge-firmware` | the injector: Embassy entry point. |

Line counts, for perspective: the host is one 236-line file and two dependencies,
`tokio` and `serialport`.

### Why there is no `shared/` crate

A crate named "shared" is only justified when code is genuinely consumed by both
sides. Here neither candidate is:

* **The protocol is owned by the firmware.** It is the instrument; the host is a
  remote control for it. So the format is defined once, in
  `firmware/src/protocol.rs`.
* **The host only ever *writes* frames.** It sends `START` or `ABORT` and then
  counts bytes. It does not parse, decode, or score anything. So it needs only
  the encoder — but it still needs the *same* encoder, not a second copy.

The host therefore compiles `firmware/src/protocol.rs` straight into its own
binary:

```rust
#[allow(dead_code)]
#[path = "../firmware/src/protocol.rs"]
mod protocol;
```

One definition, two crates, no dependency between them. The file has no
`#![no_std]` of its own, so it compiles as a module of the firmware's `no_std`
library and of the host's `std` binary alike.

This is the fix for the one real cost of dropping `shared/`. An earlier revision
hand-copied ~50 lines of the encoder into `src/encode.rs`, which meant a change
to `encode_start`'s payload layout had to be made twice and would drift silently.
`#[path]` removes that risk. The only cost is that `src/main.rs` reaches outside
its own package directory, which rules out `cargo package` — irrelevant for a
bench tool, and the reason it is called out here.

A shared *crate* still would not work, and it is worth recording why. Cargo
*does* allow a package to depend on its parent directory (verified, not assumed),
but **Cargo resolves dependencies per package, not per target**. So a root package
carrying `tokio` + `serialport` would make the bare-metal firmware inherit both,
and the cross-build dies with `error[E0463]: can't find crate for std`. Gating
them behind an optional feature works around it at the cost of a feature flag on
every build — more machinery than the crate it removes. `#[path]` shares the
*source* without sharing the *dependency graph*, which is the part that was
actually breaking.

What `shared/` used to be is instructive: 44% of it (the bus exercisers) had
exactly one consumer, the firmware, and existed only to make the host compile
`no_std` GPIO code it never called. That part moved to `firmware/src/probe.rs`.
The rest was a contract, and a contract with one owner does not need a crate.

**The cost of this choice, stated plainly:** the host's frame encoder is a second
implementation, so a change to `encode_start`'s payload layout must be made in two
files. In exchange, the firmware gains a testable library target and the crate
count drops by one. If you ever want frame-level telemetry (frame / bad-crc /
garbage tallies) on the bench, you need `FrameParser` on the host — that is the
moment the duplication stops being worth it and `shared/` should come back.

## Wire protocol

One frame format in both directions: `0x7E` sync, `len`, `kind`, payload, and
CRC-8/ATM (poly `0x07`) over `[len, kind, payload]`. Garbage resyncs the parser, so
corrupted bytes stay visible on the wire instead of desynchronising the stream.

* Host → FW: `start` (seed, packets, cadence, weights), `abort`.
* FW → Host: `sensor` (seq + value + fault_id), `fault` (ground truth: fault_id,
  kind, at_seq), `end` (packets, faults injected).

Firmware and host share an LCG PRNG, so a campaign is reproducible from its seed.

One protocol kind is half-wired: `ping`/`pong`. The firmware's `rx_task` parses
`K_PING` and logs it without replying, so `encode_pong` has no caller. The
`glitch` kind was removed — it had no encoder, no handler and no test, and
`campaign.yaml`, which used to weight it, is gone.

## Firmware (`firmware/`)

Embassy on the STM32F401RE at 84 MHz (HSI → PLL). USART2 on PA2/PA3 is the ST-LINK
VCP. `rx_task` parses control frames into `Signal`s; `tx_task` runs the campaign
and emits faulted sensor frames interleaved with ground-truth notices. Profiles
map to a per-kind fault weight vector, picked by the LCG.

`src/lib.rs` exists purely so the logic is testable. The Embassy entry point stays
in `src/main.rs`, but a `#![no_main]` binary for a bare-metal target has no test
harness, so anything left in `main.rs` is untestable by construction. `protocol`
and `probe` are therefore in a library target with the bare-metal dependencies
gated on `cfg(target_os = "none")` — that gate is what lets the tests build
natively instead of dragging `embassy-stm32` into a host build.

Not yet wired, kept as follow-up work:

* `mpu.rs` — MPU region isolation. Genuinely baremetal: neither `cortex-m` nor
  Embassy exposes an MPU driver. Must be called before `embassy_stm32::init`.
  Note the current region 3 marks the top 1 KB of RAM `NO_ACCESS`, which is
  exactly where the stack lives — the guard needs to sit below it.
* `canary.rs` — stack canary. Redundant if the MPU guard is correct, and the
  current `0x2001_7FFC` address is the word the reset handler pushes `LR` into.
* `digital.rs` — timer-driven pulses on PA6/8/9/10/11.
* `analog.rs` — ADC1 telemetry loopback. Currently unbuildable: it imports
  `shared::{SamplePacket, ChannelId, DmaBuf}` and `crate::transport::Transport`,
  all removed in the `31c9426` dead-code sweep.

Those four are not declared as modules, so they do not compile today.

Note that `canary.rs` and `mpu.rs` currently *contradict* each other: the canary
lives at `0x2001_7FFC`, which is inside the MPU's stack-guard region
(`0x2001_7C00`–`0x2001_8000`). Enabling the MPU as written would fault the
canary's own first write. One of them has to move, and if the MPU guard is done
correctly the canary is redundant.

## Traits and lifetimes

The honest answer is that traits are already used where they pay, and lifetimes
would not pay at all here.

**Traits that earn their place:**

* `probe::GpioOps` — has two implementations: `Stm32Gpio` (registers) in
  `probe_gpio.rs` and `RecordingBus` (a mock) in the tests. That mock is the
  entire reason the bus exercisers are testable on a host with no hardware.
  Collapsing the trait into a concrete type would cost 3 passing tests.
* `Box<dyn SerialPort>` — `try_clone()` needs two independent handles to the same
  fd (one writer, one drain thread), which a concrete type cannot give you.

**A trait that would be new and useful:** a `Transport` abstraction over
`write_frame(&mut self, kind: u8, payload: &[u8])`, which would let the firmware's
campaign engine be unit-tested on the host against a `Vec<u8>` sink. The engine
is currently a 70-line `match` inline in `main.rs`, so it is untestable by
construction. This is the single highest-value abstraction left in the codebase —
and the one place where a `&mut self` plus a lifetime on the payload would be
doing real work rather than decorating owned data.

**Lifetimes: not applicable.** Nothing here is borrowed across a boundary that
owns nothing else. Frames are `([u8; MAX_FRAME], usize)` — fixed-size arrays,
deliberately allocation-free and lifetime-free, which is what `no_std` embedded
wants. The port handles are `Box<dyn SerialPort>`, i.e. `'static`. The one place a
lifetime might have helped was lending drained chunks as `&[u8]` into the async
side instead of allocating a `Vec` per chunk, but that plumbing has since been
removed (see below). Adding lifetimes now would be decoration, not design.

## Host (`src/`)

```
faultforge [OPTIONS]

  --port <path>     Serial port (default: auto-detect ttyUSB/ttyACM/usbmodem/cu.*)
  --list-ports      List serial ports and exit
  --seed <n>        Campaign RNG seed (default: 42)
  --packets <n>     Packets the firmware should emit (default: 1500)
  --cadence-ms <n>  Packet cadence in ms (default: 5)
  --profile <name>  clean | fuzz | emi | stress | delay (default: fuzz)
  --hold-ms <n>     Override the drain window (default: packets x cadence + 1000)
  --abort           Send ABORT to a running campaign instead of START
```

The drain is not optional. The firmware writes with `blocking_write`, so if the
host stops reading, a full kernel buffer turns into a hard block inside the
firmware's campaign loop. The default drain window is derived from the campaign's
own duration (`packets x cadence`) so the final `end` frame lands before the host
stops listening.

The drain runs on a plain thread that increments one `AtomicU64`. It used to feed
a bounded `mpsc` channel consumed by a `tally` task, which meant a heap
allocation per chunk and a channel hop to compute a sum of lengths; both are gone,
along with the `tokio` `sync` feature. Tokio is still doing the real work here: the
multi-thread runtime, `spawn_blocking` for the blocking port write, and the drain
window `sleep`.

Fault selection, injection and detection all happen on the target. The host does
no scoring and writes no files.

## Building

```bash
rustup target add thumbv7em-none-eabihf
cargo install flip-link          # required by firmware/.cargo/config.toml

# host sender (native)
cargo build --release --locked
cargo test --locked
cargo clippy --locked --all-targets

# firmware — must be run from inside firmware/, see below
cd firmware && cargo build --release --locked
```

`cd firmware` is not optional. Cargo finds `.cargo/config.toml` by walking up
from the *current working directory*, not from `--manifest-path`, and
`firmware/.cargo/config.toml` is what supplies the `thumbv7em-none-eabihf`
target, the `flip-link` linker, and the `-Tlink.x` / `-Tdefmt.x` linker args.
Running `cargo build --manifest-path firmware/Cargo.toml` from the repo root
therefore cross-compiles for your *host* instead, and dies with a confusing
`` `#[panic_handler]` function required, but not found `` — no `--target` ever
reaches rustc.

### Firmware tests

The protocol and probe suites run natively, but `[build] target` in
`firmware/.cargo/config.toml` applies to them too, so the host triple has to be
passed explicitly:

```bash
cd firmware
cargo test --lib --target "$(rustc -vV | sed -n 's/^host: //p')"
cargo clippy --release
```

Note what does *not* work: `cargo test`, `cargo clippy --all-targets` and
`--tests`. They all include the `main.rs` test target, which needs the
bare-metal-only Embassy dependencies and so cannot be built for a native target.
This is inherent to a crate whose binary is hardware-only, not a misconfiguration.

Both `Cargo.lock` files are committed. The firmware one matters: `firmware/Cargo.toml`
depends on `embassy-*` over `git =` with no rev pin, so without a lockfile every
clean build floats to whatever Embassy HEAD happens to be.

## Docker

`Dockerfile` is a three-stage build (sender → firmware → runtime image with
`firmware.elf` alongside the binary). It installs `flip-link` and `probe-rs`, and
builds the firmware with the working directory inside `firmware/` for the reason
above. Both test suites run as build stages, so a broken protocol fails the image
build rather than a later flash. `init-docker.sh` wraps it:

```bash
./init-docker.sh build
./init-docker.sh ports
SERIAL_DEVICE=/dev/cu.usbmodem* ./init-docker.sh run --profile stress
./init-docker.sh flash    # probe-rs, needs --privileged
```
