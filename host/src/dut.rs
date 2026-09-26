//! The device-under-test. The host receives the injector's measurement stream
//! and must detect every anomaly: CRC failures, sequence gaps, duplicate
//! frames, late arrivals and garbage bursts — exactly like a telemetry
//! consumer that must never trust a faulted link.

use std::time::Instant;

use faultforge_shared::{get_u16, get_u32, K_SENSOR, MAX_PAYLOAD};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum DetKind {
    /// A frame failed its CRC check (or framing broke so hard we saw garbage).
    Crc,
    /// Missing sequence numbers between two good frames.
    Gap,
    /// A sequence number appeared twice.
    Replay,
    /// A frame arrived later than the cadence budget allows.
    Late,
    /// A run of unparseable bytes between frames.
    Burst,
}

impl DetKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            DetKind::Crc => "crc",
            DetKind::Gap => "gap",
            DetKind::Replay => "replay",
            DetKind::Late => "late",
            DetKind::Burst => "burst",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Detection {
    pub kind: DetKind,
    pub seq: u16,
    pub at: Instant,
}

/// A frame produced by the shared streaming parser.
#[derive(Clone, Copy, Debug)]
pub struct DuFrame {
    pub kind: u8,
    pub payload: [u8; MAX_PAYLOAD],
    pub len: usize,
    pub bad_crc: bool,
    pub garbage: bool,
}

pub struct Dut {
    cadence_ms: u16,
    last_seq: Option<u16>,
    last_time: Option<Instant>,
    junk_run: u32,
    burst_flagged: bool,
    pub detections: Vec<Detection>,
}

impl Dut {
    pub fn new(cadence_ms: u16) -> Self {
        Self {
            cadence_ms,
            last_seq: None,
            last_time: None,
            junk_run: 0,
            burst_flagged: false,
            detections: Vec::new(),
        }
    }

    fn record(&mut self, kind: DetKind, seq: u16) {
        self.detections.push(Detection {
            kind,
            seq,
            at: Instant::now(),
        });
    }

    /// Late budget: allow a little headroom over a single cadence, but any
    /// frame this far outside the normal spacing is suspicious. Delayed
    /// (FAULT_DELAY) frames hop 6 cadences, far above this.
    fn late_budget_ms(&self) -> u64 {
        self.cadence_ms as u64 * 4 + 5
    }

    pub fn process(&mut self, f: &DuFrame) {
        if f.garbage {
            self.junk_run += 1;
            self.burst_flagged = false;
            return;
        }

        // A burst of junk then a real frame: coalesce into a single Burst detection.
        if self.junk_run >= 1 {
            self.junk_run = 0;
            if !self.burst_flagged {
                self.burst_flagged = true;
                let seq = self.last_seq.map(|s| s.wrapping_add(1)).unwrap_or(0);
                self.record(DetKind::Burst, seq);
            }
        }

        if f.bad_crc {
            let seq = self.last_seq.map(|s| s.wrapping_add(1)).unwrap_or(0);
            self.record(DetKind::Crc, self.guess_seq(seq));
            return;
        }

        if f.kind != K_SENSOR {
            return;
        }

        let seq = get_u16(&f.payload, 0);
        let now = Instant::now();

        if let (Some(last), Some(lt)) = (self.last_seq, self.last_time) {
            let on_time = (now.duration_since(lt).as_millis() as u64) < self.late_budget_ms();
            let expected = last.wrapping_add(1);
            if seq == expected {
                // Normal arrival. But if it was grossly late, flag it.
                if !on_time {
                    self.record(DetKind::Late, seq);
                }
            } else if seq == last {
                self.record(DetKind::Replay, seq);
            } else if seq > expected {
                // A drop (or two) happened somewhere before us.
                self.record(DetKind::Gap, seq);
            } else {
                // Arrived out of order (older than last) but not a duplicate.
                // Treated as a gap-adjacent anomaly; justify against a Drop
                // or Delay injection in the scorer.
                self.record(DetKind::Replay, seq);
            }
        }

        self.last_seq = Some(seq);
        self.last_time = Some(now);
    }

    fn guess_seq(&self, seq: u16) -> u16 {
        seq
    }
}

/// Ground-truth from the injector: it tells us exactly what it injected, so
/// the host can score its own detections.
#[derive(Clone, Copy, Debug)]
pub struct GroundTruth {
    pub fault_id: u32,
    pub kind: u8,
    pub at_seq: u16,
    pub at: Instant,
}

