//! Browser bindings for the web/PWA build (`target_arch = "wasm32"`).
//!
//! The native app persists the library, attachments, styles, themes, and
//! history as files next to the save file. Browsers have no such folder,
//! so this module provides the equivalents:
//!
//! - `localStorage` text slots for the library JSON, style map, theme file,
//!   and history JSON (written through on save; images live in their own
//!   slot since they can be large).
//! - In-memory image bytes, hydrated from `localStorage` once at startup
//!   (base64) and consulted by image resolution instead of the assets
//!   folder.
//! - File downloads (export/save flows) via Blob + anchor click, and
//!   new-tab opens for preview flows.

use std::cell::RefCell;
use std::collections::HashMap;

use wasm_bindgen::JsCast;

pub const LIBRARY_KEY: &str = "ariadne-library-v1";
pub const IMAGES_KEY: &str = "ariadne-images-v1";
pub const STYLES_KEY: &str = "ariadne-styles-v1";
pub const THEMES_KEY: &str = "ariadne-themes-v1";
pub const HISTORY_KEY: &str = "ariadne-history-v1";
pub const UI_SCALE_KEY: &str = "ariadne-ui-scale-v1";
pub const SCROLL_SPEED_KEY: &str = "ariadne-scroll-speed-v1";

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

/// Read a `localStorage` slot. Missing keys and failures yield `None`,
/// exactly like a missing file on native.
pub fn load_text(key: &str) -> Option<String> {
    storage()?.get_item(key).ok()?
}

/// Write a `localStorage` slot. Failures (notably quota) return false so
/// callers can report instead of silently dropping data.
pub fn store_text(key: &str, value: &str) -> bool {
    match storage() {
        Some(s) => s.set_item(key, value).is_ok(),
        None => false,
    }
}

/// Download `bytes` as `filename` through a Blob URL + anchor click.
/// Must run on the UI thread during event handling (it is: every caller
/// is a button handler inside `update`).
pub fn download(filename: &str, bytes: &[u8], mime: &str) -> bool {
    let array = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes).into());
    let mut bag = web_sys::BlobPropertyBag::new();
    bag.set_type(mime);
    let blob = match web_sys::Blob::new_with_u8_array_sequence_and_options(&array, &bag) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let url = match web_sys::Url::create_object_url_with_blob(&blob) {
        Ok(u) => u,
        Err(_) => return false,
    };
    let ok = (|| {
        let document = web_sys::window()?.document()?;
        let anchor: web_sys::HtmlAnchorElement =
            document.create_element("a").ok()?.dyn_into().ok()?;
        anchor.set_href(&url);
        anchor.set_download(filename);
        document.body()?.append_child(&anchor).ok()?;
        anchor.click();
        anchor.remove();
        Some(())
    })()
    .is_some();
    let _ = web_sys::Url::revoke_object_url(&url);
    ok
}

/// Guess a download MIME from an export file extension.
pub fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "html" | "htm" => "text/html",
        "md" | "markdown" => "text/markdown",
        "rtf" => "application/rtf",
        "pdf" => "application/pdf",
        "docx" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        }
        "epub" => "application/epub+zip",
        "pptx" => {
            "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        }
        "json" => "application/json",
        "css" => "text/css",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

/// Open a URL in a new tab (`window.open`). Used for the update-download
/// flow, which has no file bytes to hand to [`download`].
pub fn open_new_tab(url: &str) -> bool {
    web_sys::window()
        .and_then(|w| w.open_with_url_and_target(url, "_blank").ok().flatten())
        .is_some()
}

/// One-shot quota notice: returns true the first time storage fails,
/// so a full `localStorage` surfaces once instead of on every keystroke.
pub fn note_quota_once() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    !WARNED.swap(true, Ordering::Relaxed)
}

/// Render `bytes` in a new tab via a Blob URL (preview "Open" flow).
/// Falls back to a download when popups are blocked.
pub fn open_blob_new_tab(filename: &str, bytes: &[u8], mime: &str) -> bool {
    let array = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes).into());
    let mut bag = web_sys::BlobPropertyBag::new();
    bag.set_type(mime);
    let blob = match web_sys::Blob::new_with_u8_array_sequence_and_options(&array, &bag) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let url = match web_sys::Url::create_object_url_with_blob(&blob) {
        Ok(u) => u,
        Err(_) => return false,
    };
    let opened = web_sys::window()
        .and_then(|w| w.open_with_url_and_target(&url, "_blank").ok().flatten())
        .is_some();
    if opened {
        // Leaking one object URL per preview-open is the standard tradeoff:
        // revoking eagerly would blank the tab before it loads.
        true
    } else {
        let _ = web_sys::Url::revoke_object_url(&url);
        download(filename, bytes, mime)
    }
}

