//! Byte-stream transport to the injector.
//!
//! Real hardware uses `serialport` on a (usually) ttyUSB/ST-LINK VCP device.
//! Simulated mode runs a miniature copy of the firmware injector in a thread so
//! the whole host pipeline is exercisable without hardware.

use std::io::Write;
use std::time::Duration;

use faultforge_shared::{
    decode_start, encode_end, encode_fault_event, encode_pong, encode_sensor, FrameParser,
    K_ABORT, K_PING, K_START, Lcg,
};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct Link {
    pub outbound: mpsc::Sender<Vec<u8>>,
}

/// Open a real serial port; spawns reader + writer bridge threads.
/// Streamed bytes are forwarded to `bytes_tx` for the host parser.
pub fn open_real(path: &str, baud: u32, bytes_tx: mpsc::Sender<Vec<u8>>) -> Option<Link> {
    let port = serialport::new(path, baud)
        .timeout(Duration::from_millis(50))
        .open()
        .ok()?;
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(16);

    let tx_port = port.try_clone().ok()?;
    std::thread::spawn(move || {
        let mut port = tx_port;
        while let Some(chunk) = out_rx.blocking_recv() {
            let _ = port.write_all(&chunk);
            let _ = port.flush();
        }
    });

    std::thread::spawn(move || {
        let mut port = port;
        let mut buf = [0u8; 512];
        loop {
            match port.read(&mut buf) {
                Ok(0) => continue,
                Err(_) => continue,
                Ok(n) => {
                    if bytes_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    Some(Link { outbound: out_tx })
}

/// Spawn a thread that acts as the injector, reading outbound host bytes for
/// control frames and producing the faulted sensor stream.
pub fn open_sim(bytes_tx: mpsc::Sender<Vec<u8>>) -> Link {
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(16);

    std::thread::spawn(move || {
        let mut parser = FrameParser::new();
        let mut cfg: Option<SharedCfg> = None;

        loop {
            let got = out_rx.blocking_recv().expect("sim outbound channel closed");
            for &b in &got {
                if let Some(faultforge_shared::Decoded::Frame { kind, payload, len }) = parser.push(b) {
                    match kind {
                        K_PING => {
                            let (f, n) = encode_pong();
                            let _ = bytes_tx.blocking_send(f[..n].to_vec());
                        }
                        K_START => {
                            if let Some(c) = decode_start(&payload[..len]) {
                                cfg = Some(SharedCfg::from(c));
                            }
                        }
                        K_ABORT => {
                            cfg = None;
                        }
                        _ => {}
                    }
                }
            }

            if let Some(c) = cfg.as_mut() {
                if c.run_next(&bytes_tx) {
                    cfg = None;
                }
            }
        }
    });

    Link { outbound: out_tx }
}

/// Faithful copy of the firmware campaign engine, so `--simulate` reproduces
/// exactly what the STM32 emits for a given seed.
struct SharedCfg {
    packets: u16,
    cadence_ms: u16,
    weights: [u8; faultforge_shared::N_FAULT_KINDS],
    rng: Lcg,
    seq: u16,
    fault_id: u32,
    sent: u16,
}

impl SharedCfg {
    fn from(c: faultforge_shared::CampaignConfig) -> Self {
        Self {
            packets: c.packets,
            cadence_ms: c.cadence_ms,
            weights: c.weights,
            rng: Lcg::new(c.seed),
            seq: 0,
            fault_id: 0,
            sent: 0,
        }
    }

    fn pick(&mut self) -> u8 {
        let total: u32 = self.weights.iter().map(|&w| w as u32).sum();
        if total == 0 {
            return 0;
        }
        let mut r = self.rng.below(total);
        for (i, &w) in self.weights.iter().enumerate() {
            if r < w as u32 {
                return (i as u8) + 1; // FAULT_BITFLIP..FAULT_DUTY
            }
            r -= w as u32;
        }
        0
    }

    /// Emit whatever the injector would for the next slot in the cadence.
    /// Returns true when the campaign is exhausted.
    fn run_next(&mut self, bytes_tx: &mpsc::Sender<Vec<u8>>) -> bool {
        if self.sent >= self.packets {
            let (f, n) = encode_end(self.packets, self.fault_id);
            let _ = bytes_tx.blocking_send(f[..n].to_vec());
            return true;
        }

        let kind = self.pick();
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        self.sent += 1;

        let value = 100u32 + self.rng.below(50);
        match kind {
            faultforge_shared::FAULT_BITFLIP | faultforge_shared::FAULT_BYTE => {
                self.fault_id += 1;
                let fid = self.fault_id;
                let mut payload = [0u8; faultforge_shared::SENSOR_PAYLOAD_LEN];
                payload[2..6].copy_from_slice(&value.to_le_bytes());
                if kind == faultforge_shared::FAULT_BITFLIP {
                    let bit = self.rng.below(16) as usize;
                    payload[2 + (bit >> 3)] ^= 1 << (bit & 7);
                } else {
                    let idx = 2 + self.rng.below(4) as usize;
                    payload[idx] ^= 0xA5;
                }
                let (b, n) = encode_sensor(seq, read_u32(&payload, 2), fid);
                let _ = bytes_tx.blocking_send(b[..n].to_vec());
                let (f, n) = encode_fault_event(fid, kind, seq);
                let _ = bytes_tx.blocking_send(f[..n].to_vec());
            }
            faultforge_shared::FAULT_DROP => {
                self.fault_id += 1;
                let fid = self.fault_id;
                let (f, n) = encode_fault_event(fid, kind, seq);
                let _ = bytes_tx.blocking_send(f[..n].to_vec());
            }
            faultforge_shared::FAULT_DELAY => {
                self.fault_id += 1;
                let fid = self.fault_id;
                std::thread::sleep(Duration::from_millis(self.cadence_ms as u64 * 2));
                let (b, n) = encode_sensor(seq, value, 0);
                let _ = bytes_tx.blocking_send(b[..n].to_vec());
                let (f, n) = encode_fault_event(fid, kind, seq);
                let _ = bytes_tx.blocking_send(f[..n].to_vec());
            }
            faultforge_shared::FAULT_REPLAY => {
                self.fault_id += 1;
                let fid = self.fault_id;
                let (b, n) = encode_sensor(seq, value, 0);
                let _ = bytes_tx.blocking_send(b[..n].to_vec());
                let (b, n) = encode_sensor(seq, value, 0);
                let _ = bytes_tx.blocking_send(b[..n].to_vec());
                let (f, n) = encode_fault_event(fid, kind, seq);
                let _ = bytes_tx.blocking_send(f[..n].to_vec());
            }
            faultforge_shared::FAULT_BURST => {
                self.fault_id += 1;
                let fid = self.fault_id;
                let mut junk = [0u8; 6];
                for j in junk.iter_mut() {
                    *j = self.rng.next_u8();
                }
                let _ = bytes_tx.blocking_send(junk.to_vec());
                let (b, n) = encode_sensor(seq, value, 0);
                let _ = bytes_tx.blocking_send(b[..n].to_vec());
                let (f, n) = encode_fault_event(fid, kind, seq);
                let _ = bytes_tx.blocking_send(f[..n].to_vec());
            }
            _ => {
                // clean
                let (b, n) = encode_sensor(seq, value, 0);
                let _ = bytes_tx.blocking_send(b[..n].to_vec());
            }
        }

        std::thread::sleep(Duration::from_millis(self.cadence_ms as u64));
        false
    }
}

fn read_u32(p: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]])
}