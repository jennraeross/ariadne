//! Google Drive sync backend for the Google Drive section.
//!
//! The app treats Drive exactly like the iCloud store: one section-scoped
//! `library.json` plus the transparent folder tree of `*.md` files and an
//! `attachments/` folder, all inside a user-visible `Ariadne` folder in the
//! user's My Drive. The JSON store file is the source of truth; the folders
//! are regenerated from it, same contract as the iCloud mirror.
//!
//! All network I/O runs on a single background worker thread with blocking
//! requests, so the UI thread never waits on the network. The UI talks to
//! the worker through [`DriveJob`] / [`DriveEvent`] channels.
//!
//! Auth is OAuth 2.0 for a Desktop (installed-app) client with PKCE and a
//! loopback redirect — no client secret is embedded or needed. The shared
//! client ID is baked in at build time from `ARIADNE_GOOGLE_CLIENT_ID`
//! (a GitHub secret in CI); without it the feature reports itself as
//! unconfigured instead of failing. Tokens live in the OS keychain.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use oauth2::TokenResponse as _;

/// User-visible folder in My Drive holding the store + mirror.
pub const APP_FOLDER_NAME: &str = "Ariadne";
/// Name of the section-scoped store file, same schema as the local one.
pub const STORE_FILE_NAME: &str = "library.json";

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";
const SCOPE_DRIVE_FILE: &str = "https://www.googleapis.com/auth/drive.file";
const DRIVE_API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_API: &str = "https://www.googleapis.com/upload/drive/v3";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
/// Refresh proactively when fewer than this many seconds remain.
const REFRESH_GRACE_SECS: u64 = 120;

/// Shared OAuth client ID, baked in at build time. `None` in local builds
/// made without the env var (and in CI without the secret): the UI then
/// shows Google Drive as unconfigured.
pub fn client_id() -> Option<String> {
    option_env!("ARIADNE_GOOGLE_CLIENT_ID").map(|s| s.to_string())
}

/// OAuth tokens with an absolute expiry, JSON-serializable for storage.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GoogleTokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds when `access_token` expires.
    pub expires_at_unix: u64,
}

impl GoogleTokens {
    pub fn expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now + REFRESH_GRACE_SECS >= self.expires_at_unix
    }
}

/// Token persistence. The app uses the OS keychain; tests use memory.
pub trait TokenStore: Send + 'static {
    fn load(&self) -> Option<GoogleTokens>;
    fn save(&self, tokens: &GoogleTokens);
    fn clear(&self);
}

/// OS-keychain token store (`Ariadne` / `google-drive`).
pub struct KeyringTokenStore;

impl TokenStore for KeyringTokenStore {
    fn load(&self) -> Option<GoogleTokens> {
        let entry = keyring::Entry::new("Ariadne", "google-drive").ok()?;
        let text = entry.get_password().ok()?;
        serde_json::from_str(&text).ok()
    }

    fn save(&self, tokens: &GoogleTokens) {
        if let Ok(entry) = keyring::Entry::new("Ariadne", "google-drive") {
            if let Ok(text) = serde_json::to_string(tokens) {
                let _ = entry.set_password(&text);
            }
        }
    }

    fn clear(&self) {
        if let Ok(entry) = keyring::Entry::new("Ariadne", "google-drive") {
            let _ = entry.delete_credential();
        }
    }
}

/// In-memory token store for tests.
#[derive(Default)]
pub struct MemoryTokenStore {
    inner: Mutex<Option<GoogleTokens>>,
}

impl TokenStore for MemoryTokenStore {
    fn load(&self) -> Option<GoogleTokens> {
        self.inner.lock().ok()?.clone()
    }

    fn save(&self, tokens: &GoogleTokens) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = Some(tokens.clone());
        }
    }

    fn clear(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = None;
        }
    }
}

/// One remote file inside the app folder, paths relative to it (`a/b.md`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveEntry {
    /// Slash-separated path relative to the app folder, e.g. `Inbox/Note.md`.
    pub path: String,
    pub id: String,
    /// Hex MD5 of the content (`md5Checksum`), if Drive reported one.
    pub md5: Option<String>,
}

/// Minimal Drive surface the sync needs. The worker owns the client; the
/// fake below implements the same surface for tests.
pub trait DriveFiles: Send {
    /// Find the app folder in My Drive, creating it (and only it) if needed.
    fn ensure_app_folder(&mut self) -> Result<String, String>;
    /// Recursively list everything under `folder_id` (files only).
    fn list_recursive(&mut self, folder_id: &str) -> Result<Vec<DriveEntry>, String>;
    fn download(&mut self, file_id: &str) -> Result<Vec<u8>, String>;
    /// Create `name` under `parent_id`; returns the new file id.
    fn upload_new(
        &mut self,
        parent_id: &str,
        name: &str,
        mime: &str,
        bytes: &[u8],
    ) -> Result<String, String>;
    fn upload_update(&mut self, file_id: &str, bytes: &[u8], mime: &str) -> Result<(), String>;
    fn delete(&mut self, file_id: &str) -> Result<(), String>;
    /// Create nested folders under `folder_id` for `components`, returning
    /// the deepest folder id.
    fn ensure_path(&mut self, folder_id: &str, components: &[&str]) -> Result<String, String>;
}

