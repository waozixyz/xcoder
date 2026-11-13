use super::TRACKED_UPSTREAM_CLIENT_VERSION;
use super::client_version_to_whole;
use super::whole_client_version;
use pretty_assertions::assert_eq;

#[test]
fn development_version_reports_tracked_upstream_release() {
    assert_eq!(
        whole_client_version("0", "0", "0"),
        TRACKED_UPSTREAM_CLIENT_VERSION
    );
}

#[test]
fn released_version_is_reported_unchanged() {
    assert_eq!(whole_client_version("0", "170", "2"), "0.170.2");
}

#[test]
fn catalog_never_reports_the_development_placeholder() {
    assert_ne!(client_version_to_whole(), "0.0.0");
}
