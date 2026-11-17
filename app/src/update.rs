//! In-app update checks (check + guided download).
//!
//! The app queries the GitHub Releases API for the latest published tag,
//! compares it to the built-in [`current_version`], and — when newer —
//! points the user at the right installer asset for their OS. The app
//! never replaces itself: the user downloads the DMG / installer / `.deb`
//! and installs it the same way they installed the app.
//!
//! All network I/O runs on a short-lived background thread (see
//! `WritingApp::update_tick`), so the UI thread never waits on the
//! network. Failures are reported as status text, never dialogs.

use std::path::{Path, PathBuf};
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};
// Browser build: std clocks panic on wasm; web_time mirrors the std API.
#[cfg(target_arch = "wasm32")]
use web_time::{SystemTime, UNIX_EPOCH};

/// Default API endpoint for the latest release. Tests can point elsewhere
/// with `ARIADNE_UPDATE_URL`.
pub const LATEST_API_URL: &str = "https://api.github.com/repos/jennraeross/ariadne/releases/latest";
/// How long a check may take before it gives up.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
/// Minimum age of the last check before an automatic re-check.
pub const AUTO_CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// Version this binary was built as.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// What a newer release looks like to the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseInfo {
    /// Raw tag, e.g. `v0.6.4`.
    pub tag: String,
    /// Human-readable release name (may be empty).
    pub name: String,
    /// Release notes (may be empty / long; the UI excerpts it).
    pub notes: String,
    /// Browser URL of the release page (fallback destination).
    pub html_url: String,
    /// Direct download URL for this OS's installer, or the release page
    /// when no per-OS asset matched.
    pub download_url: String,
}

/// Parse `v1.2.3`-style versions into `(major, minor, patch)`. Tolerates a
/// leading `v`, surrounding whitespace, missing components (treated as 0),
/// and pre-release suffixes (`1.2.3-beta` parses as `1.2.3`).
pub fn parse_semver(s: &str) -> (u64, u64, u64) {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let mut out = [0u64; 3];
    for (i, part) in s.split('.').take(3).enumerate() {
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        out[i] = digits.parse::<u64>().unwrap_or(0);
    }
    (out[0], out[1], out[2])
}

/// True when `latest` is strictly newer than `current`.
pub fn is_newer(current: &str, latest: &str) -> bool {
    parse_semver(latest) > parse_semver(current)
}

/// Pick this OS's installer URL from `(file name, download url)` pairs.
/// Falls back to `None` so the caller can link the release page instead.
pub fn pick_asset(assets: &[(String, String)]) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        assets
            .iter()
            .find(|(name, _)| name.ends_with(".dmg") && name.contains("macos"))
            .or_else(|| assets.iter().find(|(name, _)| name.ends_with(".dmg")))
            .map(|(_, url)| url.clone())
    }
    #[cfg(target_os = "windows")]
    {
        assets
            .iter()
            .find(|(name, _)| name.ends_with(".exe"))
            .map(|(_, url)| url.clone())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        assets
            .iter()
            .find(|(name, _)| name.ends_with(".deb"))
            .or_else(|| assets.iter().find(|(name, _)| name.ends_with(".tar.gz")))
            .map(|(_, url)| url.clone())
    }
}

/// Build a [`ReleaseInfo`] from a decoded GitHub release JSON object.
pub fn parse_release(v: &serde_json::Value) -> Result<ReleaseInfo, String> {
    let tag = v
        .get("tag_name")
        .and_then(|s| s.as_str())
        .ok_or_else(|| "Release is missing tag_name".to_string())?
        .to_string();
    let name = v
        .get("name")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string();
    let notes = v
        .get("body")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string();
    let html_url = v
        .get("html_url")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string();
    let assets: Vec<(String, String)> = v
        .get("assets")
        .and_then(|a| a.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    Some((
                        a.get("name")?.as_str()?.to_string(),
                        a.get("browser_download_url")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let download_url = pick_asset(&assets).unwrap_or_else(|| html_url.clone());
    Ok(ReleaseInfo {
        tag,
        name,
        notes,
        html_url,
        download_url,
    })
}

/// API endpoint, honoring the `ARIADNE_UPDATE_URL` test override.
fn api_url() -> String {
    std::env::var("ARIADNE_UPDATE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| LATEST_API_URL.to_string())
}

/// Query the Releases API on the calling thread. Times out after
/// [`CHECK_TIMEOUT`]; errors are plain-language strings for the status line.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_latest() -> Result<ReleaseInfo, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(CHECK_TIMEOUT)
        .user_agent(format!("Ariadne/{}", current_version()))
        .build()
        .map_err(|e| format!("Could not start the update check ({e})"))?;
    let text = client
        .get(api_url())
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|e| {
            if e.is_timeout() {
                "Update check timed out".to_string()
            } else {
                format!("Could not check for updates ({e})")
            }
        })?
        .error_for_status()
        .map_err(|e| format!("Update check failed ({e})"))?
        .text()
        .map_err(|e| format!("Could not read the update reply ({e})"))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Could not read the update reply ({e})"))?;
    parse_release(&v)
}

/// Browser build: the web app is served, not installed, so there is
/// nothing to check — the Updates tab explains where new versions come from.
#[cfg(target_arch = "wasm32")]
pub fn fetch_latest() -> Result<ReleaseInfo, String> {
    Err("Update checks are not available in the browser build".to_string())
}

/// Per-machine update preferences. Lives in `update.json` next to the
/// library — deliberately outside the synced stores, since check times and
/// the auto-check toggle are per-device choices.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct UpdatePrefs {
    pub auto_check: bool,
    pub last_check_unix: u64,
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self {
            auto_check: true,
            last_check_unix: 0,
        }
    }
}

