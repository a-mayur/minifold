use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::error::{AppError, ErrorCode};

pub const USER_COOKIE: &str = "minifold_session";
pub const ADMIN_COOKIE: &str = "minifold_admin";
const COOKIE_MAX_AGE_SECS: u32 = 30 * 24 * 60 * 60;

/// Rejects requests without the shared user token (cookie or `Authorization: Bearer`).
pub async fn require_user(State(app): State<AppState>, req: Request, next: Next) -> Response {
    if presents(req.headers(), USER_COOKIE, &app.config.auth_token) {
        return next.run(req).await;
    }
    AppError::new(ErrorCode::Unauthorized, "Sign in with the access token to continue.").into_response()
}

/// Rejects requests without the admin token.
pub async fn require_admin(State(app): State<AppState>, req: Request, next: Next) -> Response {
    if presents(req.headers(), ADMIN_COOKIE, &app.config.admin_token) {
        return next.run(req).await;
    }
    AppError::new(ErrorCode::Unauthorized, "Sign in with the admin token to continue.").into_response()
}

fn presents(headers: &HeaderMap, cookie: &str, expected: &str) -> bool {
    presented_token(headers, cookie).is_some_and(|token| tokens_match(token, expected))
}

pub fn tokens_match(given: &str, expected: &str) -> bool {
    // Constant-time comparison so response timing doesn't reveal the token.
    given.len() == expected.len()
        && given.bytes().zip(expected.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

pub fn session_cookie(name: &str, token: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("{name}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={COOKIE_MAX_AGE_SECS}"))
        .expect("token was validated as a header-safe value")
}

pub fn clear_cookie(name: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("{name}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"))
        .expect("cookie name is a constant")
}

fn presented_token<'a>(headers: &'a HeaderMap, cookie: &str) -> Option<&'a str> {
    if let Some(bearer) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        return Some(bearer);
    }
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == cookie)
        .map(|(_, value)| value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_tokens_exactly() {
        assert!(tokens_match("abcdefghijklmnop", "abcdefghijklmnop"));
        assert!(!tokens_match("abcdefghijklmnoq", "abcdefghijklmnop"));
        assert!(!tokens_match("abc", "abcdefghijklmnop"));
    }

    #[test]
    fn reads_the_right_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("minifold_session=user-token; minifold_admin=admin-token"));
        assert_eq!(presented_token(&headers, USER_COOKIE), Some("user-token"));
        assert_eq!(presented_token(&headers, ADMIN_COOKIE), Some("admin-token"));
    }
}