/// What to upload/delete to converge the remote folder to `local`.
/// Pure so tests cover it without any network.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PushPlan {
    /// `(path, bytes, existing_id)`: `None` id means create, else update.
    pub upserts: Vec<(String, Vec<u8>, Option<String>)>,
    /// Remote file ids to delete.
    pub deletes: Vec<String>,
}

pub fn plan_push(
    local: &BTreeMap<String, Vec<u8>>,
    remote: &[DriveEntry],
) -> PushPlan {
    let mut plan = PushPlan::default();
    let mut remote_by_path: HashMap<&str, &DriveEntry> = HashMap::new();
    for e in remote {
        remote_by_path.insert(e.path.as_str(), e);
    }
    for (path, bytes) in local {
        match remote_by_path.get(path.as_str()) {
            Some(e) if e.md5.as_deref() == Some(md5_hex(bytes).as_str()) => {}
            Some(e) => plan.upserts.push((path.clone(), bytes.clone(), Some(e.id.clone()))),
            None => plan.upserts.push((path.clone(), bytes.clone(), None)),
        }
    }
    for e in remote {
        if !local.contains_key(&e.path) {
            plan.deletes.push(e.id.clone());
        }
    }
    plan
}

fn md5_hex(bytes: &[u8]) -> String {
    format!("{:x}", md5::compute(bytes))
}

/// Jobs the UI thread sends to the worker. Push payloads are built on the
/// UI thread from already-merged memory (merge-before-write, same rule as
/// the local stores).
pub enum DriveJob {
    /// Report SignedIn/SignedOut from stored tokens (refreshing if needed).
    CheckAuth,
    /// Run the loopback OAuth flow (opens the browser via the event loop).
    Login,
    /// Download the store file; when `then_push`, the UI merges and sends
    /// back a `PushNow` with merged bytes.
    Pull { then_push: bool },
    /// Converge the remote folder to these exact contents.
    PushNow {
        /// Bytes for the top-level store file.
        store_json: Vec<u8>,
        /// Relative path -> bytes for mirror + attachments.
        files: Vec<(String, Vec<u8>)>,
    },
}

/// Results the worker sends back; the UI drains these every frame.
#[derive(Debug)]
pub enum DriveEvent {
    /// Browser needs opening to continue the login flow.
    LoginUrl(String),
    Authed { email: String },
    SignedOut,
    AuthFailed(String),
    Pulled {
        store_json: Option<Vec<u8>>,
        then_push: bool,
    },
    Pushed {
        store_hash: u64,
    },
    SyncError(String),
}

fn content_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Spawn the worker thread. It owns its HTTP client and tokens; everything
/// it needs arrives in [`DriveJob`]s.
pub fn spawn_worker(
    token_store: Box<dyn TokenStore>,
    rx: std::sync::mpsc::Receiver<DriveJob>,
    tx: std::sync::mpsc::Sender<DriveEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("gdrive-sync".to_string())
        .spawn(move || worker_loop(token_store, rx, &tx))
        .expect("spawn gdrive worker")
}

struct Worker {
    client_id: String,
    tokens: Option<GoogleTokens>,
    token_store: Box<dyn TokenStore>,
    http: reqwest::blocking::Client,
}

fn worker_loop(
    token_store: Box<dyn TokenStore>,
    rx: std::sync::mpsc::Receiver<DriveJob>,
    tx: &std::sync::mpsc::Sender<DriveEvent>,
) {
    let Some(client_id) = client_id() else {
        // No client configured: every job fails fast with the same note.
        for job in rx {
            let _ = &job;
            let _ = tx.send(DriveEvent::AuthFailed(
                "Google Drive is not configured in this build".to_string(),
            ));
        }
        return;
    };
    let mut w = Worker {
        client_id,
        tokens: token_store.load(),
        token_store,
        http: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new()),
    };
    for job in rx {
        let event = match job {
            DriveJob::CheckAuth => w.check_auth(),
            DriveJob::Login => w.login(tx),
            DriveJob::Pull { then_push } => w.pull(then_push),
            DriveJob::PushNow { store_json, files } => w.push(store_json, files),
        };
        if tx.send(event).is_err() {
            break;
        }
    }
}

impl Worker {
    fn check_auth(&mut self) -> DriveEvent {
        if self.valid_token().is_err() {
            return DriveEvent::SignedOut;
        }
        // Email is display-only; never fail auth for it (e.g. a project
        // that only granted the Drive scope).
        DriveEvent::Authed {
            email: self.fetch_email().unwrap_or_default(),
        }
    }

