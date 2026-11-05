use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

const HEADER: &str = "# Minifold settings.\n# Change them on the admin page (/admin) or here, then restart Minifold to apply.\n\n";

/// Admin-editable settings, stored in `settings.toml`. Every key is required.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Largest file a user may upload, in GB.
    pub max_upload_gb: u64,
    /// How many files are compressed at the same time.
    pub workers: usize,
    /// Delete the uploaded original as soon as its job finishes or fails.
    pub delete_originals_after_job: bool,
    /// Used only when `delete_originals_after_job` is false.
    pub original_retention_minutes: u64,
    /// How long a compressed file is kept after its job finished.
    pub output_retention_minutes: u64,
    /// Delete a compressed file as soon as it has been downloaded once.
    pub delete_after_download: bool,
    /// Remove EXIF/GPS/XMP and similar metadata from images (color profile and orientation are kept).
    pub strip_metadata: bool,
    /// Hardware video encoder; only accelerators detected at startup can be chosen.
    pub hw_accel: HwAccel,
    /// What visitors see first; each visitor can switch with the button in the top bar.
    pub theme_mode: ThemeMode,
    /// Palette ids from web/palettes.json. Theme settings apply without a restart.
    pub theme_light: String,
    pub theme_dark: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HwAccel {
    Off,
    Nvenc,
    Qsv,
    Vaapi,
}

impl HwAccel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "No hardware acceleration",
            Self::Nvenc => "NVIDIA NVENC",
            Self::Qsv => "Intel Quick Sync",
            Self::Vaapi => "VA-API (Intel/AMD)",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct InvalidSetting {
    pub field: &'static str,
    pub message: String,
}

impl Settings {
    pub fn defaults() -> Self {
        Self {
            max_upload_gb: 2,
            workers: 2,
            delete_originals_after_job: true,
            original_retention_minutes: 60,
            output_retention_minutes: 60,
            delete_after_download: false,
            strip_metadata: true,
            hw_accel: HwAccel::Off,
            theme_mode: ThemeMode::System,
            theme_light: "peach".into(),
            theme_dark: "peach".into(),
        }
    }

    pub fn validate(&self) -> Result<(), InvalidSetting> {
        in_range("max_upload_gb", "Maximum file size", self.max_upload_gb, 1, 1024, "GB")?;
        in_range("workers", "Files compressed at the same time", self.workers as u64, 1, 64, "")?;
        in_range("original_retention_minutes", "Keep originals for", self.original_retention_minutes, 1, 10_080, "minutes")?;
        in_range("output_retention_minutes", "Keep compressed files for", self.output_retention_minutes, 1, 10_080, "minutes")?;
        for (field, id) in [("theme_light", &self.theme_light), ("theme_dark", &self.theme_dark)] {
            if !crate::theme::exists(id) {
                let message = format!("Unknown color palette {id:?}. Choose one of: {}.", crate::theme::ids().join(", "));
                return Err(InvalidSetting { field, message });
            }
        }
        Ok(())
    }

    /// True when `self` (saved) differs from `active` in anything a restart must apply.
    /// Theme settings take effect immediately, so they don't count.
    pub fn needs_restart(&self, active: &Settings) -> bool {
        let theme_as_active = Settings {
            theme_mode: active.theme_mode,
            theme_light: active.theme_light.clone(),
            theme_dark: active.theme_dark.clone(),
            ..self.clone()
        };
        theme_as_active != *active
    }

    pub fn max_upload_bytes(&self) -> u64 {
        self.max_upload_gb * 1024 * 1024 * 1024
    }

    pub fn original_retention(&self) -> Duration {
        Duration::from_secs(self.original_retention_minutes * 60)
    }

    pub fn output_retention(&self) -> Duration {
        Duration::from_secs(self.output_retention_minutes * 60)
    }
}

fn in_range(field: &'static str, label: &str, value: u64, min: u64, max: u64, unit: &str) -> Result<(), InvalidSetting> {
    if (min..=max).contains(&value) {
        return Ok(());
    }
    let unit = if unit.is_empty() { String::new() } else { format!(" {unit}") };
    Err(InvalidSetting { field, message: format!("{label} must be between {min} and {max}{unit}.") })
}