impl GroundTruth {
    pub fn from_payload(p: &[u8]) -> Option<Self> {
        if p.len() < faultforge_shared::FAULT_PAYLOAD_LEN {
            return None;
        }
        Some(Self {
            fault_id: get_u32(p, 0),
            kind: p[4],
            at_seq: get_u16(p, 5),
            at: Instant::now(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faultforge_shared::{encode_sensor, encode_fault_event};

    fn feed(dut: &mut Dut, bytes: &[u8]) {
        let mut p = faultforge_shared::FrameParser::new();
        for &b in bytes {
            if let Some(outcome) = p.push(b) {
                let frame = match outcome {
                    faultforge_shared::Decoded::Frame { kind, payload, len } => DuFrame {
                        kind,
                        payload,
                        len,
                        bad_crc: false,
                        garbage: false,
                    },
                    faultforge_shared::Decoded::BadCrc { kind } => DuFrame {
                        kind,
                        payload: [0; MAX_PAYLOAD],
                        len: 0,
                        bad_crc: true,
                        garbage: false,
                    },
                    faultforge_shared::Decoded::Garbage => DuFrame {
                        kind: u8::MAX,
                        payload: [0; MAX_PAYLOAD],
                        len: 0,
                        bad_crc: false,
                        garbage: true,
                    },
                };
                dut.process(&frame);
            }
        }
    }

    #[test]
    fn clean_stream_no_detections() {
        let mut dut = Dut::new(5);
        for seq in 0u16..100 {
            let (b, n) = encode_sensor(seq, 100 + seq as u32, 0);
            feed(&mut dut, &b[..n]);
        }
        assert!(dut.detections.is_empty(), "{:?}", dut.detections);
    }

    #[test]
    fn replay_detected() {
        let mut dut = Dut::new(5);
        for seq in [7u16, 8, 8] {
            let (b, n) = encode_sensor(seq, 100, 0);
            feed(&mut dut, &b[..n]);
        }
        let kinds: Vec<_> = dut.detections.iter().map(|d| d.kind).collect();
        assert_eq!(kinds, vec![DetKind::Replay]);
    }

    #[test]
    fn drop_causes_gap() {
        let mut dut = Dut::new(5);
        for seq in [3u16, 4, 6, 7] {
            let (b, n) = encode_sensor(seq, 100, 0);
            feed(&mut dut, &b[..n]);
        }
        let kinds: Vec<_> = dut.detections.iter().map(|d| d.kind).collect();
        assert_eq!(kinds, vec![DetKind::Gap]);
    }

    #[test]
    fn corrupted_frame_flagged() {
        let mut dut = Dut::new(5);
        let (b, n) = encode_sensor(1, 100, 0);
        feed(&mut dut, &b[..n]);
        // next frame with a corrupted payload byte
        let (mut b2, n2) = encode_sensor(2, 100, 0);
        b2[4] ^= 0xFF;
        feed(&mut dut, &b2[..n2]);
        let kinds: Vec<_> = dut.detections.iter().map(|d| d.kind).collect();
        assert!(kinds.contains(&DetKind::Crc), "{:?}", kinds);
    }

    #[test]
    fn late_frame_detected() {
        let mut dut = Dut::new(2);
        let (b, n) = encode_sensor(0, 100, 0);
        feed(&mut dut, &b[..n]);
        std::thread::sleep(std::time::Duration::from_millis(30));
        let (b, n) = encode_sensor(1, 100, 0);
        feed(&mut dut, &b[..n]);
        let kinds: Vec<_> = dut.detections.iter().map(|d| d.kind).collect();
        assert!(kinds.contains(&DetKind::Late), "{:?}", kinds);
    }

    #[test]
    fn fault_event_parses() {
        let (b, n) = encode_fault_event(9, faultforge_shared::FAULT_DROP, 42);
        let mut p = faultforge_shared::FrameParser::new();
        let mut got = None;
        for &x in &b[..n] {
            if let Some(faultforge_shared::Decoded::Frame { payload, len, .. }) = p.push(x) {
                got = GroundTruth::from_payload(&payload[..len]);
            }
        }
        let gt = got.unwrap();
        assert_eq!(gt.fault_id, 9);
        assert_eq!(gt.kind, faultforge_shared::FAULT_DROP);
        assert_eq!(gt.at_seq, 42);
    }
}