    fn login(&mut self, tx: &std::sync::mpsc::Sender<DriveEvent>) -> DriveEvent {
        use oauth2::{AuthUrl, ClientId, CsrfToken, PkceCodeChallenge, RedirectUrl, Scope, TokenUrl};
        use oauth2::basic::BasicClient;

        // Bind loopback first so the redirect URI carries the real port.
        let server = match tiny_http::Server::http("127.0.0.1:0") {
            Ok(s) => s,
            Err(e) => return DriveEvent::AuthFailed(format!("Login listener failed: {e}")),
        };
        let port = match server.server_addr().to_ip().map(|a| a.port()) {
            Some(p) => p,
            None => return DriveEvent::AuthFailed("Login listener failed".to_string()),
        };
        let redirect = format!("http://127.0.0.1:{port}/callback");
        let auth_url = match AuthUrl::new(AUTH_URL.to_string()) {
            Ok(u) => u,
            Err(e) => return DriveEvent::AuthFailed(format!("Bad auth endpoint: {e}")),
        };
        let token_url = match TokenUrl::new(TOKEN_URL.to_string()) {
            Ok(u) => u,
            Err(e) => return DriveEvent::AuthFailed(format!("Bad token endpoint: {e}")),
        };
        let redirect_url = match RedirectUrl::new(redirect) {
            Ok(u) => u,
            Err(e) => return DriveEvent::AuthFailed(format!("Bad redirect URI: {e}")),
        };
        let client = BasicClient::new(ClientId::new(self.client_id.clone()))
            .set_auth_uri(auth_url)
            .set_token_uri(token_url)
            .set_redirect_uri(redirect_url);
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, csrf) = client
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new(SCOPE_DRIVE_FILE.to_string()))
            .add_scope(Scope::new("openid".to_string()))
            .add_scope(Scope::new("email".to_string()))
            .set_pkce_challenge(challenge)
            .add_extra_param("access_type", "offline")
            .add_extra_param("prompt", "consent")
            .url();
        if tx
            .send(DriveEvent::LoginUrl(url.to_string()))
            .is_err()
        {
            return DriveEvent::AuthFailed("UI went away".to_string());
        }
        let (code, _state) = match wait_for_code(server, csrf.secret()) {
            Ok(v) => v,
            Err(e) => return DriveEvent::AuthFailed(e),
        };
        let http = self.http.clone();
        let token = client
            .exchange_code(oauth2::AuthorizationCode::new(code))
            .set_pkce_verifier(verifier)
            .request(&http);
        let token = match token {
            Ok(t) => t,
            Err(e) => return DriveEvent::AuthFailed(friendly_token_error("exchange", &e)),
        };
        let refresh = match token.refresh_token() {
            Some(r) => r.secret().clone(),
            None => {
                return DriveEvent::AuthFailed(
                    "Google did not return a refresh token; try again".to_string(),
                )
            }
        };
        let expires_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            + token.expires_in().map(|d| d.as_secs()).unwrap_or(3600);
        let tokens = GoogleTokens {
            access_token: token.access_token().secret().clone(),
            refresh_token: refresh,
            expires_at_unix,
        };
        self.token_store.save(&tokens);
        self.tokens = Some(tokens);
        // Email is display-only; the tokens are fresh by construction, so
        // never fail the login for a userinfo hiccup.
        let email = self.fetch_email().unwrap_or_default();
        DriveEvent::Authed { email }
    }

    fn pull(&mut self, then_push: bool) -> DriveEvent {
        if let Err(e) = self.valid_token() {
            return e;
        }
        let mut files = DriveHttp::new(self);
        let folder = match files.ensure_app_folder() {
            Ok(id) => id,
            Err(e) => return DriveEvent::SyncError(e),
        };
        let listing = match files.list_recursive(&folder) {
            Ok(l) => l,
            Err(e) => return DriveEvent::SyncError(e),
        };
        // Mirror + attachments download into the staging dir so the editor
        // can resolve other-device images offline (same as iCloud).
        if let Err(e) = stage_remote_files(&mut files, &listing) {
            return DriveEvent::SyncError(e);
        }
        let entry = listing.iter().find(|e| e.path == STORE_FILE_NAME);
        let store_json = match entry {
            Some(e) => match files.download(&e.id) {
                Ok(b) => Some(b),
                Err(e) => return DriveEvent::SyncError(e),
            },
            None => None,
        };
        DriveEvent::Pulled {
            store_json,
            then_push,
        }
    }

    fn push(&mut self, store_json: Vec<u8>, files: Vec<(String, Vec<u8>)>) -> DriveEvent {
        if let Err(e) = self.valid_token() {
            return e;
        }
        let mut local: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        local.insert(STORE_FILE_NAME.to_string(), store_json.clone());
        for (path, bytes) in files {
            local.insert(path, bytes);
        }
        let mut files_api = DriveHttp::new(self);
        let folder = match files_api.ensure_app_folder() {
            Ok(id) => id,
            Err(e) => return DriveEvent::SyncError(e),
        };
        let remote = match files_api.list_recursive(&folder) {
            Ok(l) => l,
            Err(e) => return DriveEvent::SyncError(e),
        };
        let plan = plan_push(&local, &remote);
        // Group upserts by parent dir so each folder is ensured once.
        let mut dir_cache: HashMap<String, String> = HashMap::new();
        dir_cache.insert(String::new(), folder.clone());
        for (path, bytes, existing) in &plan.upserts {
            let (dir, name) = match path.rfind('/') {
                Some(i) => (&path[..i], &path[i + 1..]),
                None => ("", path.as_str()),
            };
            let parent = match ensure_dirs(&mut files_api, &folder, &mut dir_cache, dir) {
                Ok(id) => id,
                Err(e) => return DriveEvent::SyncError(e),
            };
            let mime = mime_for_name(name);
            let res = match existing {
                Some(id) => files_api.upload_update(id, bytes, mime).map(|_| id.clone()),
                None => files_api.upload_new(&parent, name, mime, bytes),
            };
            if let Err(e) = res {
                return DriveEvent::SyncError(e);
            }
        }
        for id in &plan.deletes {
            if let Err(e) = files_api.delete(id) {
                return DriveEvent::SyncError(e);
            }
        }
        DriveEvent::Pushed {
            store_hash: content_hash(&store_json),
        }
    }

    /// Ensure a valid access token, refreshing + persisting when stale.
    /// Returns `SignedOut` (and clears stored tokens) when no usable
    /// credential exists, so the UI can prompt to connect.
    fn valid_token(&mut self) -> Result<(), DriveEvent> {
        let mut tokens = match self.tokens.clone().or_else(|| self.token_store.load()) {
            Some(t) => t,
            None => return Err(DriveEvent::SignedOut),
        };
        if tokens.expired() {
            tokens = match refresh_tokens(&self.http, &self.client_id, &tokens.refresh_token) {
                Ok(t) => t,
                Err(_) => {
                    self.tokens = None;
                    self.token_store.clear();
                    return Err(DriveEvent::SignedOut);
                }
            };
            self.token_store.save(&tokens);
        }
        self.tokens = Some(tokens);
        Ok(())
    }

    fn bearer(&self) -> String {
        self.tokens
            .as_ref()
            .map(|t| t.access_token.clone())
            .unwrap_or_default()
    }

    fn fetch_email(&mut self) -> Result<String, String> {
        if let Err(e) = self.valid_token() {
            return Err(match e {
                DriveEvent::SignedOut => "Not signed in".to_string(),
                other => format!("{other:?}"),
            });
        }
        let resp = self
            .http
            .get(USERINFO_URL)
            .bearer_auth(self.bearer())
            .send()
            .map_err(|e| format!("Profile request failed: {e}"))?;
        let resp = Self::check_status(resp)?;
        let v: serde_json::Value = resp
            .json()
            .map_err(|e| format!("Profile parse failed: {e}"))?;
        Ok(v
            .get("email")
            .and_then(|e| e.as_str())
            .unwrap_or("")
            .to_string())
    }

    fn check_status(
        resp: reqwest::blocking::Response,
    ) -> Result<reqwest::blocking::Response, String> {
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err("unauthorized".to_string());
        }
        if !status.is_success() {
            return Err(format!("Request failed: {status}"));
        }
        Ok(resp)
    }
}

