//! LilyGo T-A7670X board implementation.
//!
//! Pin defaults are from config.toml; can be overridden for other boards
//! simply by creating a new Board impl with different constants.

use super::{Board, BoardError};
use crate::config::Config;
use crate::creds::RuntimeConfig;
use crate::modem::{
    a76xx::{at::HardwareAtPort, A76xxModem},
    ModemPort,
};
use esp_idf_hal::{
    peripherals::Peripherals,
    uart::{config::Config as UartConfig, UartDriver},
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// LilyGo T-A7670X: BOARD_POWERON_PIN (GPIO12) powers the modem rail.
const BOARD_POWERON_PIN: u8 = 12;
/// MODEM_RESET_PIN (GPIO5, active-HIGH on T-A7670X). LilyGo gpio.h reference.
const MODEM_RESET_PIN: u8 = 5;

pub struct TA7670X;

impl Board for TA7670X {
    fn init(&self, _peripherals: &mut Peripherals) -> Result<(), BoardError> {
        use esp_idf_hal::gpio::AnyOutputPin;

        // BOARD_POWERON_PIN drives the modem's power rail. Without it the modem
        // gets no supply voltage and will never respond to AT commands.
        // (LilyGo gpio.h: "The modem power switch must be set to HIGH for the
        //  modem to supply power.")
        let mut poweron = unsafe {
            esp_idf_hal::gpio::PinDriver::output(AnyOutputPin::steal(BOARD_POWERON_PIN))
                .map_err(|e| BoardError::Gpio(e.to_string()))?
        };
        poweron
            .set_high()
            .map_err(|e| BoardError::Gpio(e.to_string()))?;
        std::thread::sleep(Duration::from_millis(200)); // let rail stabilise

        // On warm reboots (watchdog, panic, /restart command) the ESP32
        // resets but the modem power rail stays up — the modem is already on
        // and responsive. Skip the 7s RESET+PWRKEY sequence; the AT probe
        // loop in A76xxModem::init() will find it immediately.
        // Only PowerOn and Brownout guarantee the modem is truly off.
        let cold_boot = matches!(
            esp_idf_hal::reset::ResetReason::get(),
            esp_idf_hal::reset::ResetReason::PowerOn | esp_idf_hal::reset::ResetReason::Brownout
        );

        if cold_boot {
            // MODEM_RESET_PIN — hard-reset the modem.
            // Sequence from LilyGo C++ reference (MODEM_RESET_LEVEL=HIGH for T-A7670X):
            //   LOW for 100 ms → HIGH for 2600 ms → LOW.
            let mut reset_pin = unsafe {
                esp_idf_hal::gpio::PinDriver::output(AnyOutputPin::steal(MODEM_RESET_PIN))
                    .map_err(|e| BoardError::Gpio(e.to_string()))?
            };
            reset_pin
                .set_low()
                .map_err(|e| BoardError::Gpio(e.to_string()))?;
            std::thread::sleep(Duration::from_millis(100));
            reset_pin
                .set_high()
                .map_err(|e| BoardError::Gpio(e.to_string()))?;
            std::thread::sleep(Duration::from_millis(2600));
            reset_pin
                .set_low()
                .map_err(|e| BoardError::Gpio(e.to_string()))?;
            drop(reset_pin);

            // PWRKEY pulse — LOW for 100 ms, HIGH for 1000 ms, back to LOW.
            // A7670G datasheet: minimum PWRKEY HIGH time for power-on is 1000 ms.
            // (100 ms is only enough to power OFF an already-running modem.)
            let mut pwrkey = unsafe {
                esp_idf_hal::gpio::PinDriver::output(AnyOutputPin::steal(Config::PWRKEY_PIN))
                    .map_err(|e| BoardError::Gpio(e.to_string()))?
            };
            pwrkey
                .set_low()
                .map_err(|e| BoardError::Gpio(e.to_string()))?;
            std::thread::sleep(Duration::from_millis(100));
            pwrkey
                .set_high()
                .map_err(|e| BoardError::Gpio(e.to_string()))?;
            std::thread::sleep(Duration::from_millis(1000));
            pwrkey
                .set_low()
                .map_err(|e| BoardError::Gpio(e.to_string()))?;
            // No post-PWRKEY sleep: the AT probe loop in A76xxModem::init()
            // retries for up to 30 s, so it acts as the wait.
            log::info!("[board] cold boot — modem power-on sequence complete");
        } else {
            log::info!("[board] warm reboot — modem already powered, skipping power-on sequence");
        }

        // Keep poweron pin driven HIGH for the entire program lifetime.
        // Dropping it here would let the GPIO go to a default state — keep it
        // alive via core::mem::forget so the rail stays asserted.
        core::mem::forget(poweron);

        Ok(())
    }

    fn build_modem_port(
        &self,
        peripherals: &mut Peripherals,
        config: &RuntimeConfig,
    ) -> Result<Arc<Mutex<dyn ModemPort + Send>>, BoardError> {
        let uart_config = UartConfig::new().baudrate(esp_idf_hal::units::Hertz(Config::UART_BAUD));

        // Build UartDriver and extend its lifetime to 'static.
        //
        // SAFETY: `Peripherals::take()` is called once in main() and the resulting
        // struct lives for the entire program duration. The 'd lifetime in UartDriver<'d>
        // is a borrow-checker exclusivity mechanism to prevent two drivers on the same
        // UART port — not an indication the peripheral will be dropped. Since this
        // UartDriver is the sole user of UART1 for the program's lifetime, the transmute
        // to 'static is sound.
        let uart: UartDriver<'static> = unsafe {
            use esp_idf_hal::gpio::AnyIOPin;
            let tx = AnyIOPin::steal(Config::UART_TX);
            let rx = AnyIOPin::steal(Config::UART_RX);
            let driver = UartDriver::new(
                peripherals.uart1.reborrow(),
                tx,
                rx,
                Option::<AnyIOPin>::None,
                Option::<AnyIOPin>::None,
                &uart_config,
            )
            .map_err(|e| BoardError::Uart(e.to_string()))?;
            std::mem::transmute(driver)
        };

        let port = HardwareAtPort::new(uart);
        let mut modem = A76xxModem::new(port);
        modem
            .init(config.cellular_data, &config.sim_pin)
            .map_err(|e| BoardError::Uart(e.to_string()))?;

        Ok(Arc::new(Mutex::new(modem)))
    }
}
