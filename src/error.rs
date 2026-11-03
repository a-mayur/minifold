use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    Unauthorized,
    InvalidRequest,
    MissingFilename,
    InvalidLevel,
    InvalidSetting,
    EmptyFile,
    CorruptInput,
    PasswordProtected,
    ImageTooLarge,
    Timeout,
    HwUnavailable,
    HwEncodeFailed,
    FileTooLarge,
    UploadInterrupted,
    UnsupportedType,
    NotFound,
    NotReady,
    DiskFull,
    EngineFailed,
    Internal,
}

impl ErrorCode {
    fn status(self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::InvalidRequest
            | Self::MissingFilename
            | Self::InvalidLevel
            | Self::InvalidSetting
            | Self::EmptyFile
            | Self::UploadInterrupted => StatusCode::BAD_REQUEST,
            Self::FileTooLarge | Self::ImageTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::CorruptInput | Self::PasswordProtected => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Timeout => StatusCode::GATEWAY_TIMEOUT,
            Self::UnsupportedType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::NotReady => StatusCode::CONFLICT,
            Self::DiskFull => StatusCode::INSUFFICIENT_STORAGE,
            Self::HwUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::EngineFailed | Self::HwEncodeFailed | Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// An error with a stable code and a message that is safe to show to the user.
#[derive(Clone, Debug, Serialize)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    /// Server-side I/O failure (storage, not the engine).
    pub fn io(err: &std::io::Error) -> Self {
        if err.kind() == std::io::ErrorKind::StorageFull {
            return Self::new(ErrorCode::DiskFull, "The server's disk is full. Ask your admin to free up space.");
        }
        tracing::error!("I/O error: {err}");
        Self::new(ErrorCode::Internal, "Unexpected server error. Check the server logs.")
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.code.status(), Json(self)).into_response()
    }
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::human_bytes;

    #[test]
    fn formats_sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2 * 1024 * 1024 * 1024), "2.0 GB");
    }
}
