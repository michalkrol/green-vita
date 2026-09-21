//! Streaming power profile: raises CPU/GPU clocks, enables wireless
//! performance mode, and keeps auto-suspend/screen-off at bay while the app
//! runs. Mirrors the clock profile vita-moonlight-relay validated for smooth
//! streaming (ARM 444 / BUS 222 / GPU 222 / GPU-XBAR 166 MHz + wireless mode
//! + a 10 s power-tick heartbeat); previous values are restored on drop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vitasdk_sys::{
    sceKernelPowerTick, scePowerGetArmClockFrequency, scePowerGetBusClockFrequency,
    scePowerGetGpuClockFrequency, scePowerGetGpuXbarClockFrequency, scePowerGetUsingWireless,
    scePowerSetArmClockFrequency, scePowerSetBusClockFrequency, scePowerSetGpuClockFrequency,
    scePowerSetGpuXbarClockFrequency, scePowerSetUsingWireless,
    SCE_KERNEL_POWER_TICK_DISABLE_AUTO_SUSPEND, SCE_KERNEL_POWER_TICK_DISABLE_OLED_OFF,
};

const ARM_CLOCK_MHZ: i32 = 444;
const BUS_CLOCK_MHZ: i32 = 222;
const GPU_CLOCK_MHZ: i32 = 222;
const GPU_XBAR_CLOCK_MHZ: i32 = 166;
const POWER_TICK_INTERVAL: Duration = Duration::from_secs(10);
const POWER_TICK_MASK: u32 =
    (SCE_KERNEL_POWER_TICK_DISABLE_AUTO_SUSPEND | SCE_KERNEL_POWER_TICK_DISABLE_OLED_OFF) as u32;

/// Disabled: clock/wireless overrides regressed drift during testing.
pub(crate) const ENABLED: bool = false;

struct SavedClocks {
    arm: i32,
    bus: i32,
    gpu: i32,
    gpu_xbar: i32,
    wireless: i32,
}

/// Held for the app's lifetime; restores the previous power state on drop.
pub struct StreamingPowerGuard {
    stop: Arc<AtomicBool>,
    ticker: Option<thread::JoinHandle<()>>,
    saved: SavedClocks,
}

pub fn engage() -> StreamingPowerGuard {
    if !ENABLED {
        return StreamingPowerGuard {
            stop: Arc::new(AtomicBool::new(true)),
            ticker: None,
            saved: SavedClocks {
                arm: 0,
                bus: 0,
                gpu: 0,
                gpu_xbar: 0,
                wireless: 0,
            },
        };
    }
    let saved = SavedClocks {
        arm: unsafe { scePowerGetArmClockFrequency() },
        bus: unsafe { scePowerGetBusClockFrequency() },
        gpu: unsafe { scePowerGetGpuClockFrequency() },
        gpu_xbar: unsafe { scePowerGetGpuXbarClockFrequency() },
        wireless: unsafe { scePowerGetUsingWireless() },
    };

    unsafe {
        let _ = scePowerSetArmClockFrequency(ARM_CLOCK_MHZ);
        let _ = scePowerSetBusClockFrequency(BUS_CLOCK_MHZ);
        let _ = scePowerSetGpuClockFrequency(GPU_CLOCK_MHZ);
        let _ = scePowerSetGpuXbarClockFrequency(GPU_XBAR_CLOCK_MHZ);
        let _ = scePowerSetUsingWireless(1);
    }
    eprintln!(
        "streaming power profile engaged (was arm:{} bus:{} gpu:{} xbar:{} wireless:{})",
        saved.arm, saved.bus, saved.gpu, saved.gpu_xbar, saved.wireless
    );

    let stop = Arc::new(AtomicBool::new(false));
    let ticker_stop = Arc::clone(&stop);
    let ticker = thread::Builder::new()
        .name("power-tick".into())
        .spawn(move || {
            while !ticker_stop.load(Ordering::Relaxed) {
                unsafe {
                    let _ = sceKernelPowerTick(POWER_TICK_MASK);
                }
                thread::sleep(POWER_TICK_INTERVAL);
            }
        })
        .ok();

    StreamingPowerGuard {
        stop,
        ticker,
        saved,
    }
}

impl Drop for StreamingPowerGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
        unsafe {
            let _ = scePowerSetArmClockFrequency(self.saved.arm);
            let _ = scePowerSetBusClockFrequency(self.saved.bus);
            let _ = scePowerSetGpuClockFrequency(self.saved.gpu);
            let _ = scePowerSetGpuXbarClockFrequency(self.saved.gpu_xbar);
            let _ = scePowerSetUsingWireless(self.saved.wireless);
        }
        eprintln!("streaming power profile restored");
    }
}
