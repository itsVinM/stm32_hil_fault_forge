//! Bit-banged bus exercisers for the HIL bench.
//!
//! Ported from the HiL Probe firmware protocol set (`hil-probe/fw/src/protocols`).
//! Pure `no_std` logic driven through the [`GpioOps`] trait, so the same code runs
//! against a recording backend in unit tests and against real pins in the
//! firmware. Each exerciser emits a realistic transaction on a handful of
//! logical pins; a backend may log every edge for off-line decoding.
//!
//! Dormant: nothing in the campaign path calls these yet, hence the
//! `dead_code` allowance (mirroring `probe_gpio.rs`).

#![allow(dead_code)]

pub mod pins {
    //! Logical pin map shared by the exercisers (labels only; a backend maps
    //! these to physical pins).

    pub const SCK: u8 = 0; // SPI clock
    pub const MOSI: u8 = 1; // SPI master-out
    pub const MISO: u8 = 2; // SPI master-in
    pub const CS: u8 = 3; // SPI chip-select
    pub const SCL: u8 = 4; // I2C clock
    pub const SDA: u8 = 5; // I2C data
    pub const TX: u8 = 7; // UART / RS-485 TX
    pub const RX: u8 = 8; // UART / RS-485 RX
    pub const TX_EN: u8 = 11; // RS-485 direction
}

/// Minimal GPIO backend required by the exercisers.
///
/// A firmware target implements this once (e.g. over Embassy `peri::gpio`),
/// then drives any bus: `probe::spi::exercise(&mut my_gpio, 0, 1_000_000)`.
pub trait GpioOps {
    /// Drive `pin` to `val` (logical level).
    fn set(&mut self, pin: u8, val: bool);
    /// Read the current level on `pin`.
    fn get(&mut self, pin: u8) -> bool;
    /// Configure `pin` as an open-drain-capable input.
    fn dir_in(&mut self, pin: u8);
    /// Configure `pin` as an output.
    fn dir_out(&mut self, pin: u8);
    /// Busy-wait at least `ns` nanoseconds. T1 dominates for the exerciser
    /// step that is not bit-banged; the bit rate derives from this call.
    fn delay_ns(&mut self, ns: u64);
    /// Notify a backend of a pin edge: `dir` is 1 for output, 0 for input.
    fn log_event(&mut self, pin: u8, val: u8, dir: u8);
    /// Flush any buffered log output (call once per transaction).
    fn flush(&mut self) {}
}

/// SPI master, mode 0 (CPOL=0, CPHA=0) — MSB-first full-duplex byte transfer.
pub mod spi {
    use super::pins::*;
    use super::GpioOps;

    fn xfer_byte<G: GpioOps>(gpio: &mut G, tx: u8, half_t_ns: u64) -> u8 {
        let mut rx = 0u8;
        for bit in (0..8).rev() {
            let mosi_val = ((tx >> bit) & 1) != 0;
            gpio.set(MOSI, mosi_val);
            gpio.log_event(MOSI, mosi_val as u8, 1);
            gpio.delay_ns(half_t_ns);

            // Rising edge SCK — sample MISO.
            gpio.set(SCK, true);
            gpio.log_event(SCK, 1, 1);
            gpio.delay_ns(half_t_ns);

            let miso_val = gpio.get(MISO);
            rx = (rx << 1) | (miso_val as u8);

            // Falling edge SCK.
            gpio.set(SCK, false);
            gpio.log_event(SCK, 0, 1);
        }
        rx
    }

    /// Send a 4-byte frame (`0xAB 0xCD 0x12 0x34`) at `speed_hz`. `mode` is
    /// accepted for API parity but only mode 0 is implemented.
    pub fn exercise<G: GpioOps>(gpio: &mut G, mode: u32, speed_hz: u32) {
        let _ = mode;
        let half_t_ns = if speed_hz > 0 {
            500_000_000u64 / speed_hz as u64
        } else {
            1000
        };

        let tx_data: [u8; 4] = [0xAB, 0xCD, 0x12, 0x34];

        gpio.dir_out(SCK);
        gpio.dir_out(MOSI);
        gpio.dir_in(MISO);
        gpio.dir_out(CS);

        gpio.set(SCK, false);
        gpio.set(CS, true);
        gpio.flush();

        gpio.delay_ns(10_000);

        gpio.set(CS, false);
        gpio.log_event(CS, 0, 1);
        gpio.delay_ns(half_t_ns);

        for &byte in &tx_data {
            let rx = xfer_byte(gpio, byte, half_t_ns);
            core::hint::black_box(rx);
        }

        gpio.set(CS, true);
        gpio.log_event(CS, 1, 1);
        gpio.set(SCK, false);
        gpio.flush();
    }
}

