//! User-built themes: ten key colors auto-derived into a full palette.
//!
//! The builder edits [`CustomKeys`] (surfaces, text, accent, and three
//! semantic colors); [`resolve`] expands them into a complete
//! [`super::ThemePalette`] with fixed rules so every custom stays
//! coherent and readable. Storage is a per-machine `themes.json` next to
//! the library (same pattern as `update.json`), deliberately outside the
//! synced stores.

use std::path::{Path, PathBuf};

/// Colors the theme builder edits.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CustomKeys {
    pub background: [u8; 3],
    pub panel: [u8; 3],
    pub sidebar: [u8; 3],
    pub sheets: [u8; 3],
    pub text: [u8; 3],
    pub dim: [u8; 3],
    pub accent: [u8; 3],
    pub green: [u8; 3],
    pub amber: [u8; 3],
    pub red: [u8; 3],
}

impl CustomKeys {
    /// `(label, value)` pairs in editor order. The mutable borrows are to
    /// disjoint fields, so they can all live at once.
    pub fn entries(&mut self) -> [(&'static str, &mut [u8; 3]); 10] {
        [
            ("Background", &mut self.background),
            ("Header bar", &mut self.panel),
            ("Library sidebar", &mut self.sidebar),
            ("Sheets panel", &mut self.sheets),
            ("Text", &mut self.text),
            ("Secondary text", &mut self.dim),
            ("Accent", &mut self.accent),
            ("Green (code)", &mut self.green),
            ("Amber (italics)", &mut self.amber),
            ("Red (errors)", &mut self.red),
        ]
    }
}

/// One saved user theme.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CustomTheme {
    pub id: String,
    pub name: String,
    pub keys: CustomKeys,
}

impl CustomTheme {
    pub fn new(name: String, keys: CustomKeys) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            keys,
        }
    }
}

/// On-disk form of `themes.json`.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct ThemesFile {
    #[serde(default)]
    themes: Vec<CustomTheme>,
    #[serde(default)]
    active: Option<String>,
}

pub fn themes_path(dir: &Path) -> PathBuf {
    dir.join("themes.json")
}

/// Load `(themes, active_id)`. Missing or corrupt files yield empty
/// defaults rather than failing.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_all(dir: Option<PathBuf>) -> (Vec<CustomTheme>, Option<String>) {
    let Some(dir) = dir else {
        return (Vec::new(), None);
    };
    let Ok(text) = std::fs::read_to_string(themes_path(&dir)) else {
        return (Vec::new(), None);
    };
    let file: ThemesFile = serde_json::from_str(&text).unwrap_or_default();
    // Drop an active id that no longer resolves.
    let active = file
        .active
        .filter(|id| file.themes.iter().any(|t| &t.id == id));
    (file.themes, active)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn save_all(dir: Option<PathBuf>, themes: &[CustomTheme], active: Option<&str>) {
    let Some(dir) = dir else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let file = ThemesFile {
        themes: themes.to_vec(),
        active: active.map(|s| s.to_string()),
    };
    if let Ok(text) = serde_json::to_string_pretty(&file) {
        let _ = std::fs::write(themes_path(&dir), text);
    }
}

/// Browser build: themes persist in `localStorage` under [`THEMES_KEY`]
/// instead of the per-machine `themes.json` file.
#[cfg(target_arch = "wasm32")]
pub fn load_all(_dir: Option<PathBuf>) -> (Vec<CustomTheme>, Option<String>) {
    let Some(text) = crate::web::load_text(crate::web::THEMES_KEY) else {
        return (Vec::new(), None);
    };
    let file: ThemesFile = serde_json::from_str(&text).unwrap_or_default();
    let active = file
        .active
        .filter(|id| file.themes.iter().any(|t| &t.id == id));
    (file.themes, active)
}

/// Browser build: see [`load_all`].
#[cfg(target_arch = "wasm32")]
pub fn save_all(_dir: Option<PathBuf>, themes: &[CustomTheme], active: Option<&str>) {
    let file = ThemesFile {
        themes: themes.to_vec(),
        active: active.map(|s| s.to_string()),
    };
    if let Ok(text) = serde_json::to_string(&file) {
        crate::web::store_text(crate::web::THEMES_KEY, &text);
    }
}

/// Linear mix of two sRGB colors. Good enough for tints and washes.
fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t).round() as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t).round() as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t).round() as u8,
    ]
}

