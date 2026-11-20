use std::env::VarError;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Startup configuration from environment variables. Admin-editable values live in `Settings`.
pub struct Config {
    /// `None`: the app needs no sign-in.
    pub auth_token: Option<String>,
    /// `None`: the admin page needs no sign-in.
    pub admin_token: Option<String>,
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub bind: SocketAddr,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let config_dir = PathBuf::from(std::env::var("MINIFOLD_CONFIG_DIR").unwrap_or_else(|_| "/config".into()));
        std::fs::create_dir_all(&config_dir).map_err(|e| format!("cannot create {}: {e}", config_dir.display()))?;
        let auth_token = token("MINIFOLD_AUTH_TOKEN", std::env::var("MINIFOLD_AUTH_TOKEN"))?;
        let admin_token = token("MINIFOLD_ADMIN_TOKEN", std::env::var("MINIFOLD_ADMIN_TOKEN"))?;
        if auth_token.is_none() {
            tracing::warn!("MINIFOLD_AUTH_TOKEN is empty: sign-in is off, anyone who can reach this server can use it");
        }
        if admin_token.is_none() {
            tracing::warn!("MINIFOLD_ADMIN_TOKEN is empty: the admin page is open to anyone who can reach this server");
        }

        let bind_raw = std::env::var("MINIFOLD_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
        let bind = bind_raw
            .parse()
            .map_err(|_| format!("MINIFOLD_BIND is not a valid address: {bind_raw:?} (expected e.g. 0.0.0.0:8080)"))?;

        Ok(Self {
            auth_token,
            admin_token,
            data_dir: PathBuf::from(std::env::var("MINIFOLD_DATA_DIR").unwrap_or_else(|_| "/data".into())),
            config_dir,
            bind,
        })
    }

    pub fn settings_path(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }
}

/// Any value is used exactly as given. Empty and unset both mean "no sign-in"
/// (Unraid passes an empty template field as unset).
fn token(name: &str, value: Result<String, VarError>) -> Result<Option<String>, String> {
    match value {
        Ok(v) if v.is_empty() => Ok(None),
        Ok(v) => Ok(Some(v)),
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => Err(format!("{name} is not valid UTF-8.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_unset_turns_sign_in_off() {
        assert_eq!(token("T", Err(VarError::NotPresent)).unwrap(), None);
        assert_eq!(token("T", Ok(String::new())).unwrap(), None);
    }

    #[test]
    fn any_value_is_kept_exactly() {
        for v in ["a", " spaced; with, commas ", "ünïcødé 🔑", "x".repeat(500).as_str()] {
            assert_eq!(token("T", Ok(v.to_string())).unwrap().as_deref(), Some(v));
        }
    }

    #[test]
    fn non_utf8_fails_fast() {
        use std::os::unix::ffi::OsStringExt;
        let bad = std::ffi::OsString::from_vec(vec![0xff, 0xfe]);
        assert!(token("T", Err(VarError::NotUnicode(bad))).unwrap_err().contains("UTF-8"));
    }
}