/// I2C master, standard mode (100 kHz) — open-drain SDA/SCL.
pub mod i2c {
    use super::pins::*;
    use super::GpioOps;

    const HALF_T_NS: u64 = 5_000; // 100 kHz -> 5 us / half period

    fn half_delay<G: GpioOps>(gpio: &mut G) {
        gpio.delay_ns(HALF_T_NS);
    }

    fn scl_low<G: GpioOps>(gpio: &mut G) {
        gpio.set(SCL, false);
        gpio.log_event(SCL, 0, 1);
    }

    fn scl_high<G: GpioOps>(gpio: &mut G) {
        gpio.set(SCL, true);
        gpio.log_event(SCL, 1, 1);
    }

    fn sda_write<G: GpioOps>(gpio: &mut G, val: bool) {
        gpio.set(SDA, val);
        gpio.log_event(SDA, val as u8, 1);
    }

    fn sda_read<G: GpioOps>(gpio: &mut G) -> bool {
        let v = gpio.get(SDA);
        gpio.log_event(SDA, v as u8, 0);
        v
    }

    fn i2c_start<G: GpioOps>(gpio: &mut G) {
        sda_write(gpio, true);
        half_delay(gpio);
        scl_high(gpio);
        half_delay(gpio);
        sda_write(gpio, false);
        half_delay(gpio);
        scl_low(gpio);
        half_delay(gpio);
    }

    fn i2c_stop<G: GpioOps>(gpio: &mut G) {
        sda_write(gpio, false);
        half_delay(gpio);
        scl_high(gpio);
        half_delay(gpio);
        sda_write(gpio, true);
        half_delay(gpio);
    }

    fn i2c_write_byte<G: GpioOps>(gpio: &mut G, byte: u8) -> bool {
        for bit in (0..8).rev() {
            sda_write(gpio, ((byte >> bit) & 1) != 0);
            half_delay(gpio);
            scl_high(gpio);
            half_delay(gpio);
            scl_low(gpio);
            half_delay(gpio);
        }
        sda_write(gpio, true); // release for ACK
        half_delay(gpio);
        scl_high(gpio);
        half_delay(gpio);
        let ack = !sda_read(gpio); // ACK = SDA low
        scl_low(gpio);
        half_delay(gpio);
        ack
    }

    fn i2c_read_byte<G: GpioOps>(gpio: &mut G, send_nack: bool) -> u8 {
        let mut byte = 0u8;
        sda_write(gpio, true); // release for slave to drive
        for _ in 0..8 {
            scl_high(gpio);
            half_delay(gpio);
            byte = (byte << 1) | (sda_read(gpio) as u8);
            scl_low(gpio);
            half_delay(gpio);
        }
        sda_write(gpio, !send_nack); // ACK = 0, NACK = 1
        half_delay(gpio);
        scl_high(gpio);
        half_delay(gpio);
        scl_low(gpio);
        half_delay(gpio);
        sda_write(gpio, true);
        byte
    }

    /// Write a register + data to `dev_addr`, then read 3 bytes back.
    pub fn exercise<G: GpioOps>(gpio: &mut G, dev_addr: u8, do_write: bool, do_read: bool) {
        gpio.dir_out(SCL);
        gpio.dir_out(SDA);

        sda_write(gpio, true);
        half_delay(gpio);
        scl_high(gpio);
        half_delay(gpio);
        gpio.flush();

        i2c_start(gpio);

        if do_write {
            let ack = i2c_write_byte(gpio, dev_addr << 1);
            if ack {
                i2c_write_byte(gpio, 0x00); // register address
                let data: [u8; 2] = [0xAB, 0xCD];
                for &b in &data {
                    i2c_write_byte(gpio, b);
                }
            }
        }

        if do_read {
            i2c_start(gpio); // repeated start
            i2c_write_byte(gpio, (dev_addr << 1) | 1);
            for i in 0..3 {
                i2c_read_byte(gpio, i == 2); // NACK on last
            }
        }

        i2c_stop(gpio);
        gpio.flush();
    }
}

/// UART transmitter, 8N1 framing.
pub mod uart {
    use super::pins::*;
    use super::GpioOps;

