//! Embedded supervision with isolated native cells. A session allocates only its
//! explicit state; creating one never starts a tool-host process or JS engine.
use crate::InProcessCodeModeSession;
use codex_code_mode_protocol::CodeModeSession;
use codex_code_mode_protocol::CodeModeSessionCellExecutionLimits;
use codex_code_mode_protocol::CodeModeSessionOptions;
use codex_code_mode_protocol::CodeModeSessionProvider;
use codex_code_mode_protocol::CodeModeSessionProviderFuture;
use std::sync::Arc;

#[derive(Default)]
pub struct NativeCodeModeSessionProvider;

impl CodeModeSessionProvider for NativeCodeModeSessionProvider {
    fn availability(&self) -> Result<(), String> {
        crate::native_sandbox::toolchain().map(|_| ())
    }

    fn create_session(&self) -> CodeModeSessionProviderFuture<'_> {
        self.create_session_with_options(CodeModeSessionOptions::default())
    }

    fn create_session_with_limits<'a>(
        &'a self,
        limits: CodeModeSessionCellExecutionLimits,
    ) -> CodeModeSessionProviderFuture<'a> {
        self.create_session_with_options(CodeModeSessionOptions {
            cell_execution_limits: limits,
            ..Default::default()
        })
    }

    fn create_session_with_options<'a>(
        &'a self,
        options: CodeModeSessionOptions,
    ) -> CodeModeSessionProviderFuture<'a> {
        Box::pin(async move {
            self.availability()?;
            Ok(
                Arc::new(InProcessCodeModeSession::with_options(options, None))
                    as Arc<dyn CodeModeSession>,
            )
        })
    }
}