/// Relative luminance, 0 (black) to 1 (white).
fn luminance(c: [u8; 3]) -> f32 {
    (0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32) / 255.0
}

const WHITE: [u8; 3] = [255, 255, 255];
const NEAR_BLACK: [u8; 3] = [20, 20, 22];

/// Whether custom keys describe a dark theme, from background luminance.
pub fn is_dark_keys(keys: &CustomKeys) -> bool {
    luminance(keys.background) < 0.4
}

/// Expand key colors into a full palette plus its dark/light polarity.
/// Rules: body, markers, quotes, and secondary runs read from
/// text/dim; headings, links, and the caret use the accent; code is
/// green, italics amber, spelling errors red; cards interpolate from
/// the sheets surface toward the text so hover always sits between
/// resting and selected.
pub fn resolve(keys: &CustomKeys) -> (super::ThemePalette, bool) {
    let dark = is_dark_keys(keys);
    let extreme = if dark { WHITE } else { NEAR_BLACK };
    let k = keys;
    let palette = super::ThemePalette {
        bg_app: rgb(k.background),
        bg_panel: rgb(k.panel),
        text_main: rgb(k.text),
        text_bright: rgb(mix(k.text, extreme, 0.45)),
        text_dim: rgb(k.dim),
        accent: rgb(k.accent),
        ed_text: rgb(k.text),
        ed_marker: rgb(k.dim),
        ed_heading: rgb(k.accent),
        ed_code: rgb(k.green),
        ed_code_bg: rgb(k.panel),
        ed_code_underline: rgb(k.dim),
        ed_quote: rgb(k.dim),
        ed_link_url: rgb(k.accent),
        ed_dim2: rgb(k.dim),
        ed_strike: rgb(k.dim),
        ed_italic: rgb(k.amber),
        ed_caret: rgb(k.accent),
        grammar_spelling: rgb(k.red),
        ed_margin_mark: rgb(k.dim),
        text_faint: rgb(k.dim),
        card_selected: rgb(mix(k.sheets, k.text, 0.22)),
        card_hover: rgb(mix(k.sheets, k.text, 0.10)),
        bg_sheets: rgb(k.sheets),
        bg_sheets_header: rgb(mix(k.sheets, k.text, 0.06)),
        bg_sidebar: rgb(k.sidebar),
    };
    (palette, dark)
}

/// `[u8; 3]` to egui color.
pub fn rgb(c: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(c[0], c[1], c[2])
}

/// egui color to `[u8; 3]`.
pub fn triple(c: egui::Color32) -> [u8; 3] {
    [c.r(), c.g(), c.b()]
}

/// `#rrggbb` hex string.
pub fn to_hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// Parse `#rrggbb` or `rrggbb` (case-insensitive).
pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().strip_prefix('#').unwrap_or(s.trim());
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// Shared export file shape: friendly hex colors keyed by editor label.
#[derive(serde::Serialize, serde::Deserialize)]
struct ExportFile {
    name: String,
    #[serde(default = "export_version")]
    version: u32,
    colors: std::collections::BTreeMap<String, String>,
}

fn export_version() -> u32 {
    1
}

const COLOR_KEYS: [(&str, fn(&CustomKeys) -> [u8; 3]); 10] = [
    ("background", |k: &CustomKeys| k.background),
    ("panel", |k| k.panel),
    ("sidebar", |k| k.sidebar),
    ("sheets", |k| k.sheets),
    ("text", |k| k.text),
    ("dim", |k| k.dim),
    ("accent", |k| k.accent),
    ("green", |k| k.green),
    ("amber", |k| k.amber),
    ("red", |k| k.red),
];

/// Serialize a theme for export (fresh id is assigned on import).
pub fn export_json(theme: &CustomTheme) -> String {
    let colors = COLOR_KEYS
        .iter()
        .map(|(name, get)| (name.to_string(), to_hex(get(&theme.keys))))
        .collect();
    let file = ExportFile {
        name: theme.name.clone(),
        version: 1,
        colors,
    };
    serde_json::to_string_pretty(&file).unwrap_or_default()
}

