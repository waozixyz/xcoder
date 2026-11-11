The changelog can be found on the [releases page](https://github.com/openai/codex/releases).

## Unreleased

- Xcoder executes tool programs natively from Ziran, with tool approvals, cancellation and saved session values.
- Certify installation only after the complete local test suite passes for the exact packaged executables; test pending parallel calls and recovery after callback errors.

- Fix missing command lifecycle events when unified exec cannot create a process, preserving approval rejection and sandbox retry behavior. Thanks @Marvinthebored ([#44557](https://github.com/openai/codex/issues/44557)).
