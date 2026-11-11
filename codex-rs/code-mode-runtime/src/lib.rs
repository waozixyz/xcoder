mod cell_actor;
mod native_dispatch;
mod native_sandbox;
mod output;
mod provider;
mod runtime;
mod service;
mod session_runtime;
#[cfg(feature = "ui")]
mod ui;
#[cfg(feature = "ui")]
mod v8_init;

pub(crate) type TaskFailureHandler = std::sync::Arc<dyn Fn(String) + Send + Sync>;

pub use codex_code_mode_protocol::*;
pub use provider::NativeCodeModeSessionProvider;
pub use service::InProcessCodeModeSession;
#[cfg(feature = "ui")]
pub use ui::run as run_ui_sandbox;
#[cfg(not(feature = "ui"))]
pub fn run_ui_sandbox() -> std::io::Result<()> {
    Err(std::io::Error::other(
        "JavaScript UI plugins require the optional ui build feature",
    ))
}
#[cfg(feature = "ui")]
pub use v8_init::V8JitMode;
#[cfg(feature = "ui")]
pub use v8_init::initialize_v8;
