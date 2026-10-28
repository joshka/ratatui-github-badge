//! The CYW43439 radio (an RM2 module): Bluetooth LE for nearby hosts, and Wi-Fi for the relay.
//! Which of them run comes from `badge.toml`; with neither, the radio stays powered off.
use badge_ui::config::{BLE_KEY, BLE_NAME, RELAY};
use cyw43::{A4, Aligned};
use cyw43_pio::{PioSpi, RM2_CLOCK_DIVIDER};
use embassy_rp::executor::Executor;
use embassy_futures::join::join;
use embassy_rp::{
    Peri, bind_interrupts,
    gpio::{Level, Output},
    multicore::{Stack, spawn_core1},
    peripherals::{CORE1, DMA_CH0, DMA_CH1, PIN_23, PIN_24, PIN_25, PIN_29, PIO0, TRNG},
    pio::{self, Pio},
    trng::{self, Trng},
};
use static_cell::{ConstStaticCell, StaticCell};

mod auth;
mod ble;
mod relay;

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => pio::InterruptHandler<PIO0>;
    TRNG_IRQ => trng::InterruptHandler<TRNG>;
    DMA_IRQ_0 => embassy_rp::dma::InterruptHandler<DMA_CH0>, embassy_rp::dma::InterruptHandler<DMA_CH1>;
});

/// Pins from the pico-sdk's `pimoroni_tufty2350.h`, the same as a Pico 2 W.
pub struct Pins {
    pub pio: Peri<'static, PIO0>,
    pub dma_tx: Peri<'static, DMA_CH0>,
    /// The patched cyw43-pio reads with its own channel (see vendor/cyw43-pio).
    pub dma_rx: Peri<'static, DMA_CH1>,
    pub trng: Peri<'static, TRNG>,
    pub power: Peri<'static, PIN_23>,
    pub data: Peri<'static, PIN_24>,
    pub cs: Peri<'static, PIN_25>,
    pub clock: Peri<'static, PIN_29>,
}

/// The firmware blobs need 4-byte alignment; the crate only provides plain byte arrays.
static WIFI_FW: &Aligned<A4, [u8]> = &Aligned(*cyw43_firmware::CYW43_43439A0);
static BT_FW: &Aligned<A4, [u8]> = &Aligned(*cyw43_firmware::CYW43_43439A0_BTFW);
static NVRAM: &Aligned<A4, [u8]> = &Aligned(*include_bytes!("../firmware/nvram_rp2040.bin"));

/// The radio runs on core 1 with its own executor. Uploading its firmware takes thousands of
/// bus transfers and its driver polls busily, so sharing core 0 either starved the main loop
/// into watchdog resets (on an interrupt executor) or crawled between frames (on the main one).
pub fn start(core1: Peri<'static, CORE1>, safe_mode: bool, pins: Pins) {
    if BLE_NAME.is_none() && RELAY.is_none() {
        return;
    }
    if safe_mode {
        crate::link::write("radio: off for this boot (safe mode after a stall or panic)\r\n");
        return;
    }
    static STACK: ConstStaticCell<Stack<CORE1_STACK>> = ConstStaticCell::new(Stack::new());
    spawn_core1(core1, STACK.take(), move || {
        static EXECUTOR: StaticCell<Executor> = StaticCell::new();
        EXECUTOR.init(Executor::new()).run(|spawner| spawner.spawn(radio_main(pins).unwrap()))
    });
}

/// Polling the radio task reserves about 14 KB of stack in one go (the BLE and relay state
/// passes through it), on top of the driver's own use.
const CORE1_STACK: usize = 32768;
/// `cyw43::State` isn't `Send`, but only core 1 ever touches it.
struct CoreState(cyw43::State);
unsafe impl Send for CoreState {}

/// Everything radio-related runs in this one task on core 1.
#[embassy_executor::task]
async fn radio_main(pins: Pins) {
    let mut pio = Pio::new(pins.pio, Irqs);
    let spi = Yielding::new(PioSpi::new(
        &mut pio.common,
        pio.sm0,
        // Faster dividers garble the bus to an RM2 module.
        RM2_CLOCK_DIVIDER,
        pio.irq0,
        Output::new(pins.cs, Level::High),
        pins.data,
        pins.clock,
        embassy_rp::dma::Channel::new(pins.dma_tx, Irqs),
        embassy_rp::dma::Channel::new(pins.dma_rx, Irqs),
    ));
    // Built in place: at about 21 KB it would otherwise pass through core 1's small stack.
    static STATE: ConstStaticCell<CoreState> = ConstStaticCell::new(CoreState(cyw43::State::new()));
    let state = &mut STATE.take().0;
    let power = Output::new(pins.power, Level::Low);
    let mut rng = Trng::new(pins.trng, Irqs, trng::Config::default());
    let mut seed = [0; 32];
    // The blocking read: the async one never completed here. It only spins briefly.
    rand_core::RngCore::fill_bytes(&mut rng, &mut seed);
    let (net, bt, mut control, runner) =
        cyw43::new_with_bluetooth(state, power, spi, WIFI_FW, BT_FW, NVRAM).await;

    join(async {
        control.init(cyw43_firmware::CYW43_43439A0_CLM).await;
        control
            .set_power_management(cyw43::PowerManagementMode::PowerSave)
            .await;
        crate::link::write("radio: ready\r\n");
        join(
            async {
                if let Some(config) = RELAY {
                    relay::run(net, &mut control, &mut rng, config).await;
                }
            },
            async {
                if let Some(name) = BLE_NAME {
                    ble::run(bt, name, BLE_KEY, seed).await;
                }
            },
        )
        .await;
    }, runner.run())
    .await;
}

/// With Bluetooth on, cyw43 busy-polls the radio instead of waiting for its interrupt, and short
/// bus transfers finish within a single poll, so its loop can spin without ever returning to
/// the executor. Pausing every few transfers lets the rest of the radio task run, and stops
/// core 1 hogging the flash and memory buses that core 0 draws frames through.
struct Yielding<S> {
    spi: S,
    transfers: u8,
}

impl<S> Yielding<S> {
    fn new(spi: S) -> Self {
        Self { spi, transfers: 0 }
    }

    async fn tick(&mut self) {
        self.transfers = self.transfers.wrapping_add(1);
        if self.transfers.is_multiple_of(16) {
            embassy_time::Timer::after_micros(500).await;
        }
    }
}

impl<S: cyw43::SpiBusCyw43> cyw43::SpiBusCyw43 for Yielding<S> {
    async fn cmd_write(&mut self, write: &[u32]) -> u32 {
        let status = self.spi.cmd_write(write).await;
        self.tick().await;
        status
    }

    async fn cmd_read(&mut self, write: u32, read: &mut [u32]) -> u32 {
        let status = self.spi.cmd_read(write, read).await;
        self.tick().await;
        status
    }

    async fn wait_for_event(&mut self) {
        self.spi.wait_for_event().await
    }
}
