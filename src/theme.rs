//! Material 3 color themes. `web/palettes.json` holds the light and dark color roles of each
//! palette, generated from a pastel seed color by `tools/gen-palettes.mjs` (Google's
//! material-color-utilities, which keeps text contrast readable in every scheme).

use std::sync::LazyLock;

use serde_json::{Map, Value};

use crate::settings::{Settings, ThemeMode};

static PALETTES: LazyLock<Map<String, Value>> =
    LazyLock::new(|| serde_json::from_str(include_str!("../web/palettes.json")).expect("web/palettes.json is valid"));

/// Every palette as `.m3-<id>-<mode>` classes, for the admin page's color swatches.
static PALETTES_CSS: LazyLock<String> = LazyLock::new(|| {
    PALETTES
        .keys()
        .flat_map(|id| ["light", "dark"].map(|mode| format!(".m3-{id}-{mode}{{{}}}\n", vars(id, mode))))
        .collect()
});

pub fn exists(id: &str) -> bool {
    PALETTES.contains_key(id)
}

pub fn ids() -> Vec<&'static str> {
    PALETTES.keys().map(String::as_str).collect()
}

pub fn palettes_css() -> &'static str {
    &PALETTES_CSS
}

/// The site's colors: the chosen light palette by default, the dark one under
/// `data-mode="dark"`, and the admin's default mode for `theme.js` to read.
pub fn theme_css(settings: &Settings) -> String {
    let mode = match settings.theme_mode {
        ThemeMode::System => "system",
        ThemeMode::Light => "light",
        ThemeMode::Dark => "dark",
    };
    format!(
        ":root{{--default-mode:{mode}}}\n:root{{color-scheme:light;{}}}\n:root[data-mode=\"dark\"]{{color-scheme:dark;{}}}\n",
        vars(&settings.theme_light, "light"),
        vars(&settings.theme_dark, "dark")
    )
}

/// `--primary:#…;--on-primary:#…;…` for one palette and mode (palette ids are validated in Settings).
fn vars(id: &str, mode: &str) -> String {
    let roles = PALETTES[id][mode].as_object().expect("palette has light and dark roles");
    roles
        .iter()
        .map(|(role, color)| format!("--{}:{};", kebab(role), color.as_str().expect("colors are strings")))
        .collect()
}

fn kebab(camel: &str) -> String {
    camel.chars().fold(String::new(), |mut out, c| {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_six_complete_palettes() {
        assert_eq!(ids().len(), 6);
        for id in ids() {
            for mode in ["light", "dark"] {
                assert_eq!(PALETTES[id][mode].as_object().unwrap().len(), 19, "{id} {mode}");
            }
        }
    }

    #[test]
    fn theme_css_uses_the_chosen_palettes() {
        let s = Settings { theme_mode: ThemeMode::Dark, theme_light: "peach".into(), theme_dark: "sage".into(), ..Settings::defaults() };
        let css = theme_css(&s);
        assert!(css.contains("--default-mode:dark"));
        assert!(css.contains("--primary:#8a5022"), "peach light primary");
        let dark = css.split("[data-mode=\"dark\"]").nth(1).expect("dark block");
        assert!(dark.contains("--primary:#aed18d;"), "sage dark primary in the dark block");
        assert!(css.contains("--on-primary-container:"), "roles are kebab-case");
        assert_eq!(palettes_css().matches(".m3-").count(), 12);
    }
}
