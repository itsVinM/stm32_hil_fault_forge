//! faultforge-host: Tokio orchestrator + DUT for the fault-injection bench.
//! The host *is* the device under test: it validates a measurement stream that
//! the STM32 injector deliberately faults, scores detections against the
//! ground-truth FAULT_EVENT stream, and reports campaign metrics.

mod campaign;
mod config;
mod dut;
mod link;
mod report;

use std::path::Path;
use std::time::Duration;

use tokio::sync::mpsc;

const CMD_HELP: &str = "\
faultforge — hardware-in-the-loop fault injection bench

USAGE:
  faultforge run [OPTIONS]

OPTIONS:
  --config <file>       Campaign YAML config (default: campaign.yaml)
  --simulate            Run the injector in-process (no hardware needed)
  --seed <n>            Campaign RNG seed (default: 42)
  --packets <n>         Packets per campaign (default: 1500)
  --cadence-ms <n>      Packet cadence in ms (default: 5)
  --profile <name>      clean | fuzz | emi | stress | delay (default: fuzz)
  --quiet               Only print the final report
  --list-ports          List serial ports and exit
";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{CMD_HELP}");
        return;
    }
    if args.iter().any(|a| a == "--list-ports") {
        list_ports();
        return;
    }

    // Load YAML config if present
    let config_path = arg_val(&args, "--config").unwrap_or_else(|| "campaign.yaml".into());
    let opts = if Path::new(&config_path).exists() {
        match config::CampaignConfigYaml::load(Path::new(&config_path)) {
            Ok(cfg) => {
                eprintln!("→ loaded campaign config from {config_path}");
                cfg.to_opts()
            }
            Err(e) => {
                eprintln!("failed to load {config_path}: {e}");
                parse_args(&args)
            }
        }
    } else {
        parse_args(&args)
    };

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(run(opts));
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Opts {
    simulated: bool,
    seed: u32,
    packets: u16,
    cadence_ms: u16,
    profile: &'static str,
    quiet: bool,
    weights: Option<[u8; faultforge_shared::N_FAULT_KINDS]>,
}

fn arg_val(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn parse_args(args: &[String]) -> Opts {
    let profile = arg_val(args, "--profile").unwrap_or_else(|| "fuzz".into());
    Opts {
        simulated: args.iter().any(|a| a == "--simulate"),
        seed: arg_val(args, "--seed").and_then(|s| s.parse().ok()).unwrap_or(42),
        packets: arg_val(args, "--packets").and_then(|s| s.parse().ok()).unwrap_or(1500),
        cadence_ms: arg_val(args, "--cadence-ms").and_then(|s| s.parse().ok()).unwrap_or(5),
        profile: match profile.as_str() {
            "clean" => "clean",
            "fuzz" => "fuzz",
            "emi" => "emi",
            "stress" => "stress",
            "delay" => "delay",
            other => {
                eprintln!("unknown profile '{other}', using fuzz");
                "fuzz"
            }
        },
        quiet: args.iter().any(|a| a == "--quiet"),
        weights: None,
    }
}

fn list_ports() {
    println!("Available serial ports:");
    match serialport::available_ports() {
        Ok(ports) => {
            for p in ports {
                println!("  {}", p.port_name);
            }
        }
        Err(e) => eprintln!("could not enumerate ports: {e}"),
    }
}

fn find_port() -> Option<String> {
    let ports = serialport::available_ports().ok()?;
    // Favour ttyUSB / usbmodem / ttyACM style devices (ST-LINK VCP is usbmodem).
    ports
        .iter()
        .map(|p| p.port_name.clone())
        .find(|n| {
            n.contains("ttyUSB") || n.contains("usbmodem") || n.contains("ttyACM") || n.contains("cu.")
        })
}

async fn run(opts: Opts) {
    if !opts.simulated {
        let port = match find_port() {
            Some(p) => p,
            None => {
                eprintln!("no serial port found; try --simulate or --config with host.port");
                return;
            }
        };
        eprintln!("→ linking to injector on {port}");
    } else {
        eprintln!("→ simulated injector (in-process, no hardware)");
    }

    // Pipeline: link (bytes) -> parser task -> frame channel -> campaign task.
    let (bytes_tx, bytes_rx) = mpsc::channel::<Vec<u8>>(64);
    let (frame_tx, mut frame_rx) = mpsc::channel::<crate::dut::DuFrame>(256);

    let link = if opts.simulated {
        link::open_sim(bytes_tx)
    } else {
        let port = find_port().unwrap();
        match link::open_real(&port, 115200, bytes_tx) {
            Some(l) => l,
            None => {
                eprintln!("could not open {port}");
                return;
            }
        }
    };

    tokio::spawn(async move {
        parse_stream(bytes_rx, frame_tx).await;
    });

    // Handshake: PING for PONG before launching the campaign.
    if let Err(e) = handshake(&link, &mut frame_rx).await {
        eprintln!("handshake failed: {e}");
        eprintln!("is the injector flashed and the port right? (use faultforge --list-ports)");
        return;
    }
    eprintln!("→ link confirmed (injector responded to PING)");

    let weights = campaign::profile_weights(opts.profile);
    if !opts.quiet {
        eprintln!("→ campaign seed={} profile={} packets={} cadence={}ms", opts.seed, opts.profile, opts.packets, opts.cadence_ms);
        eprintln!("→ weights: {weights:?}");
    }

    let (end_tx, mut end_rx) = mpsc::channel::<campaign::CampaignResult>(1);
    tokio::spawn(campaign::run(opts, link, frame_rx, end_tx));

    match end_rx.recv().await {
        Some(res) => report::print(&res),
        None => eprintln!("campaign ended without a result"),
    }
}

async fn handshake(link: &link::Link, frame_rx: &mut mpsc::Receiver<dut::DuFrame>) -> Result<(), String> {
    let (frame, n) = faultforge_shared::encode_ping();
    let _ = link.outbound.send(frame[..n].to_vec()).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let f = frame_rx
                .recv()
                .await
                .ok_or_else(|| "frame channel closed".to_string())?;
            if f.kind == faultforge_shared::K_PONG {
                return Ok(());
            }
        }
    })
    .await
    .map_err(|_| "timeout waiting for PONG".to_string())?
}

async fn parse_stream(mut rx: mpsc::Receiver<Vec<u8>>, frame_tx: mpsc::Sender<dut::DuFrame>) {
    let mut parser = faultforge_shared::FrameParser::new();
    while let Some(chunk) = rx.recv().await {
        for &b in &chunk {
            if let Some(outcome) = parser.push(b) {
                match outcome {
                    faultforge_shared::Decoded::Frame { kind, payload, len } => {
                        let _ = frame_tx
                            .send(dut::DuFrame { kind, payload, len, bad_crc: false, garbage: false })
                            .await;
                    }
                    faultforge_shared::Decoded::BadCrc { kind } => {
                        let _ = frame_tx
                            .send(dut::DuFrame { kind, payload: [0; faultforge_shared::MAX_PAYLOAD], len: 0, bad_crc: true, garbage: false })
                            .await;
                    }
                    faultforge_shared::Decoded::Garbage => {
                        let _ = frame_tx
                            .send(dut::DuFrame { kind: u8::MAX, payload: [0; faultforge_shared::MAX_PAYLOAD], len: 0, bad_crc: false, garbage: true })
                            .await;
                    }
                }
            }
        }
    }
}