/// Download the remote mirror + attachments into the local staging dir
/// (skip-if-identical), so other-device images resolve offline.
fn stage_remote_files(
    files: &mut impl DriveFiles,
    listing: &[DriveEntry],
) -> Result<(), String> {
    let Some(stage) = staging_dir() else {
        return Ok(());
    };
    for e in listing {
        if e.path == STORE_FILE_NAME {
            continue;
        }
        let dest = stage.join(&e.path);
        let same = std::fs::read(&dest)
            .map(|old| e.md5.as_deref() == Some(md5_hex(&old).as_str()))
            .unwrap_or(false);
        if same {
            continue;
        }
        let bytes = files.download(&e.id)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("Staging dir failed: {err}"))?;
        }
        std::fs::write(&dest, &bytes).map_err(|err| format!("Staging write failed: {err}"))?;
    }
    // Prune staged files that no longer exist remotely.
    let wanted: std::collections::HashSet<&str> =
        listing.iter().map(|e| e.path.as_str()).collect();
    prune_staging(&stage, &stage, &wanted);
    Ok(())
}

fn ensure_dirs(
    files: &mut DriveHttp,
    folder: &str,
    cache: &mut HashMap<String, String>,
    dir: &str,
) -> Result<String, String> {
    if dir.is_empty() {
        return Ok(folder.to_string());
    }
    if let Some(id) = cache.get(dir) {
        return Ok(id.clone());
    }
    let mut current = folder.to_string();
    let mut prefix = String::new();
    for comp in dir.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(comp);
        if let Some(id) = cache.get(&prefix) {
            current = id.clone();
            continue;
        }
        current = files.ensure_path(&current, &[comp])?;
        cache.insert(prefix.clone(), current.clone());
    }
    Ok(current)
}

