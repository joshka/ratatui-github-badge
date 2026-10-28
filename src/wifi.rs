//! RM2 Wi-Fi startup runs independently so missing Wi-Fi never blocks the badge UI.
use core::{
    cell::RefCell,
    sync::atomic::{AtomicBool, Ordering},
};
use critical_section::Mutex;
use cyw43::{JoinOptions, ScanOptions, aligned_bytes};
use cyw43_pio::{PioSpi, RM2_CLOCK_DIVIDER};
use embassy_executor::Spawner;
use embassy_rp::{
    Peri, bind_interrupts, dma,
    gpio::{Level, Output},
    peripherals::{DMA_CH0, PIN_23, PIN_24, PIN_25, PIN_29, PIO0},
    pio::{InterruptHandler, Pio},
};
use embassy_time::{Duration, Timer, with_timeout};
use static_cell::StaticCell;

#[derive(Clone, Copy)]
pub struct Status {
    pub phase: &'static str,
    pub ssid: [u8; 32],
    pub ssid_len: usize,
    pub ip: [u8; 4],
    pub found: u16,
}
static STATUS: Mutex<RefCell<Status>> = Mutex::new(RefCell::new(Status {
    phase: "Off / ENTER to connect",
    ssid: [0; 32],
    ssid_len: 0,
    ip: [0; 4],
    found: 0,
}));

static START_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn request_start() {
    START_REQUESTED.store(true, Ordering::Relaxed);
}

pub fn status() -> Status {
    critical_section::with(|cs| *STATUS.borrow(cs).borrow())
}
fn update(f: impl FnOnce(&mut Status)) {
    critical_section::with(|cs| f(&mut STATUS.borrow(cs).borrow_mut()));
}
fn phase(value: &'static str) {
    update(|s| s.phase = value);
}

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
});

#[embassy_executor::task]
async fn radio(
    runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>>,
) -> ! {
    runner.run().await
}
#[embassy_executor::task]
async fn network(mut runner: embassy_net::Runner<'static, cyw43::NetDriver<'static>>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
pub async fn run(
    spawner: Spawner,
    pio: Peri<'static, PIO0>,
    dma: Peri<'static, DMA_CH0>,
    power: Peri<'static, PIN_23>,
    cs: Peri<'static, PIN_25>,
    data: Peri<'static, PIN_24>,
    clock: Peri<'static, PIN_29>,
) {
    // Keep the offline badge completely independent of radio initialization.
    while !START_REQUESTED.load(Ordering::Relaxed) {
        Timer::after_millis(100).await;
    }
    phase("Starting radio");
    let mut pio = Pio::new(pio, Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        RM2_CLOCK_DIVIDER,
        pio.irq0,
        Output::new(cs, Level::High),
        data,
        clock,
        dma::Channel::new(dma, Irqs),
    );
    static STATE: StaticCell<cyw43::State> = StaticCell::new();
    let (device, mut control, runner) = cyw43::new(
        STATE.init(cyw43::State::new()),
        Output::new(power, Level::Low),
        spi,
        aligned_bytes!("../assets/wifi/43439A0.bin"),
        aligned_bytes!("../assets/wifi/nvram.bin"),
    )
    .await;
    spawner.spawn(radio(runner).unwrap());
    control
        .init(aligned_bytes!("../assets/wifi/43439A0_clm.bin"))
        .await;
    control
        .set_power_management(cyw43::PowerManagementMode::PowerSave)
        .await;
    static RESOURCES: StaticCell<embassy_net::StackResources<2>> = StaticCell::new();
    let seed = embassy_rp::clocks::RoscRng.next_u64();
    let (stack, runner) = embassy_net::new(
        device,
        embassy_net::Config::dhcpv4(Default::default()),
        RESOURCES.init(embassy_net::StackResources::new()),
        seed,
    );
    spawner.spawn(network(runner).unwrap());
    let password = include_str!(concat!(env!("OUT_DIR"), "/wifi-password.txt"));
    if password.is_empty() {
        phase("Wi-Fi password not configured");
        return;
    }
    loop {
        phase("Scanning for GitHub Guest");
        update(|s| {
            s.found = 0;
            s.ssid_len = 0;
            s.ip = [0; 4];
        });
        let mut target = None;
        {
            let mut scan = control.scan(ScanOptions::default()).await;
            let scan_deadline = embassy_time::Instant::now() + Duration::from_secs(15);
            while let Ok(Some(bss)) = embassy_time::with_deadline(scan_deadline, scan.next()).await
            {
                let length = usize::from(bss.ssid_len).min(32);
                update(|s| {
                    s.found = s.found.saturating_add(1);
                });
                let name = &bss.ssid[..length];
                // Preserve the AP's exact spelling; match the supplied name ignoring case/spaces.
                let mut normalized = [0; 32];
                let mut n = 0;
                for &b in name {
                    if b.is_ascii_alphanumeric() {
                        normalized[n] = b.to_ascii_lowercase();
                        n += 1;
                    }
                }
                if &normalized[..n] == b"githubguest" {
                    target = Some((bss.ssid, length));
                }
            }
        }
        let Some((ssid, len)) = target else {
            phase("GitHub Guest not found / retrying");
            Timer::after_secs(15).await;
            continue;
        };
        update(|s| {
            s.ssid = ssid;
            s.ssid_len = len;
        });
        phase("Joining guest Wi-Fi");
        let name = core::str::from_utf8(&ssid[..len]).unwrap_or("GitHub Guest");
        if !matches!(
            with_timeout(
                Duration::from_secs(30),
                control.join(name, JoinOptions::new(password.as_bytes()))
            )
            .await,
            Ok(Ok(()))
        ) {
            phase("Join failed / retrying");
            Timer::after_secs(10).await;
            continue;
        }
        phase("Connected / requesting IP");
        if with_timeout(Duration::from_secs(30), stack.wait_config_up())
            .await
            .is_err()
        {
            phase("DHCP timed out / retrying");
            control.leave().await;
            Timer::after_secs(10).await;
            continue;
        }
        if let Some(config) = stack.config_v4() {
            update(|s| s.ip = config.address.address().octets());
        }
        phase("Connected / IP assigned");
        while stack.is_link_up() && stack.is_config_up() {
            Timer::after_secs(2).await;
        }
        phase("Disconnected / reconnecting");
        control.leave().await;
    }
}