/// Now as Unix seconds.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn prefs_path(dir: &Path) -> PathBuf {
    dir.join("update.json")
}

pub fn load_prefs(dir: Option<PathBuf>) -> UpdatePrefs {
    let Some(dir) = dir else { return UpdatePrefs::default() };
    let Ok(text) = std::fs::read_to_string(prefs_path(&dir)) else {
        return UpdatePrefs::default();
    };
    serde_json::from_str::<UpdatePrefs>(&text).unwrap_or_default()
}

pub fn save_prefs(dir: Option<PathBuf>, prefs: &UpdatePrefs) {
    let Some(dir) = dir else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(prefs) {
        let _ = std::fs::write(prefs_path(&dir), text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(parse_semver("v0.6.4"), (0, 6, 4));
        assert_eq!(parse_semver("1.2"), (1, 2, 0));
        assert_eq!(parse_semver("  V2.10.1-beta "), (2, 10, 1));
        assert_eq!(parse_semver("garbage"), (0, 0, 0));
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.6.3", "v0.6.4"));
        assert!(is_newer("0.6.3", "0.7.0"));
        assert!(!is_newer("0.6.4", "0.6.4"));
        assert!(!is_newer("0.7.0", "0.6.9"));
        assert!(!is_newer("0.6.3", "garbage"));
    }

    #[test]
    fn parses_release_payload() {
        let v: serde_json::Value = serde_json::json!({
            "tag_name": "v0.7.0",
            "name": "Ariadne 0.7.0",
            "body": "Bug fixes.",
            "html_url": "https://github.com/jennraeross/ariadne/releases/tag/v0.7.0",
            "assets": [
                {"name": "ariadne-0.7.0-macos-universal.dmg", "browser_download_url": "https://example.com/a.dmg"},
                {"name": "Ariadne-Setup-0.7.0-x64.exe", "browser_download_url": "https://example.com/a.exe"},
                {"name": "ariadne_0.7.0-1_amd64.deb", "browser_download_url": "https://example.com/a.deb"},
                {"name": "ariadne-0.7.0-linux-x86_64.tar.gz", "browser_download_url": "https://example.com/a.tar.gz"},
            ]
        });
        let info = parse_release(&v).expect("parses");
        assert_eq!(info.tag, "v0.7.0");
        assert_eq!(info.name, "Ariadne 0.7.0");
        assert_eq!(info.notes, "Bug fixes.");
        assert!(!info.download_url.is_empty());
        #[cfg(target_os = "macos")]
        assert!(info.download_url.ends_with(".dmg"));
        #[cfg(target_os = "windows")]
        assert!(info.download_url.ends_with(".exe"));
        #[cfg(target_os = "linux")]
        assert!(info.download_url.ends_with(".deb"));
    }

    #[test]
    fn release_without_matching_asset_falls_back_to_page() {
        let v: serde_json::Value = serde_json::json!({
            "tag_name": "v0.7.0",
            "html_url": "https://github.com/jennraeross/ariadne/releases/tag/v0.7.0",
            "assets": []
        });
        let info = parse_release(&v).expect("parses");
        assert_eq!(
            info.download_url,
            "https://github.com/jennraeross/ariadne/releases/tag/v0.7.0"
        );
    }

    #[test]
    fn release_missing_tag_errors() {
        let v: serde_json::Value = serde_json::json!({"name": "nope"});
        assert!(parse_release(&v).is_err());
    }

    #[test]
    fn prefs_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ariadne-update-test-{}", now_unix()));
        let _ = std::fs::remove_dir_all(&dir);
        let prefs = UpdatePrefs {
            auto_check: false,
            last_check_unix: 123,
        };
        save_prefs(Some(dir.clone()), &prefs);
        let back = load_prefs(Some(dir.clone()));
        assert!(!back.auto_check);
        assert_eq!(back.last_check_unix, 123);
        let _ = std::fs::remove_dir_all(&dir);
        // Missing dir falls back to defaults without failing.
        assert!(load_prefs(None).auto_check);
    }
}