/// Parse an exported theme file, assigning a fresh id. Requires all ten
/// colors in valid hex; extras are ignored.
pub fn import_json(text: &str) -> Result<CustomTheme, String> {
    let file: ExportFile =
        serde_json::from_str(text).map_err(|e| format!("Not a theme file ({e})"))?;
    let name = file.name.trim().to_string();
    if name.is_empty() {
        return Err("Theme file has no name".to_string());
    }
    let mut values = std::collections::HashMap::new();
    for (key, hex) in &file.colors {
        values.insert(
            key.clone(),
            parse_hex(hex).ok_or_else(|| format!("Bad color for “{key}”: {hex}"))?,
        );
    }
    let mut get = |key: &str| values.remove(key).ok_or_else(|| format!("Missing color: {key}"));
    let keys = CustomKeys {
        background: get("background")?,
        panel: get("panel")?,
        sidebar: get("sidebar")?,
        sheets: get("sheets")?,
        text: get("text")?,
        dim: get("dim")?,
        accent: get("accent")?,
        green: get("green")?,
        amber: get("amber")?,
        red: get("red")?,
    };
    Ok(CustomTheme::new(name, keys))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_dark() -> CustomKeys {
        CustomKeys {
            background: [40, 40, 40],
            panel: [48, 48, 49],
            sidebar: [40, 40, 41],
            sheets: [32, 32, 33],
            text: [205, 205, 208],
            dim: [130, 135, 145],
            accent: [110, 190, 235],
            green: [155, 220, 165],
            amber: [255, 220, 170],
            red: [224, 108, 117],
        }
    }

    #[test]
    fn dark_keys_resolve_dark_with_distinct_surfaces() {
        let (p, dark) = resolve(&sample_dark());
        assert!(dark);
        assert_ne!(p.bg_app, p.bg_panel);
        assert_ne!(p.bg_sidebar, p.bg_panel);
        assert_ne!(p.bg_sidebar, p.bg_sheets);
        assert_ne!(p.text_main, p.bg_app);
        // Cards interpolate from the sheets surface: hover sits between.
        for (r, h, s) in [
            (p.bg_sheets.r(), p.card_hover.r(), p.card_selected.r()),
            (p.bg_sheets.g(), p.card_hover.g(), p.card_selected.g()),
            (p.bg_sheets.b(), p.card_hover.b(), p.card_selected.b()),
        ] {
            assert!((r <= h && h <= s) || (r >= h && h >= s));
        }
    }

    #[test]
    fn light_background_resolves_light() {
        let mut keys = sample_dark();
        keys.background = [253, 246, 227];
        keys.text = [88, 110, 117];
        let (_, dark) = resolve(&keys);
        assert!(!dark);
    }

    #[test]
    fn hex_roundtrips() {
        assert_eq!(to_hex([255, 0, 171]), "#FF00AB");
        assert_eq!(parse_hex("#ff00ab"), Some([255, 0, 171]));
        assert_eq!(parse_hex("00FF00"), Some([0, 255, 0]));
        assert!(parse_hex("xyz").is_none());
        assert!(parse_hex("#12345").is_none());
    }

    #[test]
    fn export_import_roundtrips_with_fresh_id() {
        let theme = CustomTheme::new("Mine".to_string(), sample_dark());
        let json = export_json(&theme);
        let back = import_json(&json).expect("imports");
        assert_eq!(back.name, "Mine");
        assert_eq!(back.keys, theme.keys);
        assert_ne!(back.id, theme.id);
    }

    #[test]
    fn import_rejects_bad_files() {
        assert!(import_json("not json").is_err());
        assert!(import_json(r#"{"name":"","version":1,"colors":{}}"#).is_err());
        // Missing one color.
        let mut theme = CustomTheme::new("Mine".to_string(), sample_dark());
        let mut json: serde_json::Value =
            serde_json::from_str(&export_json(&theme)).unwrap();
        json["colors"].as_object_mut().unwrap().remove("red");
        assert!(import_json(&serde_json::to_string(&json).unwrap()).is_err());
        // Bad hex.
        theme.name = "Bad".to_string();
        let green_hex = to_hex(sample_dark().green);
        let bad = export_json(&theme).replace(&green_hex, "nope");
        assert!(import_json(&bad).is_err());
    }

    #[test]
    fn themes_file_roundtrips() {
        let dir = std::env::temp_dir().join(format!(
            "ariadne-themes-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let themes = vec![CustomTheme::new("Mine".to_string(), sample_dark())];
        let id = themes[0].id.clone();
        save_all(Some(dir.clone()), &themes, Some(&id));
        let (back, active) = load_all(Some(dir.clone()));
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].keys, sample_dark());
        assert_eq!(active.as_deref(), Some(id.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
        // Missing dir yields defaults.
        let (empty, none) = load_all(None);
        assert!(empty.is_empty() && none.is_none());
    }
}
