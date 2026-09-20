#![feature(type_alias_impl_trait)]

use vita_newlib_shims as _;

mod api;
mod api_xbox;
mod app;
mod i18n;
mod input;
mod jobs;
mod safe_memory;
mod settings;
mod shell;
mod streaming;

use api_xbox::api::{
    ApiClient, ApiClientConfig, Console, ConsolesResponse, StreamKind, WaitTimeResponse,
};
use api_xbox::auth::{DeviceCodeAuth, DeviceCodePoll, MsalAuth, StreamingCredentials, XboxProfile};
use api_xbox::stream::{Stream, StreamState};
use app::{App, AppCommand, AppState, InputCommand, NavigationCommand};
use settings::Locale;

#[used]
#[unsafe(export_name = "sceUserMainThreadStackSize")]
pub static SCE_USER_MAIN_THREAD_STACK_SIZE: u32 = 4 * 1024 * 1024;

#[used]
#[unsafe(export_name = "sceLibcHeapSize")]
pub static SCE_LIBC_HEAP_SIZE: u32 = 40 * 1024 * 1024;

#[used]
#[unsafe(export_name = "_newlib_heap_size_user")]
pub static NEWLIB_HEAP_SIZE_USER: u32 = 192 * 1024 * 1024;

mod fs_utils {
    use anyhow::{Context, Result};

    /// Removes `path` before writing - `std::fs::write` alone doesn't reliably truncate an
    /// existing file on the Vita's newlib filesystem.
    pub fn write_file_truncating(path: &str, data: impl AsRef<[u8]>) -> Result<()> {
        let _ = std::fs::remove_file(path);
        std::fs::write(path, data).with_context(|| format!("failed to write {path}"))
    }
}

fn main() -> anyhow::Result<()> {
    let _app_util = safe_memory::AppUtil::initialize()?;
    // Pin the main/UI thread to CPU0 so the rtc-recv RX thread (core 1) is never
    // starved by render/GUI load; failures are non-fatal (mask may be restricted).
    unsafe {
        let result = vitasdk_sys::sceKernelChangeThreadCpuAffinityMask(
            vitasdk_sys::SCE_KERNEL_THREAD_ID_SELF as _,
            0b0001,
        );
        if result != 0 {
            eprintln!("main thread affinity pin failed: {result}");
        }
    }
    // Raise clocks / enable wireless mode for the whole app lifetime; restored on exit.
    let _streaming_power = shell::power::engage();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let app = App::new()?;
        shell::run(app).await
    })
}