// ---------------------------------------------------------------------------
// Attached images: in-memory bytes + localStorage persistence.
// ---------------------------------------------------------------------------

/// One attached image, mirroring `AttachedImage` metadata plus bytes.
#[derive(Clone)]
pub struct WebImage {
    pub name: String,
    pub ext: String,
    pub bytes: Vec<u8>,
}

thread_local! {
    static IMAGES: RefCell<HashMap<String, WebImage>> = RefCell::new(HashMap::new());
}

/// Base64 helpers (no extra deps: `base64` is already in the tree).
fn encode_b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode_b64(text: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(compact).ok()
}

/// Hydrate the in-memory image store from `localStorage` (once, at startup).
pub fn images_hydrate() {
    let Some(text) = load_text(IMAGES_KEY) else {
        return;
    };
    let Ok(raw): Result<HashMap<String, serde_json::Value>, _> =
        serde_json::from_str(&text)
    else {
        return;
    };
    IMAGES.with(|cell| {
        let mut map = cell.borrow_mut();
        for (id, v) in raw {
            let (Some(name), Some(ext), Some(data)) = (
                v.get("name").and_then(|s| s.as_str()),
                v.get("ext").and_then(|s| s.as_str()),
                v.get("data").and_then(|s| s.as_str()),
            ) else {
                continue;
            };
            if let Some(bytes) = decode_b64(data) {
                map.insert(
                    id,
                    WebImage {
                        name: name.to_string(),
                        ext: ext.to_string(),
                        bytes,
                    },
                );
            }
        }
    });
}

fn images_persist() {
    IMAGES.with(|cell| {
        let map = cell.borrow();
        let mut raw = serde_json::Map::new();
        for (id, img) in map.iter() {
            raw.insert(
                id.clone(),
                serde_json::json!({
                    "name": img.name,
                    "ext": img.ext,
                    "data": encode_b64(&img.bytes),
                }),
            );
        }
        store_text(IMAGES_KEY, &serde_json::Value::Object(raw).to_string());
    });
}

/// Find an identical image's id (attach dedupes by content, like native).
pub fn images_find_by_bytes(bytes: &[u8]) -> Option<String> {
    IMAGES.with(|cell| {
        cell.borrow()
            .iter()
            .find(|(_, img)| img.bytes == bytes)
            .map(|(id, _)| id.clone())
    })
}

pub fn images_get(id: &str) -> Option<(Vec<u8>, String)> {
    IMAGES.with(|cell| {
        cell.borrow()
            .get(id)
            .map(|img| (img.bytes.clone(), img.ext.clone()))
    })
}

pub fn images_insert(id: String, name: String, ext: String, bytes: Vec<u8>) {
    IMAGES.with(|cell| {
        cell.borrow_mut().insert(id, WebImage { name, ext, bytes });
    });
    images_persist();
}

// ---------------------------------------------------------------------------
// Export styles: in-memory name -> CSS map + localStorage persistence.
// ---------------------------------------------------------------------------

thread_local! {
    static STYLES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// Hydrate the style map from `localStorage` (once, at startup).
pub fn styles_hydrate() {
    let Some(text) = load_text(STYLES_KEY) else {
        return;
    };
    if let Ok(raw) = serde_json::from_str::<HashMap<String, String>>(&text) {
        STYLES.with(|cell| *cell.borrow_mut() = raw);
    }
}

fn styles_persist() {
    STYLES.with(|cell| {
        let map = cell.borrow();
        store_text(STYLES_KEY, &serde_json::to_string(&*map).unwrap_or_default());
    });
}

pub fn styles_list() -> Vec<String> {
    STYLES.with(|cell| {
        let mut names: Vec<String> = cell.borrow().keys().cloned().collect();
        names.sort();
        names
    })
}

pub fn styles_get(name: &str) -> Option<String> {
    STYLES.with(|cell| cell.borrow().get(name).cloned())
}

/// Insert or overwrite `name`, picking an unused `name 2`, `name 3`, …
/// suffix on collision like the native file flow. Returns the final name.
pub fn styles_set_named(name: &str, css: String, is_new: bool) -> String {
    let mut out = String::new();
    STYLES.with(|cell| {
        let mut map = cell.borrow_mut();
        let final_name = if is_new {
            let mut candidate = name.to_string();
            let mut n = 2;
            while map.contains_key(&candidate) {
                candidate = format!("{name} {n}");
                n += 1;
            }
            candidate
        } else {
            name.to_string()
        };
        map.insert(final_name.clone(), css);
        out = final_name;
    });
    styles_persist();
    out
}

pub fn styles_remove(name: &str) -> bool {
    let removed = STYLES.with(|cell| cell.borrow_mut().remove(name).is_some());
    if removed {
        styles_persist();
    }
    removed
}
