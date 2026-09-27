//! A thin Tokio sender for the faultforge firmware.
//!
//! The firmware owns the whole instrument — fault selection, injection, the
//! sensor stream, and the ground-truth notices. This binary only does two
//! things: write one control frame (START or ABORT) to the ST-LINK VCP, and
//! keep draining the injector's reply so its blocking UART writes never stall.
//!
//! The drain is not optional: at 115200 baud the firmware emits frames faster
//! than the USB-serial bridge can absorb them, and a full kernel buffer turns
//! into a hard block inside `tx.blocking_write`.

/// The firmware's wire format, compiled directly into this binary.
///
/// `#[path]` rather than a shared crate: the protocol must not be a separate
/// package, because a package's dependencies are per-package, and a crate
/// holding the protocol would drag `tokio` + `serialport` into the bare-metal
/// firmware's dependency graph (which then fails to cross-compile with
/// `E0463: can't find crate for std`).
///
/// The host legitimately uses only a slice of this module — the encoder for the
/// two frames it sends. The rest exists for the firmware, so unused-item warnings
/// are expected and silenced here rather than with `#[allow(dead_code)]` scattered
/// through the protocol itself.
#[allow(dead_code)]
#[path = "../firmware/src/protocol.rs"]
mod protocol;

use std::io::{ErrorKind, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;

use protocol::{encode_abort, encode_start, CampaignConfig, N_FAULT_KINDS};
use serialport::SerialPort;

const BAUD: u32 = 115_200;
const READ_BUF: usize = 1024;

/// `serialport` hands back a type-erased handle; we need two (writer + reader).
type Port = Box<dyn SerialPort>;

const USAGE: &str = "\
faultforge — drive the faultforge firmware injector over USB serial

USAGE:
  faultforge [OPTIONS]

Sends one control frame, then drains the injector's output for the
length of the campaign (packets x cadence) plus a 1s margin.

OPTIONS:
  --port <path>     Serial port (default: auto-detect ttyUSB/ttyACM/usbmodem/cu.*)
  --list-ports      List serial ports and exit
  --seed <n>        Campaign RNG seed (default: 42)
  --packets <n>     Packets the firmware should emit (default: 1500)
  --cadence-ms <n>  Packet cadence in ms (default: 5)
  --profile <name>  clean | fuzz | emi | stress | delay (default: fuzz)
  --hold-ms <n>     Override the drain window (default: packets x cadence + 1000)
  --abort           Send ABORT to a running campaign instead of START
";

/// Per-profile fault weight vectors, indexed `FAULT_BITFLIP..=FAULT_DUTY`.
fn profile_weights(name: &str) -> [u8; N_FAULT_KINDS] {
    match name {
        "clean" => [0; N_FAULT_KINDS],
        "fuzz" => [8, 8, 8, 8, 8, 8, 0],
        "emi" => [30, 15, 0, 5, 10, 25, 0],
        "stress" => [5, 5, 40, 30, 15, 5, 0],
        "delay" => [0, 0, 0, 100, 0, 0, 0],
        other => {
            eprintln!("unknown profile '{other}', using fuzz");
            [8, 8, 8, 8, 8, 8, 0]
        }
    }
}

fn arg_val(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn num<T: std::str::FromStr>(args: &[String], flag: &str, default: T) -> T {
    arg_val(args, flag)
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn list_ports() {
    match serialport::available_ports() {
        Ok(ports) => {
            for p in ports {
                println!("  {}", p.port_name);
            }
        }
        Err(e) => eprintln!("could not enumerate ports: {e}"),
    }
}

/// Favour the ST-LINK VCP naming conventions over built-in serial devices.
fn find_port() -> Option<String> {
    serialport::available_ports()
        .ok()?
        .iter()
        .map(|p| p.port_name.clone())
        .find(|n| {
            n.contains("ttyUSB")
                || n.contains("ttyACM")
                || n.contains("usbmodem")
                || n.contains("cu.")
        })
}

/// Open the port for writing, plus a clone for the drain thread.
fn open_port(path: &str) -> Result<(Port, Port), String> {
    let port = serialport::new(path, BAUD)
        .timeout(Duration::from_millis(50))
        .open()
        .map_err(|e| format!("could not open {path}: {e}"))?;
    let reader = port
        .try_clone()
        .map_err(|e| format!("could not clone {path}: {e}"))?;
    Ok((port, reader))
}

/// Drain the port on a blocking thread, counting bytes as they go.
///
/// `serialport` is a blocking API, so the read stays on a thread. It talks to the
/// async side through one `AtomicU64` rather than a channel: the only thing the
/// async side wants is a total, so a bounded channel plus a task plus a `Vec`
/// allocation per chunk would be three moving parts to compute a sum of lengths.
/// A `dyn SerialPort` handle is `'static`, so it can cross the thread boundary.
fn spawn_drain(port: Port, bytes: Arc<AtomicU64>) {
    std::thread::spawn(move || {
        let mut port = port;
        let mut buf = [0u8; READ_BUF];
        loop {
            match port.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    bytes.fetch_add(n as u64, Relaxed);
                }
                // The 50ms read timeout is a liveness check, not an error.
                Err(e) if e.kind() == ErrorKind::TimedOut => continue,
                Err(_) => break, // unplugged or reconfigured
            }
        }
    });
}

async fn drive(path: String, frame: Vec<u8>, what: &str, hold: Duration) -> Result<(), String> {
    let (mut port, reader) = open_port(&path)?;
    let bytes = Arc::new(AtomicU64::new(0));
    spawn_drain(reader, Arc::clone(&bytes));

    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        port.write_all(&frame)?;
        port.flush()
    })
    .await
    .map_err(|e| format!("writer task panicked: {e}"))?
    .map_err(|e| format!("could not write to {path}: {e}"))?;

    eprintln!(
        "→ {what} sent to {path}; draining for {}ms",
        hold.as_millis()
    );
    tokio::time::sleep(hold).await;

    eprintln!("→ drained {} bytes", bytes.load(Relaxed));
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() || has_flag(&args, "--help") || has_flag(&args, "-h") {
        print!("{USAGE}");
        return;
    }
    if has_flag(&args, "--list-ports") {
        list_ports();
        return;
    }

    let path = match arg_val(&args, "--port").or_else(find_port) {
        Some(p) => p,
        None => {
            eprintln!("no serial port found; pass --port <path> (see --list-ports)");
            std::process::exit(1);
        }
    };

    let (frame, what) = if has_flag(&args, "--abort") {
        let (buf, n) = encode_abort();
        (buf[..n].to_vec(), "ABORT")
    } else {
        let profile = arg_val(&args, "--profile").unwrap_or_else(|| "fuzz".into());
        let packets: u16 = num(&args, "--packets", 1500);
        let cadence_ms: u16 = num(&args, "--cadence-ms", 5);
        let cfg = CampaignConfig {
            seed: num(&args, "--seed", 42),
            packets,
            cadence_ms,
            weights: profile_weights(&profile),
        };
        eprintln!(
            "→ campaign seed={} profile={} packets={} cadence={}ms weights={:?}",
            cfg.seed, profile, cfg.packets, cfg.cadence_ms, cfg.weights
        );
        let (buf, n) = encode_start(&cfg);
        (buf[..n].to_vec(), "START")
    };

    // Default the drain to the campaign's own duration plus a margin, so the
    // firmware's final END frame lands before we stop listening.
    let hold = arg_val(&args, "--hold-ms")
        .and_then(|s| s.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| {
            Duration::from_millis(
                num::<u64>(&args, "--packets", 1500) * num::<u64>(&args, "--cadence-ms", 5) + 1000,
            )
        });

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(e) = rt.block_on(drive(path, frame, what, hold)) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