fn prune_staging(stage: &std::path::Path, dir: &std::path::Path, wanted: &std::collections::HashSet<&str>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            prune_staging(stage, &path, wanted);
            let _ = std::fs::remove_dir(&path);
        } else if path.is_file() {
            let rel = path
                .strip_prefix(stage)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if rel == STORE_FILE_NAME || !wanted.contains(rel.as_str()) {
                // Never delete the staged store upload payload here; the
                // remote listing drives deletions on push instead.
                if rel != STORE_FILE_NAME {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

/// Local staging dir for the Drive mirror (`<data-dir>/gdrive/`). Holds the
/// generated `*.md`/attachments uploaded on push and the copies downloaded
/// on pull for offline image resolution.
pub fn staging_dir() -> Option<std::path::PathBuf> {    if let Ok(p) = std::env::var("ARIADNE_GDRIVE_DIR") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    directories::ProjectDirs::from("com", "craft-anything", "ariadne")
        .map(|dirs| dirs.data_dir().join("gdrive"))
}

/// Open a URL in the default browser (used for the OAuth sign-in page).
pub fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", url])
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Explain the most common token-endpoint misconfiguration in plain
/// language: a Web-application client ID (which mandates a secret) was
/// used where a Desktop-app client ID (PKCE, no secret) is required.
fn friendly_token_error(action: &str, e: &impl std::fmt::Display) -> String {
    let msg = e.to_string();
    if msg.contains("client_secret") {
        return "Google rejected the sign-in: this looks like a Web-application \
            OAuth client ID, which requires a client secret. Create a \
            Desktop-app client ID instead (APIs & Services → Credentials) \
            and update GOOGLE_CLIENT_ID."
            .to_string();
    }
    format!("Token {action} failed: {msg}")
}

/// Wait for the OAuth redirect (validating `state`), with a 5-minute cap.
fn wait_for_code(
    server: tiny_http::Server,
    expected_state: &str,
) -> Result<(String, String), String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return Err("Sign-in timed out; try again".to_string());
        }
        match server.recv_timeout(deadline - now) {
            Ok(Some(request)) => {
                let full = format!("http://localhost{}", request.url());
                let parsed =
                    url::Url::parse(&full).map_err(|e| format!("Bad redirect: {e}"))?;
                let mut code = None;
                let mut state = None;
                for (k, v) in parsed.query_pairs() {
                    if k == "code" {
                        code = Some(v.into_owned());
                    } else if k == "state" {
                        state = Some(v.into_owned());
                    }
                }
                let _ = request.respond(
                    tiny_http::Response::from_string(
                        "Signed in to Ariadne — you can close this tab.",
                    )
                    .with_status_code(200),
                );
                match (code, state) {
                    (Some(c), Some(s)) if s == expected_state => return Ok((c, s)),
                    _ => return Err("Sign-in was denied or failed".to_string()),
                }
            }
            Ok(None) => return Err("Sign-in timed out; try again".to_string()),
            Err(e) => return Err(format!("Login listener failed: {e}")),
        }
    }
}

fn refresh_tokens(
    http: &reqwest::blocking::Client,
    client_id: &str,
    refresh_token: &str,
) -> Result<GoogleTokens, String> {
    use oauth2::basic::BasicClient;
    use oauth2::{AuthUrl, ClientId, RefreshToken, TokenUrl};
    let auth_url = AuthUrl::new(AUTH_URL.to_string())
        .map_err(|e| format!("Bad auth endpoint: {e}"))?;
    let token_url = TokenUrl::new(TOKEN_URL.to_string())
        .map_err(|e| format!("Bad token endpoint: {e}"))?;
    let client = BasicClient::new(ClientId::new(client_id.to_string()))
        .set_auth_uri(auth_url)
        .set_token_uri(token_url);
    let token = client
        .exchange_refresh_token(&RefreshToken::new(refresh_token.to_string()))
        .request(http)
        .map_err(|e| friendly_token_error("refresh", &e))?;
    // Google may or may not rotate the refresh token; keep the old one when
    // it doesn't return a new one.
    let refresh = token
        .refresh_token()
        .map(|r| r.secret().clone())
        .unwrap_or_else(|| refresh_token.to_string());
    let expires_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        + token.expires_in().map(|d| d.as_secs()).unwrap_or(3600);
    Ok(GoogleTokens {
        access_token: token.access_token().secret().clone(),
        refresh_token: refresh,
        expires_at_unix,
    })
}

fn mime_for_name(name: &str) -> &'static str {
    if name.ends_with(".md") || name.ends_with(".markdown") {
        "text/markdown"
    } else if name.ends_with(".json") {
        "application/json"
    } else {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".png") {
            "image/png"
        } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
            "image/jpeg"
        } else if lower.ends_with(".gif") {
            "image/gif"
        } else {
            "application/octet-stream"
        }
    }
}

/// Thin HTTP surface over [`Worker`] credentials, implementing
/// [`DriveFiles`] with one 401-refresh retry.
struct DriveHttp<'a> {
    worker: &'a mut Worker,
}

impl<'a> DriveHttp<'a> {
    fn new(worker: &'a mut Worker) -> Self {
        Self { worker }
    }

    fn get(&mut self, url: &str) -> Result<reqwest::blocking::Response, String> {
        let resp = self
            .worker
            .http
            .get(url)
            .bearer_auth(self.worker.bearer())
            .send()
            .map_err(|e| format!("Request failed: {e}"))?;
        self.retry_on_unauthorized(url, resp, |http, token, url| {
            http.get(url).bearer_auth(token).send()
        })
    }

    fn retry_on_unauthorized(
        &mut self,
        url: &str,
        resp: reqwest::blocking::Response,
        retry: impl FnOnce(
            &reqwest::blocking::Client,
            String,
            &str,
        ) -> Result<reqwest::blocking::Response, reqwest::Error>,
    ) -> Result<reqwest::blocking::Response, String> {
        if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Worker::check_status(resp);
        }
        // One refresh + retry, then give up.
        let refresh = self
            .worker
            .tokens
            .as_ref()
            .map(|t| t.refresh_token.clone());
        let Some(refresh) = refresh else {
            return Err("unauthorized".to_string());
        };
        let tokens = refresh_tokens(&self.worker.http, &self.worker.client_id, &refresh)
            .map_err(|e| format!("Re-auth needed: {e}"))?;
        self.worker.token_store.save(&tokens);
        self.worker.tokens = Some(tokens.clone());
        let resp = retry(&self.worker.http, tokens.access_token, url)
            .map_err(|e| format!("Request failed: {e}"))?;
        Worker::check_status(resp)
    }

    fn drive_url(path: &str) -> String {
        format!("{DRIVE_API}{path}")
    }

    fn escape_query(s: &str) -> String {
        s.replace('\\', "\\\\").replace('\'', "\\'")
    }
}