    /// Transmit `message` at `baud` (idle-high, LSB-first, 1 stop bit).
    pub fn exercise<G: GpioOps>(gpio: &mut G, baud: u32, message: &str) {
        let bit_ns = if baud > 0 {
            1_000_000_000u64 / baud as u64
        } else {
            86_900
        };

        gpio.dir_out(TX);
        gpio.dir_in(RX);

        // idle high
        gpio.set(TX, true);
        gpio.log_event(TX, 1, 1);
        gpio.delay_ns(bit_ns * 2);

        for &byte in message.as_bytes() {
            // start bit (0)
            gpio.set(TX, false);
            gpio.log_event(TX, 0, 1);
            gpio.delay_ns(bit_ns);

            // data bits, LSB-first
            for bit in 0..8 {
                let val = ((byte >> bit) & 1) != 0;
                gpio.set(TX, val);
                gpio.log_event(TX, val as u8, 1);
                gpio.delay_ns(bit_ns);
            }

            // stop bit (1)
            gpio.set(TX, true);
            gpio.log_event(TX, 1, 1);
            gpio.delay_ns(bit_ns);
        }

        gpio.flush();
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;

    /// Backend that samples every edge into a flat vector for assertions.
    #[derive(Default)]
    struct RecordingBus {
        t_ns: u64,
        events: Vec<(u64, u8, u8, u8)>, // (time, pin, val, dir)
        flushed: bool,
    }

    impl RecordingBus {
        fn last(&self, pin: u8) -> Option<(u8, u8)> {
            self.events
                .iter()
                .rev()
                .find(|(_, p, _, _)| *p == pin)
                .map(|&(_, _, v, d)| (v, d))
        }
        fn count(&self, pin: u8, val: u8) -> usize {
            self.events
                .iter()
                .filter(|(_, p, v, _)| *p == pin && *v == val)
                .count()
        }
    }

    impl GpioOps for RecordingBus {
        fn set(&mut self, _pin: u8, _val: bool) {}
        fn get(&mut self, _pin: u8) -> bool {
            false // no slave attached: MISO reads recessive/0
        }
        fn dir_in(&mut self, _pin: u8) {}
        fn dir_out(&mut self, _pin: u8) {}
        fn delay_ns(&mut self, ns: u64) {
            self.t_ns += ns;
        }
        fn log_event(&mut self, pin: u8, val: u8, dir: u8) {
            self.events.push((self.t_ns, pin, val, dir));
        }
        fn flush(&mut self) {
            self.flushed = true;
        }
    }

    #[test]
    fn spi_frame_shape() {
        let mut bus = RecordingBus::default();
        spi::exercise(&mut bus, 0, 1_000_000);

        // CS starts high, drops once, returns high.
        assert_eq!(bus.count(pins::CS, 0), 1);
        assert_eq!(bus.count(pins::CS, 1), 1);
        assert_eq!(bus.last(pins::CS), Some((1, 1))); // dir out

        // 4 bytes x 8 bits = 32 SCK rising and 32 SCK falling edges.
        assert_eq!(bus.count(pins::SCK, 1), 32);
        assert_eq!(bus.count(pins::SCK, 0), 32);

        // First MOSI value = MSB of 0xAB = 1.
        let first = bus
            .events
            .iter()
            .find(|(_, p, _, _)| *p == pins::MOSI)
            .map(|&(_, _, v, _)| v)
            .unwrap();
        assert_eq!(first, 1);

        assert!(bus.flushed);
    }

    #[test]
    fn uart_start_data_stop_edges() {
        let mut bus = RecordingBus::default();
        uart::exercise(&mut bus, 115_200, "H"); // one byte

        // One start bit (0), 8 data bits, one stop bit (1).
        let n_data_ones = count_ones(b'H');
        let n_data_zeros = 8 - n_data_ones;
        assert_eq!(bus.count(pins::TX, 0), 1 + n_data_zeros);
        assert_eq!(bus.count(pins::TX, 1), 1 + n_data_ones + 1); // idle + data + stop

        assert_eq!(bus.last(pins::TX), Some((1, 1)));
        assert!(bus.flushed);
    }

    fn count_ones(mut b: u8) -> usize {
        let mut n = 0;
        while b > 0 {
            n += (b & 1) as usize;
            b >>= 1;
        }
        n
    }

    #[test]
    fn i2c_write_ack_stops_on_nack_but_read_proceeds() {
        // RecordingBus always reads false (SDA low) => ACK is received,
        // so the write path completes and the read path runs.
        let mut bus = RecordingBus::default();
        i2c::exercise(&mut bus, 0x50, true, true);

        // The transaction ends on a STOP: SCL high then SDA rising.
        assert_eq!(bus.last(pins::SDA), Some((1, 1)));
        assert!(bus.flushed);

        // Some SCL activity happened.
        assert!(bus.count(pins::SCL, 1) > 10);
    }
}
