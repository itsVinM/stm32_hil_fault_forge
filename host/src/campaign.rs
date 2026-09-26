//! Campaign execution: fires the injector, runs the DUT over the stream,
//! scores every injection against the DUT's detections, and hands the results
//! to reporting.

use std::time::Duration;

use faultforge_shared::{encode_start, get_u16, get_u32, K_END, K_FAULT, K_SENSOR};
use tokio::sync::mpsc;

use crate::dut::{DetKind, Detection, Dut, DuFrame, GroundTruth};
use crate::link::Link;
use crate::Opts;

pub const FAULT_LABELS: [&str; 8] =
    ["none", "bitflip", "byte", "drop", "delay", "replay", "burst", "duty"];

/// Weight vectors per profile, indexed FAULT_BITFLIP..FAULT_DUTY.
pub fn profile_weights(name: &str) -> [u8; faultforge_shared::N_FAULT_KINDS] {
    match name {
        "clean" => [0, 0, 0, 0, 0, 0, 0],
        "fuzz" => [8, 8, 8, 8, 8, 8, 0],
        "emi" => [30, 15, 0, 5, 10, 25, 0],
        "stress" => [5, 5, 40, 30, 15, 5, 0],
        "delay" => [0, 0, 0, 100, 0, 0, 0],
        _ => [8, 8, 8, 8, 8, 8, 0],
    }
}

pub struct KindStat {
    pub kind: u8,
    pub injected: u16,
    pub detected: u16,
}

pub struct CampaignResult {
    pub seed: u32,
    pub profile: &'static str,
    pub packets: u16,
    pub simulated: bool,
    pub injected: u16,
    pub detected: u16,
    pub missed: u16,
    pub physical: u16, // out-of-band faults (duty glitch) not scored
    pub avg_latency_ms: f64,
    pub max_latency_ms: u64,
    pub per_kind: Vec<KindStat>,
    /// Raw rows for the CSV report.
    pub rows: Vec<(u32, u8, u16, bool, &'static str, u64)>, // fault_id, kind, at_seq, detected, kind_label, latency_ms
}

/// Expected DUT detections for each injected fault kind.
fn expected_dets(kind: u8) -> &'static [DetKind] {
    match kind {
        faultforge_shared::FAULT_BITFLIP | faultforge_shared::FAULT_BYTE => {
            &[DetKind::Crc, DetKind::Burst]
        }
        faultforge_shared::FAULT_DROP => &[DetKind::Gap],
        faultforge_shared::FAULT_DELAY => &[DetKind::Late],
        faultforge_shared::FAULT_REPLAY => &[DetKind::Replay],
        faultforge_shared::FAULT_BURST => &[DetKind::Burst],
        _ => &[],
    }
}

pub async fn run(opts: Opts, link: Link, mut frame_rx: mpsc::Receiver<DuFrame>, end_tx: mpsc::Sender<CampaignResult>) {
    let cfg = faultforge_shared::CampaignConfig {
        seed: opts.seed,
        packets: opts.packets,
        cadence_ms: opts.cadence_ms,
        weights: profile_weights(opts.profile),
    };
    let (frame, n) = encode_start(&cfg);
    let _ = link.outbound.send(frame[..n].to_vec()).await;

    let mut dut = Dut::new(opts.cadence_ms);
    let mut truths: Vec<GroundTruth> = Vec::new();

    // Drain frames until the injector says the campaign is over.
    loop {
        let f = match frame_rx.recv().await {
            Some(f) => f,
            None => break,
        };
        match f.kind {
            K_SENSOR => dut.process(&f),
            K_FAULT => {
                if let Some(gt) = GroundTruth::from_payload(&f.payload[..f.len], dut.frame_idx) {
                    truths.push(gt);
                }
            }
            K_END => break,
            _ => dut.process(&f),
        }
    }

    let res = score(opts, std::mem::take(&mut dut.detections), truths);
    let _ = end_tx.send(res).await;
}

