use std::str::FromStr;

use serde::Serialize;

use crate::error::{AppError, ErrorCode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Low,
    Medium,
    High,
    Extra,
}

impl FromStr for Level {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "extra" => Ok(Self::Extra),
            _ => Err(AppError::new(
                ErrorCode::InvalidLevel,
                format!("Unknown compression level {s:?}. Use low, medium, high or extra."),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Level;

    #[test]
    fn parses_known_levels_only() {
        assert_eq!("extra".parse::<Level>().unwrap(), Level::Extra);
        assert!("max".parse::<Level>().is_err());
        assert!("Low".parse::<Level>().is_err());
    }
}