impl DriveFiles for DriveHttp<'_> {
    fn ensure_app_folder(&mut self) -> Result<String, String> {
        let q = format!(
            "name = '{}' and mimeType = '{FOLDER_MIME}' and trashed = false",
            Self::escape_query(APP_FOLDER_NAME)
        );
        let url = format!(
            "{}?q={}&fields=files(id,name)&spaces=drive",
            Self::drive_url("/files"),
            url::form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>()
        );
        let resp = self.get(&url)?;
        let v: serde_json::Value = resp
            .json()
            .map_err(|e| format!("Folder lookup failed: {e}"))?;
        if let Some(id) = v
            .get("files")
            .and_then(|f| f.as_array())
            .and_then(|a| a.first())
            .and_then(|f| f.get("id"))
            .and_then(|i| i.as_str())
        {
            return Ok(id.to_string());
        }
        // Create it in My Drive root.
        let body = serde_json::json!({
            "name": APP_FOLDER_NAME,
            "mimeType": FOLDER_MIME,
        });
        let resp = self
            .worker
            .http
            .post(Self::drive_url("/files") + "?fields=id")
            .bearer_auth(self.worker.bearer())
            .json(&body)
            .send()
            .map_err(|e| format!("Folder create failed: {e}"))?;
        let resp = Worker::check_status(resp)?;
        let v: serde_json::Value = resp
            .json()
            .map_err(|e| format!("Folder create parse failed: {e}"))?;
        v.get("id")
            .and_then(|i| i.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "Folder create returned no id".to_string())
    }

    fn list_recursive(&mut self, folder_id: &str) -> Result<Vec<DriveEntry>, String> {
        let mut out = Vec::new();
        self.list_into(folder_id, String::new(), &mut out)?;
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn download(&mut self, file_id: &str) -> Result<Vec<u8>, String> {
        let url = format!("{}?alt=media", Self::drive_url(&format!("/files/{file_id}")));
        let mut resp = self.get(&url)?;
        let mut bytes = Vec::new();
        resp
            .copy_to(&mut bytes)
            .map_err(|e| format!("Download failed: {e}"))?;
        Ok(bytes)
    }

    fn upload_new(
        &mut self,
        parent_id: &str,
        name: &str,
        mime: &str,
        bytes: &[u8],
    ) -> Result<String, String> {
        let meta = serde_json::json!({ "name": name, "parents": [parent_id] });
        self.upload_multipart(None, &meta, mime, bytes)
    }

    fn upload_update(&mut self, file_id: &str, bytes: &[u8], mime: &str) -> Result<(), String> {
        let meta = serde_json::json!({});
        self.upload_multipart(Some(file_id), &meta, mime, bytes)?;
        Ok(())
    }

    fn delete(&mut self, file_id: &str) -> Result<(), String> {
        let url = Self::drive_url(&format!("/files/{file_id}"));
        let resp = self
            .worker
            .http
            .delete(&url)
            .bearer_auth(self.worker.bearer())
            .send()
            .map_err(|e| format!("Delete failed: {e}"))?;
        Worker::check_status(resp)?;
        Ok(())
    }

    fn ensure_path(&mut self, folder_id: &str, components: &[&str]) -> Result<String, String> {
        let mut current = folder_id.to_string();
        for comp in components {
            let q = format!(
                "name = '{}' and '{}' in parents and mimeType = '{FOLDER_MIME}' and trashed = false",
                Self::escape_query(comp),
                current
            );
            let url = format!(
                "{}?q={}&fields=files(id)&spaces=drive",
                Self::drive_url("/files"),
                url::form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>()
            );
            let resp = self.get(&url)?;
            let v: serde_json::Value = resp
                .json()
                .map_err(|e| format!("Folder lookup failed: {e}"))?;
            let found = v
                .get("files")
                .and_then(|f| f.as_array())
                .and_then(|a| a.first())
                .and_then(|f| f.get("id"))
                .and_then(|i| i.as_str())
                .map(|s| s.to_string());
            current = match found {
                Some(id) => id,
                None => {
                    let body = serde_json::json!({
                        "name": comp,
                        "mimeType": FOLDER_MIME,
                        "parents": [current],
                    });
                    let resp = self
                        .worker
                        .http
                        .post(Self::drive_url("/files") + "?fields=id")
                        .bearer_auth(self.worker.bearer())
                        .json(&body)
                        .send()
                        .map_err(|e| format!("Folder create failed: {e}"))?;
                    let resp = Worker::check_status(resp)?;
                    let v: serde_json::Value = resp
                        .json()
                        .map_err(|e| format!("Folder create parse failed: {e}"))?;
                    v.get("id")
                        .and_then(|i| i.as_str())
                        .map(|s| s.to_string())
                        .ok_or_else(|| "Folder create returned no id".to_string())?
                }
            };
        }
        Ok(current)
    }
}

