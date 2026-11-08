//! The release feed: is there a newer buzzr than the one running?
//!
//! `update.rs` names *how* to upgrade the copy you have; this module answers the other half — is there anything newer — the way
//! `gh` and `uv` do: one GET of the repository's "latest release", skipped when a check ran in the last `INTERVAL` (the answer is
//! remembered in a small state file next to the settings) and when the person has opted out (`BUZZR_NO_UPDATE_CHECK`). It is the
//! only thing buzzr fetches outside the relay, nothing but the request itself leaves the machine, and every failure is silent:
//! not knowing about a release is never worth an error on the screen. (That is the one deliberate exception to "fail visibly" —
//! a background check the user did not ask for must not shout.)

use crate::update::InstallSource;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a check stays good. A day, as `gh` uses: releases are not urgent, one request is plenty.
pub const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// The version running now.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// GitHub's "latest release" endpoint for this repository, derived from the same `repository` field in `Cargo.toml` that gives
/// `update::RELEASES_URL`.
pub fn latest_api() -> String {
    let repo = env!("CARGO_PKG_REPOSITORY").trim_end_matches('/');
    match repo.rsplit_once("github.com/") {
        Some((_, path)) => format!("https://api.github.com/repos/{path}/releases/latest"),
        None => format!("{repo}/releases/latest"),
    }
}

/// The check is on unless `BUZZR_NO_UPDATE_CHECK` is set (to anything).
pub fn enabled() -> bool {
    crate::config::env("NO_UPDATE_CHECK").is_none()
}

/// Unix seconds.
pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// What the last check learned, kept between runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct State {
    /// When we last asked (unix seconds); 0 when we never have.
    #[serde(default)]
    pub checked_at: u64,
    /// The newest version we were told about, if any.
    #[serde(default)]
    pub latest: Option<String>,
}

impl State {
    /// Read the state file. A missing or unreadable one is simply "nothing known yet".
    pub fn load(path: &Path) -> State {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        crate::config::write_private(path, &(serde_json::to_string(self).expect("a json value serialises") + "\n"))
    }

    /// Is another check allowed now?
    pub fn is_due(&self, now: u64) -> bool {
        now.saturating_sub(self.checked_at) >= INTERVAL.as_secs()
    }

    /// The remembered version, if it really is later than `current`.
    pub fn newer_than(&self, current: &str) -> Option<&str> {
        let latest = self.latest.as_deref()?;
        is_newer(latest, current).then_some(latest)
    }
}

/// Ask GitHub for the latest release's version. The thin networked half; everything it hands back is read by `parse_latest`.
pub async fn fetch() -> Result<String, String> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().map_err(|e| e.to_string())?;
    let body = client
        .get(latest_api())
        .header("User-Agent", env!("CARGO_PKG_NAME")) // the API refuses a request without one
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    parse_latest(&body)
}

/// One throttled check: ask, remember that we asked, and return the newer version when there is one.
pub async fn check_and_store(state_path: &Path) -> Option<String> {
    let mut state = State::load(state_path);
    state.checked_at = now_secs();
    let newer = match fetch().await {
        Ok(version) if is_newer(&version, CURRENT) => {
            state.latest = Some(version.clone());
            Some(version)
        }
        Ok(_) => {
            state.latest = None;
            None
        }
        // Offline, or the release is gone. Record only that we tried — one request a day, even when they all fail — and keep
        // whatever we already knew.
        Err(_) => state.latest.clone(),
    };
    let _ = state.save(state_path);
    newer
}

/// Pull the version out of GitHub's `releases/latest` body.
pub fn parse_latest(body: &str) -> Result<String, String> {
    let doc: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("the release feed is not JSON: {e}"))?;
    let tag = doc.get("tag_name").and_then(serde_json::Value::as_str).ok_or("the release feed has no tag_name")?;
    let version = version_of(tag);
    if version.is_empty() {
        return Err(format!("the release tag {tag:?} names no version"));
    }
    Ok(version)
}

/// The version a release tag names: a leading `v` and anything after the numbers (`v0.2.0`, `0.2.0-rc1`) do not change it.
pub fn version_of(tag: &str) -> String {
    let tag = tag.trim();
    let tag = tag.strip_prefix('v').unwrap_or(tag);
    tag.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect()
}

/// Is `candidate` later than `current`? Dotted numbers, compared left to right (`0.10.0` beats `0.9.9`).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    numbers(candidate) > numbers(current)
}

/// The dotted numbers at the start of a version: `0.2.0` -> `[0, 2, 0]`. A pre-release suffix ends the number.
fn numbers(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.chars().take_while(char::is_ascii_digit).collect::<String>())
        .take_while(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

/// What to print to stderr once the alternate screen is gone. `gh` does this after a command returns, so the notice never fights
/// the screen or a pipe; the second line is the same advice `/update` gives.
pub fn notice(current: &str, latest: &str, source: InstallSource) -> String {
    format!("{} {latest} is available (you have {current}).\n{}", env!("CARGO_PKG_NAME"), source.advice())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_api_is_built_from_the_repository() {
        assert_eq!(latest_api(), "https://api.github.com/repos/buzzzr/buzzr/releases/latest");
    }

    #[test]
    fn a_release_tag_names_a_version() {
        assert_eq!(version_of("v0.2.0"), "0.2.0");
        assert_eq!(version_of("0.2.0"), "0.2.0");
        assert_eq!(version_of("v0.2.0-rc1"), "0.2.0");
        assert_eq!(version_of("latest"), "");
    }

    #[test]
    fn a_later_version_is_newer_and_an_earlier_one_is_not() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9")); // numbers, not text
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn the_latest_release_body_is_read() {
        let body = r#"{"tag_name": "v0.2.0", "html_url": "https://github.com/buzzzr/buzzr/releases/tag/v0.2.0"}"#;
        assert_eq!(parse_latest(body).unwrap(), "0.2.0");
        assert!(parse_latest(r#"{"name": "no tag"}"#).is_err());
        assert!(parse_latest("<html>404</html>").is_err());
    }

    #[test]
    fn the_state_is_remembered_and_throttles_the_next_check() {
        let path = std::env::temp_dir().join(format!("buzzr-release-{}/state.json", uuid::Uuid::new_v4()));
        assert_eq!(State::load(&path), State::default()); // nothing yet, and no file needed

        let day = INTERVAL.as_secs();
        let never = State::default();
        assert!(never.is_due(1_000_000_000));
        assert_eq!(never.newer_than("0.1.0"), None);

        let state = State { checked_at: 1_000_000_000, latest: Some("0.2.0".into()) };
        state.save(&path).unwrap();
        assert_eq!(State::load(&path), state);
        assert!(!state.is_due(1_000_000_000 + day - 1));
        assert!(state.is_due(1_000_000_000 + day));
        assert_eq!(state.newer_than("0.1.0"), Some("0.2.0"));
        assert_eq!(state.newer_than("0.2.0"), None); // the running copy is already the newest

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn the_notice_names_the_version_and_the_upgrade() {
        let line = notice("0.1.0", "0.2.0", InstallSource::Homebrew);
        assert_eq!(line, "buzzr 0.2.0 is available (you have 0.1.0).\nupgrade with: brew update && brew upgrade buzzr");
        assert!(notice("0.1.0", "0.2.0", InstallSource::Other).ends_with("/releases/latest"));
    }
}