/// Loads `settings.toml`, creating it with defaults on first start. Invalid files stop startup.
pub fn load_or_create(path: &Path) -> Result<Settings, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let settings = Settings::defaults();
            save(path, &settings).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
            tracing::info!("created default settings at {}", path.display());
            return Ok(settings);
        }
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let mut table: toml::Table = toml::from_str(&text).map_err(|e| format!("{} is invalid: {e}", path.display()))?;
    let added = add_new_keys(&mut table);
    let settings: Settings =
        toml::Value::Table(table).try_into().map_err(|e| format!("{} is invalid: {e}", path.display()))?;
    if !added.is_empty() {
        save(path, &settings).map_err(|e| format!("cannot update {}: {e}", path.display()))?;
        tracing::info!("added new setting(s) {} with default values to {}", added.join(", "), path.display());
    }
    settings
        .validate()
        .map_err(|e| format!("{} is invalid: {} ({})", path.display(), e.message, e.field))?;
    Ok(settings)
}

/// Settings introduced by an upgrade are added with their default value, so an older
/// settings.toml keeps working. Unknown keys still stop startup.
fn add_new_keys(table: &mut toml::Table) -> Vec<String> {
    let toml::Value::Table(defaults) = toml::Value::try_from(Settings::defaults()).expect("defaults serialize") else {
        unreachable!("settings serialize to a table");
    };
    let mut added = Vec::new();
    for (key, value) in defaults {
        if !table.contains_key(&key) {
            added.push(key.clone());
            table.insert(key, value);
        }
    }
    added
}

/// Writes atomically (temp file + rename) so a crash never leaves a half-written file.
pub fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    let body = toml::to_string(settings).expect("settings always serialize");
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, format!("{HEADER}{body}"))?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("minifold-settings-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings.toml")
    }

    #[test]
    fn creates_defaults_then_reads_them_back() {
        let path = temp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_or_create(&path).unwrap(), Settings::defaults());
        let changed = Settings { workers: 4, delete_after_download: true, ..Settings::defaults() };
        save(&path, &changed).unwrap();
        assert_eq!(load_or_create(&path).unwrap(), changed);
    }

    #[test]
    fn upgrades_an_older_file_by_adding_new_keys() {
        let path = temp_path("upgrade");
        let old = "max_upload_gb = 5\nworkers = 3\ndelete_originals_after_job = true\noriginal_retention_minutes = 60\noutput_retention_minutes = 30\ndelete_after_download = false\n";
        std::fs::write(&path, old).unwrap();
        let s = load_or_create(&path).unwrap();
        assert_eq!((s.max_upload_gb, s.workers, s.output_retention_minutes, s.strip_metadata), (5, 3, 30, true));
        assert!(std::fs::read_to_string(&path).unwrap().contains("strip_metadata = true"), "file updated");
    }

    #[test]
    fn theme_changes_need_no_restart_but_others_do() {
        let active = Settings::defaults();
        let theme = Settings { theme_mode: ThemeMode::Dark, theme_light: "sage".into(), ..Settings::defaults() };
        assert!(!theme.needs_restart(&active));
        let workers = Settings { workers: 5, ..theme };
        assert!(workers.needs_restart(&active));
    }

    #[test]
    fn rejects_unknown_palettes() {
        let bad = Settings { theme_dark: "neon".into(), ..Settings::defaults() };
        let err = bad.validate().unwrap_err();
        assert_eq!(err.field, "theme_dark");
        assert!(err.message.contains("peach"), "lists the valid palettes");
    }

    #[test]
    fn rejects_unknown_missing_and_out_of_range_values() {
        let path = temp_path("invalid");
        std::fs::write(&path, "max_upload_gb = 2\nworkerz = 2\n").unwrap();
        assert!(load_or_create(&path).unwrap_err().contains("unknown field"));

        std::fs::write(&path, "max_upload_gb = \"two\"\n").unwrap();
        assert!(load_or_create(&path).unwrap_err().contains("invalid type"));

        let bad = Settings { workers: 0, ..Settings::defaults() };
        save(&path, &bad).unwrap();
        assert!(load_or_create(&path).unwrap_err().contains("between 1 and 64"));
    }
}
