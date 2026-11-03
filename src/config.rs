use std::net::SocketAddr;
use std::path::PathBuf;

const MIN_TOKEN_LEN: usize = 16;

/// Startup configuration from environment variables. Admin-editable values live in `Settings`.
pub struct Config {
    pub auth_token: String,
    pub admin_token: String,
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub bind: SocketAddr,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let auth_token = token("MINIFOLD_AUTH_TOKEN")?;
        let admin_token = token("MINIFOLD_ADMIN_TOKEN")?;
        if auth_token == admin_token {
            return Err("MINIFOLD_ADMIN_TOKEN must be different from MINIFOLD_AUTH_TOKEN.".into());
        }

        let bind_raw = std::env::var("MINIFOLD_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
        let bind = bind_raw
            .parse()
            .map_err(|_| format!("MINIFOLD_BIND is not a valid address: {bind_raw:?} (expected e.g. 0.0.0.0:8080)"))?;

        Ok(Self {
            auth_token,
            admin_token,
            data_dir: PathBuf::from(std::env::var("MINIFOLD_DATA_DIR").unwrap_or_else(|_| "/data".into())),
            config_dir: PathBuf::from(std::env::var("MINIFOLD_CONFIG_DIR").unwrap_or_else(|_| "/config".into())),
            bind,
        })
    }

    pub fn settings_path(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }
}

fn token(var: &str) -> Result<String, String> {
    let value = std::env::var(var)
        .map_err(|_| format!("{var} is not set. Set it to a random string of at least {MIN_TOKEN_LEN} characters."))?;
    let value = value.trim().to_string();
    if value.len() < MIN_TOKEN_LEN {
        return Err(format!("{var} must be at least {MIN_TOKEN_LEN} characters."));
    }
    if !value.chars().all(|c| c.is_ascii_graphic() && c != ';' && c != ',') {
        return Err(format!("{var} may only contain visible ASCII characters, without ';' or ','."));
    }
    Ok(value)
}