/// Greedy time-ordered match: every injection must be paired with a detection
/// whose kind is plausible for it and whose sequence/timing is close.
fn score(opts: Opts, detections: Vec<Detection>, truths: Vec<GroundTruth>) -> CampaignResult {
    let frame_period = Duration::from_millis(opts.cadence_ms as u64);

    let mut det_cursor = 0usize;
    let mut matches: Vec<(GroundTruth, Option<(Detection, u64)>)> = Vec::new();

    for t in &truths {
        let expected = expected_dets(t.kind);
        // Out-of-band (physical) faults are recorded but not scored.
        if expected.is_empty() {
            matches.push((*t, None));
            continue;
        }
        // Scan forward for the first unmatched detection matching this fault.
        let mut best: Option<(Detection, u64)> = None;
        let mut j = det_cursor;
        while j < detections.len() {
            let d = &detections[j];
            let seq_ok = dseq_diff(d.seq, t.at_seq) <= 2;
            let time_ok = d.at.duration_since(t.at).as_millis() as i64
                > -(frame_period.as_millis() as i64 * 2);
            if expected.contains(&d.kind) && seq_ok && time_ok {
                best = Some((*d, d.at.duration_since(t.at).as_millis() as u64));
                break;
            }
            j += 1;
        }
        match best {
            Some((d, lat)) => {
                det_cursor = j + 1;
                matches.push((*t, Some((d, lat))));
            }
            None => matches.push((*t, None)),
        }
    }

    let mut rows = Vec::new();
    let mut injected = 0u16;
    let mut detected = 0u16;
    let mut physical = 0u16;
    let mut lat_total = 0u64;
    let mut lat_max = 0u64;
    let mut per_kind: Vec<KindStat> = Vec::new();

    use std::collections::BTreeMap;
    let mut counts: BTreeMap<u8, (u16, u16)> = BTreeMap::new();

    for (t, m) in &matches {
        injected += 1;
        let ok = m.is_some();
        let entry = counts.entry(t.kind).or_insert((0, 0));
        entry.0 += 1;
        if ok {
            detected += 1;
            entry.1 += 1;
        }
        let (det_label, lat) = match m {
            Some((d, lat)) => (d.kind.as_str(), *lat),
            None => {
                if expected_dets(t.kind).is_empty() {
                    ("physical", 0)
                } else {
                    ("__miss__", 0)
                }
            }
        };
        if expected_dets(t.kind).is_empty() {
            physical += 1;
        }
        if !expected_dets(t.kind).is_empty() && ok {
            lat_total += lat;
            lat_max = lat_max.max(lat);
        }
        rows.push((t.fault_id, t.kind, t.at_seq, ok, det_label, lat));
    }

    for (kind, (inj, det)) in counts {
        per_kind.push(KindStat { kind, injected: inj, detected: det });
    }

    let lat_avg = if detected > 0 { lat_total as f64 / detected as f64 } else { 0.0 };

    CampaignResult {
        seed: opts.seed,
        profile: opts.profile,
        packets: opts.packets,
        simulated: opts.simulated,
        injected,
        detected,
        missed: injected.saturating_sub(detected).saturating_sub(physical),
        physical,
        avg_latency_ms: lat_avg,
        max_latency_ms: lat_max,
        per_kind,
        rows,
    }
}

fn dseq_diff(a: u16, b: u16) -> u32 {
    let d = a.wrapping_sub(b) as i16;
    u32::from(d.unsigned_abs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dut::{Detection};
    use std::time::Instant;

    fn dt(kind: DetKind, seq: u16) -> (Detection, Instant) {
        let d = Detection { kind, seq, at: Instant::now(), frame_idx: 0 };
        (d, d.at)
    }

    fn gt(kind: u8, at_seq: u16, fid: u32) -> GroundTruth {
        GroundTruth { fault_id: fid, kind, at_seq, frame_idx: 0, at: Instant::now() }
    }

    #[test]
    fn all_faults_detected_in_calibration_set() {
        let dets = vec![
            dt(DetKind::Gap, 11).0,
            dt(DetKind::Late, 21).0,
            dt(DetKind::Replay, 33).0,
            dt(DetKind::Crc, 41).0,
            dt(DetKind::Burst, 55).0,
            dt(DetKind::Crc, 66).0, // byte corrupt
            dt(DetKind::Burst, 77).0, // burst
        ];
        let truths = vec![
            gt(faultforge_shared::FAULT_DROP, 10, 1),
            gt(faultforge_shared::FAULT_DELAY, 21, 2),
            gt(faultforge_shared::FAULT_REPLAY, 33, 3),
            gt(faultforge_shared::FAULT_BITFLIP, 41, 4),
            gt(faultforge_shared::FAULT_BYTE, 55, 5),
            gt(faultforge_shared::FAULT_BITFLIP, 66, 6),
            gt(faultforge_shared::FAULT_BURST, 77, 7),
        ];
        let opts = Opts { simulated: true, seed: 1, packets: 100, cadence_ms: 5, profile: "fuzz", quiet: true };
        let res = score(opts, dets, truths);
        assert_eq!(res.injected, 7);
        assert_eq!(res.detected, 7, "all injected faults should be detected");
        assert_eq!(res.missed, 0);
    }

    #[test]
    fn undetected_fault_counts_as_miss() {
        let dets = vec![dt(DetKind::Crc, 10).0];
        let truths = vec![
            gt(faultforge_shared::FAULT_DROP, 10, 1),
            gt(faultforge_shared::FAULT_DELAY, 20, 2),
        ];
        let opts = Opts { simulated: true, seed: 1, packets: 100, cadence_ms: 5, profile: "fuzz", quiet: true };
        let res = score(opts, dets, truths);
        // drop mismatch: crc detection isn't a drop detection
        assert_eq!(res.injected, 2);
        assert_eq!(res.detected, 0);
        assert_eq!(res.missed, 2);
    }
}