impl DriveHttp<'_> {
    fn list_into(
        &mut self,
        folder_id: &str,
        prefix: String,
        out: &mut Vec<DriveEntry>,
    ) -> Result<(), String> {
        let mut page_token: Option<String> = None;
        loop {
            let q = format!(
                "'{}' in parents and trashed = false",
                folder_id
            );
            let mut url = format!(
                "{}?q={}&fields=files(id,name,mimeType,md5Checksum,modifiedTime),nextPageToken&pageSize=200&spaces=drive",
                Self::drive_url("/files"),
                url::form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>()
            );
            if let Some(t) = &page_token {
                url.push_str("&pageToken=");
                url.push_str(&url::form_urlencoded::byte_serialize(t.as_bytes()).collect::<String>());
            }
            let resp = self.get(&url)?;
            let v: serde_json::Value = resp
                .json()
                .map_err(|e| format!("List failed: {e}"))?;
            let files = v
                .get("files")
                .and_then(|f| f.as_array())
                .cloned()
                .unwrap_or_default();
            for f in files {
                let (Some(id), Some(name)) = (
                    f.get("id").and_then(|i| i.as_str()),
                    f.get("name").and_then(|n| n.as_str()),
                ) else {
                    continue;
                };
                let is_folder =
                    f.get("mimeType").and_then(|m| m.as_str()) == Some(FOLDER_MIME);
                let path = if prefix.is_empty() {
                    name.to_string()
                } else {
                    format!("{prefix}/{name}")
                };
                if is_folder {
                    self.list_into(id, path, out)?;
                } else {
                    out.push(DriveEntry {
                        path,
                        id: id.to_string(),
                        md5: f
                            .get("md5Checksum")
                            .and_then(|m| m.as_str())
                            .map(|s| s.to_string()),
                    });
                }
            }
            page_token = v
                .get("nextPageToken")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string());
            if page_token.is_none() {
                break;
            }
        }
        Ok(())
    }

    fn upload_multipart(
        &mut self,
        file_id: Option<&str>,
        meta: &serde_json::Value,
        mime: &str,
        bytes: &[u8],
    ) -> Result<String, String> {
        // Minimal multipart/related body; avoids a multipart dep.
        let boundary: String = format!(
            "ariadne{:x}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let meta_text = serde_json::to_string(meta).unwrap_or_else(|_| "{}".to_string());
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(b"Content-Type: application/json; charset=UTF-8\r\n\r\n");
        body.extend_from_slice(meta_text.as_bytes());
        body.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
        body.extend_from_slice(format!("Content-Type: {mime}\r\n\r\n").as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let url = match file_id {
            Some(id) => format!("{UPLOAD_API}/files/{id}?uploadType=multipart&fields=id"),
            None => format!("{UPLOAD_API}/files?uploadType=multipart&fields=id"),
        };
        let send = |http: &reqwest::blocking::Client, token: String, url: &str| {
            let req = http
                .request(
                    if file_id.is_some() {
                        reqwest::Method::PATCH
                    } else {
                        reqwest::Method::POST
                    },
                    url,
                )
                .bearer_auth(token)
                .header("Content-Type", format!("multipart/related; boundary={boundary}"))
                .body(body.clone());
            req.send()
        };
        let resp = send(&self.worker.http, self.worker.bearer(), &url)
            .map_err(|e| format!("Upload failed: {e}"))?;
        let resp = if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            let refresh = self
                .worker
                .tokens
                .as_ref()
                .map(|t| t.refresh_token.clone());
            let Some(refresh) = refresh else {
                return Err("unauthorized".to_string());
            };
            let tokens =
                refresh_tokens(&self.worker.http, &self.worker.client_id, &refresh)
                    .map_err(|e| format!("Re-auth needed: {e}"))?;
            self.worker.token_store.save(&tokens);
            let token = tokens.access_token.clone();
            self.worker.tokens = Some(tokens);
            let resp = send(&self.worker.http, token, &url)
                .map_err(|e| format!("Upload failed: {e}"))?;
            Worker::check_status(resp)?
        } else {
            Worker::check_status(resp)?
        };
        let v: serde_json::Value = resp
            .json()
            .map_err(|e| format!("Upload parse failed: {e}"))?;
        v.get("id")
            .and_then(|i| i.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "Upload returned no id".to_string())
    }
}

/// In-memory [`DriveFiles`] for tests: paths map straight to bytes.
#[derive(Default)]
pub struct MemoryDriveFiles {
    files: BTreeMap<String, Vec<u8>>,
    next_id: u64,
    ids: HashMap<String, String>,
}

impl DriveFiles for MemoryDriveFiles {
    fn ensure_app_folder(&mut self) -> Result<String, String> {
        Ok("app-folder".to_string())
    }

