pub mod cache;
mod catalog;
pub mod collaboration_mode_presets;
pub(crate) mod config;
pub mod manager;
pub mod model_info;
pub mod model_presets;
mod originator;
pub mod test_support;

pub use catalog::CatalogModel;
pub use catalog::ModelOrigin;
pub use codex_protocol::auth::AuthMode;
pub use config::ModelsManagerConfig;
pub use originator::ModelCatalogOriginator;

/// Load the bundled model catalog shipped with `codex-models-manager`.
pub fn bundled_models_response()
-> std::result::Result<codex_protocol::openai_models::ModelsResponse, serde_json::Error> {
    serde_json::from_str(include_str!("../models.json"))
}

/// Upstream Codex release whose model catalog this build tracks.
///
/// Xcoder keeps upstream's `0.0.0` development workspace version, but the
/// models endpoint hides newer models (for example `gpt-6.1-sol`) from
/// `0.0.0` clients. Catalog requests and cache entries therefore report the
/// tracked release instead of the development placeholder.
pub const TRACKED_UPSTREAM_CLIENT_VERSION: &str = "0.169.0";

/// Convert the client version string to a whole version string (e.g. "1.2.3-alpha.4" -> "1.2.3").
pub fn client_version_to_whole() -> String {
    whole_client_version(
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH"),
    )
}

fn whole_client_version(major: &str, minor: &str, patch: &str) -> String {
    let version = format!("{major}.{minor}.{patch}");
    if version == "0.0.0" {
        TRACKED_UPSTREAM_CLIENT_VERSION.to_string()
    } else {
        version
    }
}

#[cfg(test)]
#[path = "client_version_tests.rs"]
mod client_version_tests;
