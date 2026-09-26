use defmt::info;
use embassy_stm32::{
    adc::{Adc, AdcChannel, AnyAdcChannel, SampleTime},
    bind_interrupts, dma, mode::Blocking, peripherals,
    Peri,
};

use shared::{SamplePacket, ChannelId};

use crate::transport::Transport;

bind_interrupts!(struct Irqs {
    DMA2_STREAM0 => dma::InterruptHandler<peripherals::DMA2_CH0>;
});

static mut ADC_BUF: shared::DmaBuf<u16, 4096> = shared::DmaBuf::new();


pub struct AnalogSampler {
    pa0_mv: u32,
    pa1_mv: u32,
    sample_count: u32,
}

impl AnalogSampler {
    fn new() -> Self {
        Self { 
            pa0_mv : 0,
            pa1_mv : 0,
            sample_count : 0,
        }
    }
    fn counts_to_mv(counts : u32) -> u32 {
        (counts * 3300) / 4095
    }

    fn update_analog(&mut self, measurements: &[u16], count: usize) {
        if count < 2 { return; }  
        
        self.pa0_mv = Self::counts_to_mv(
            measurements[..count].iter().step_by(2)
                .map(|&x| x as u32).sum::<u32>() / (count / 2) as u32
        );
        self.pa1_mv = Self::counts_to_mv(
            measurements[..count].iter().skip(1).step_by(2)
                .map(|&x| x as u32).sum::<u32>() / (count / 2) as u32
        );
        self.sample_count += count as u32;
    }

    fn log(&self){
        info!("PA0={} mV PA1={} mV n={}", 
            self.pa0_mv, 
            self.pa1_mv, 
            self.sample_count);
    }
}

/// Expected VREFINT, in counts, for a 12-bit ADC at 3.3 V.
/// Datasheet VREFINT = 1.21 V typical → 1.21 / 3.3 * 4095 ≈ 1502 counts.
const VREFINT_EXPECTED: u16 = 1502;
/// How far VREFINT may drift from nominal before we flag it.
const VREFINT_TOL: u16 = 200;
/// Plausible junction temperature bounds for the report (°C), ± 25 °C sanity.
const TEMP_MIN_C: i32 = -25;
const TEMP_MAX_C: i32 = 125;

/// Result of the telemetry-loopback capture.
#[derive(Debug, Clone, Copy)]
pub struct TelemetryLoopback {
    pub vrefint_counts: u16,
    pub vrefint_mv: u32,
    pub temp_c: i32,
}

impl TelemetryLoopback {
    pub fn vrefint_ok(&self) -> bool {
        self.vrefint_counts.abs_diff(VREFINT_EXPECTED) <= VREFINT_TOL
    }
    pub fn temp_ok(&self) -> bool {
        self.temp_c >= TEMP_MIN_C && self.temp_c <= TEMP_MAX_C
    }
    pub fn ok(&self) -> bool {
        self.vrefint_ok() && self.temp_ok()
    }
}

/// Capture VREFINT + junction temperature once and sanity-check them.
///
/// Exercises the real ADC peripheral without needing external wiring — the
/// "telemetry loopback" step of BIST. Must run before `adc_task` takes
/// ownership of ADC1, since it holds the peripheral for its lifetime.
pub fn telemetry_loopback(adc: Peri<'_, peripherals::ADC1>) -> TelemetryLoopback {
    let mut adc = Adc::new_blocking(adc, Default::default());
    let mut vref = adc.enable_vrefint();
    let mut temp = adc.enable_temperature();

    let vrefint_counts = adc.blocking_read(&mut vref, SampleTime::CYCLES480) as u16;
    let vrefint_mv = AnalogSampler::counts_to_mv(vrefint_counts as u32);
    // STM32F401RM: T(degC) = (V25 - Vsense) / Avg_slope + 25, V25 ≈ 0.76 V,
    // Avg_slope ≈ 2.5 mV / °C.
    let temp_c = if vrefint_mv > 700 {
        25 - ((vrefint_mv as i32 - 760) * 10 / 25)
    } else {
        0
    };
    info!("Telemetry loopback: VREFINT = {} counts ({} mV), temp = {} °C", vrefint_counts, vrefint_mv, temp_c);
    TelemetryLoopback {
        vrefint_counts,
        vrefint_mv,
        temp_c,
    }
}

#[embassy_executor::task]
#[allow(static_mut_refs)]
pub async fn adc_task(
    adc: Peri<'static, peripherals::ADC1>,
    dma: Peri<'static, peripherals::DMA2_CH0>,
    mut pin0: Peri<'static, peripherals::PA0>,
    mut pin1: Peri<'static, peripherals::PA1>,
    mut transport: Transport,
) {
    let adc_driver = Adc::new(adc);
    let channel0: AnyAdcChannel<peripherals::ADC1> = pin0.degrade_adc();
    let channel1: AnyAdcChannel<peripherals::ADC1> = pin1.degrade_adc();
    
    let buf = unsafe { ADC_BUF.as_mut_slice() };

    let mut ring = adc_driver.into_ring_buffered(
        dma,
        buf,
        Irqs,
        [
            (channel0, SampleTime::Cycles15),
            (channel1, SampleTime::Cycles15),
        ].into_iter(),
        None,
    );

    let mut adc_sampler = AnalogSampler::new();
    let mut measurements = [0u16; 2048];
    let mut packet = SamplePacket::new();
    let mut seq = 0u16;
    let mut last_timestamp: u32 = 0;
    
    loop {
        let n = ring.read(&mut measurements).await;
        if let Err(e) = n {
            info!("adc ring read error: Overrun {} samples", e);
            continue;
        };
        let n = n.unwrap_or(0);
        let count = n.min(measurements.len());
        if count == 0 { 
            info!("adc zero-length read");
            continue; 
        }
        
        adc_sampler.update_analog(&measurements, count);
        adc_sampler.log();
        
        // Convert ADC counts to mV using calibration
        let timestamp = last_timestamp;
        let pa0_mv = adc_sampler.pa0_mv;
        let pa1_mv = adc_sampler.pa1_mv;
        
        // Push samples to packet
        if !isnan(pa0_mv) {
            packet.push(ChannelId::Analog0, timestamp, pa0_mv);
        }
        if !isnan(pa1_mv) {
            packet.push(ChannelId::Analog1, timestamp, pa1_mv);
        }
        
        if packet.is_full() {
            packet.finalize(seq);
            transport.send_packet(&packet).await;
            info!("transport: packet {} sent (seq: {}), samples: {}",
                  seq, seq, packet.count);
            seq = seq.wrapping_add(1);
            packet = SamplePacket::new();
        }

        last_timestamp = last_timestamp.wrapping_add(count as u32 * 100);
    }
}

fn isnan(value: u32) -> bool {
    value == 0 || value > 65535
}