    fn list_recursive(&mut self, _folder_id: &str) -> Result<Vec<DriveEntry>, String> {
        let mut out: Vec<DriveEntry> = self
            .files
            .iter()
            .map(|(path, bytes)| DriveEntry {
                path: path.clone(),
                id: self
                    .ids
                    .get(path)
                    .cloned()
                    .unwrap_or_else(|| format!("id-{path}")),
                md5: Some(md5_hex(bytes)),
            })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn download(&mut self, file_id: &str) -> Result<Vec<u8>, String> {
        self.files
            .iter()
            .find(|(p, _)| {
                self.ids.get(*p).map(|i| i.as_str()) == Some(file_id)
                    || format!("id-{p}") == file_id
            })
            .map(|(_, b)| b.clone())
            .ok_or_else(|| "not found".to_string())
    }

    fn upload_new(
        &mut self,
        _parent_id: &str,
        name: &str,
        _mime: &str,
        bytes: &[u8],
    ) -> Result<String, String> {
        // Tests upload flat names; nested paths arrive via ensure_path
        // flows in plan application, which the sync tests bypass.
        let id = format!("id-{name}-{}", self.next_id);
        self.next_id += 1;
        self.files.insert(name.to_string(), bytes.to_vec());
        self.ids.insert(name.to_string(), id.clone());
        Ok(id)
    }

    fn upload_update(&mut self, file_id: &str, bytes: &[u8], _mime: &str) -> Result<(), String> {
        let path = self
            .ids
            .iter()
            .find(|(_, id)| id.as_str() == file_id)
            .map(|(p, _)| p.clone())
            .or_else(|| {
                self.files
                    .keys()
                    .find(|p| format!("id-{p}") == file_id)
                    .cloned()
            })
            .ok_or_else(|| "not found".to_string())?;
        self.files.insert(path, bytes.to_vec());
        Ok(())
    }

    fn delete(&mut self, file_id: &str) -> Result<(), String> {
        let path = self
            .ids
            .iter()
            .find(|(_, id)| id.as_str() == file_id)
            .map(|(p, _)| p.clone())
            .or_else(|| {
                self.files
                    .keys()
                    .find(|p| format!("id-{p}") == file_id)
                    .cloned()
            })
            .ok_or_else(|| "not found".to_string())?;
        self.files.remove(&path);
        self.ids.remove(&path);
        Ok(())
    }

    fn ensure_path(&mut self, folder_id: &str, _components: &[&str]) -> Result<String, String> {
        Ok(folder_id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_error_names_wrong_client_type() {
        let msg = friendly_token_error(
            "exchange",
            &"invalid_request: client_secret is missing".to_string(),
        );
        assert!(msg.contains("Desktop-app"), "got: {msg}");
        let plain = friendly_token_error("refresh", &"timeout".to_string());
        assert!(plain.contains("Token refresh failed"));
    }

    #[test]
    fn tokens_serde_roundtrip() {
        let tokens = GoogleTokens {
            access_token: "a".to_string(),
            refresh_token: "r".to_string(),
            expires_at_unix: 123,
        };
        let text = serde_json::to_string(&tokens).unwrap();
        let back: GoogleTokens = serde_json::from_str(&text).unwrap();
        assert_eq!(back.refresh_token, "r");
        assert_eq!(back.expires_at_unix, 123);
    }

    #[test]
    fn memory_token_store_roundtrip() {
        let store = MemoryTokenStore::default();
        assert!(store.load().is_none());
        let tokens = GoogleTokens {
            access_token: "a".to_string(),
            refresh_token: "r".to_string(),
            expires_at_unix: u64::MAX,
        };
        assert!(!tokens.expired());
        store.save(&tokens);
        assert_eq!(store.load().unwrap().access_token, "a");
        store.clear();
        assert!(store.load().is_none());
    }

    #[test]
    fn expired_tokens_detected() {
        let tokens = GoogleTokens {
            access_token: "a".to_string(),
            refresh_token: "r".to_string(),
            expires_at_unix: 1,
        };
        assert!(tokens.expired());
    }

    #[test]
    fn plan_push_creates_updates_deletes() {
        let mut local = BTreeMap::new();
        local.insert("library.json".to_string(), b"{}".to_vec());
        local.insert("Inbox/New.md".to_string(), b"hello".to_vec());
        local.insert("Inbox/Kept.md".to_string(), b"same".to_vec());
        let remote = vec![
            DriveEntry {
                path: "Inbox/Kept.md".to_string(),
                id: "k".to_string(),
                md5: Some(md5_hex(b"same")),
            },
            DriveEntry {
                path: "Inbox/Stale.md".to_string(),
                id: "s".to_string(),
                md5: Some(md5_hex(b"old")),
            },
        ];
        let plan = plan_push(&local, &remote);
        // library.json + New.md created, Kept.md untouched, Stale.md gone.
        assert_eq!(plan.upserts.len(), 2);
        assert!(plan
            .upserts
            .iter()
            .all(|(_, _, id)| id.is_none()));
        assert_eq!(plan.deletes, vec!["s".to_string()]);
    }

    #[test]
    fn plan_push_updates_changed_content() {
        let mut local = BTreeMap::new();
        local.insert("Note.md".to_string(), b"v2".to_vec());
        let remote = vec![DriveEntry {
            path: "Note.md".to_string(),
            id: "n".to_string(),
            md5: Some(md5_hex(b"v1")),
        }];
        let plan = plan_push(&local, &remote);
        assert_eq!(plan.upserts.len(), 1);
        assert_eq!(plan.upserts[0].2.as_deref(), Some("n"));
        assert!(plan.deletes.is_empty());
    }

    #[test]
    fn memory_drive_files_crud() {
        let mut drive = MemoryDriveFiles::default();
        let folder = drive.ensure_app_folder().unwrap();
        let id = drive
            .upload_new(&folder, "Note.md", "text/markdown", b"hi")
            .unwrap();
        let listing = drive.list_recursive(&folder).unwrap();
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].path, "Note.md");
        assert_eq!(drive.download(&id).unwrap(), b"hi");
        drive.upload_update(&id, b"hi2", "text/markdown").unwrap();
        assert_eq!(drive.download(&id).unwrap(), b"hi2");
        drive.delete(&id).unwrap();
        assert!(drive.list_recursive(&folder).unwrap().is_empty());
    }
}
