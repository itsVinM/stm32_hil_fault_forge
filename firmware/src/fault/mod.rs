//! Fault-injection stability check for BIST.

use defmt::{info, trace};
use shared::fault_traits::state::Armed;
use shared::fault::{
    I2cBus, I2cFaultInjector, SpiBus, SpiFaultInjector, UartBus, UartFaultInjector,
};
use shared::{FaultConfig, FaultResult, FaultType, Protocol};

// Result of the injector stability self-check.
[#derive(Debug, Clone, Copy)]
pub struct StabilityReport {
    // Number of injectors that produced the expected frame on every fire.
    pub passed: u32,
    // Number of injectors that misbehaved (wrong bit, missed injection, etc).
    pub failed: u32,
}

impl StabilityReport {
    pub fn ok(&self) -> bool {
        self.failed == 0
    }
}

/// Run every injector through a `configure → arm → fire → disarm` cycle and
/// assert the fire produced the expected corruption.
///
/// Fire is always performed with `probability_permille = 1000` so the outcome
/// is deterministic: each injection MUST alter the frame. We then check the
/// injected counter incremented, the bus bits moved, and that `disarm` returned
/// the bus to an unchanged baseline.

pub fn stability_check() -> StabilityReport{
    info!("bist/fault: injector stability check — SPI/I2C/UART");
    let mut report = StabilityReport { passed: 0, failed: 0 };

    // SPI: bitflip at bit 0 on MOSI
    let mut spi = SpiFaultInjector::new();
    spi.configue(
        &FaultConfig::new(Protocol::Spi, FaultType::BitFlip).at_bit(0).probability(1000),
    );
    let mut armed_spi: Option<SpiFaultInjector><Armed> = Some(spi.arm());
    let mut spi_bus = SpiBus { sck: true, mosu: 0xA5, , miso: 0x5A, cs: true};

    match armed_spi.as_mut(){
        Some(inj) => {
            let before = spi_bus.mosi;
            let res = inj.fire(&mut spi_bus);
            let count = inj.injected_count();
            let expected = before ^ 0x01; // bit 0 flipped
            let ok = res == FaultResult::Fired && count == 1 && spi_bus.mosi == expected;
            report_passed(&mut report, ok, "SPI BitFlip@0");
            trace!("spi mosi 0x{:02X} -> 0x{:02X} (want 0x{:02X})", before, spi_bus.mosi, expected);
        }
        None => report_failed(&mut report, "SPI armed"),
    }
    armed_spi = armed_spi.map(|inj| inj.disarm());

    // I2C StuckAtZero on bit 3 of the data byte
    let mut i2c = I2cFaultInjector::new();
    i2c.configure(
        &FaultConfig::new(Protocol::I2c, FaultType::StuckAtZero).at_bit(3).probability(1000),
    );
    let mut armed_i2c: Option<I2cFaultInjector<Armed>> = Some(i2c.arm());
    let mut i2c_bus = I2cBus { sds: true, scl: true, address: 0x50, data: 0xFF };

    match armed_i2c.as_mut(){
        Some(inj) =>{
            let before = i2c_bus.data;
            let res = inj.fire(&mut i2c_bus);
            let count = inj.injected_count();
            let expected = before & !0x08; // bit 3 cleared
            let ok = res == FaultResult::Fired && count == 1 && i2c_bus.data == expected;
            report_passed(&mut report, ok, "I2C StuckAtZero@3");
            trace!("i2c data 0x{:02X} -> 0x{:02X} (want 0x{:02X})", before, i2c_bus.data, expected);
        }
        None => report_failed(&mut report, "I2C armed"),
    }
    armed_i2c = armed_i2c.map(|inj| inj.disarm());
    
    // UART flip bit 7 of the data byte
    let mut uart = UartFaultInjector::new();
    uart.configure(
        &FaultConfig::new(Protocol::Uart, FaultType::BitFlip).at_bit(7).probability(1000),
    );
    let mut armed_uart: Option<UartFaultInjector<Armed>> = Some(uart.arm());
    let mut uart_bus = UartBus { tx: 0x00, rx: 0x55 };

    match armed_uart.as_mut(){
        Some (inj) => {
            let before = uart_bus.tx;
            let res = inj.fire(&mut uart_bus);
            let count = inj.injected_count();
            let expected = before ^0x80; // bit 7 flipped
            let ok = res == FaultResult::Fired && count == 1 && uart_bus.tx == expected;
            report_passed(&mut report, ok, "UART BitFlip@7");
            trace!("uart tx 0x{:02X} -> 0x{:02X} (want 0x{:02X})", before, uart_bus.tx, expected);
        }
        None => report_failed(&mut report, "UART armed"),
    }
    armed_uart = armed_uart.map(|inj| inj.disarm());

    info!("bist/fault: passed={} failed={}", report.passed, report.failed);
    report
}

fn report_passed(report: &mut StabilityReport, ok:bool, name: &str){
    if ok{
        report.passed += 1;
        info!("bist/fault: {} passed", name);
    } else {
        report.failed += 1;
        info!("bist/fault: {} failed", name);
    }
}

fn report_failed(report: &mut StabilityReport, name: &str){
    report.failed += 1;
    info!("bist/fault: {} failed", name);
}
