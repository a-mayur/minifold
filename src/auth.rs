use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::error::{AppError, ErrorCode};

pub const USER_COOKIE: &str = "minifold_session";
pub const ADMIN_COOKIE: &str = "minifold_admin";
const COOKIE_MAX_AGE_SECS: u32 = 30 * 24 * 60 * 60;

/// Rejects requests without the shared user token (cookie or `Authorization: Bearer`),
/// unless no token is set.
pub async fn require_user(State(app): State<AppState>, req: Request, next: Next) -> Response {
    if allowed(req.headers(), USER_COOKIE, app.config.auth_token.as_deref()) {
        return next.run(req).await;
    }
    AppError::new(ErrorCode::Unauthorized, "Sign in with the access token to continue.").into_response()
}

/// Rejects requests without the admin token, unless no admin token is set.
pub async fn require_admin(State(app): State<AppState>, req: Request, next: Next) -> Response {
    if allowed(req.headers(), ADMIN_COOKIE, app.config.admin_token.as_deref()) {
        return next.run(req).await;
    }
    AppError::new(ErrorCode::Unauthorized, "Sign in with the admin token to continue.").into_response()
}

/// `expected` is `None` when sign-in is off. The Bearer header carries the token as is,
/// the cookie carries it percent-encoded (see `session_cookie`).
fn allowed(headers: &HeaderMap, cookie: &str, expected: Option<&str>) -> bool {
    let Some(expected) = expected else { return true };
    if let Some(bearer) = bearer(headers) {
        return tokens_match(bearer, expected.as_bytes());
    }
    cookie_value(headers, cookie).is_some_and(|value| tokens_match(value.as_bytes(), encode(expected).as_bytes()))
}

pub fn tokens_match(given: &[u8], expected: &[u8]) -> bool {
    // Constant-time comparison so response timing doesn't reveal the token.
    given.len() == expected.len() && given.iter().zip(expected).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// Tokens may contain anything (spaces, ';', non-ASCII), so the cookie holds them percent-encoded.
pub fn session_cookie(name: &str, token: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("{name}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={COOKIE_MAX_AGE_SECS}", encode(token)))
        .expect("percent-encoded value is header-safe")
}

fn encode(token: &str) -> String {
    token
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { char::from(b).to_string() } else { format!("%{b:02X}") })
        .collect()
}

pub fn clear_cookie(name: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("{name}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"))
        .expect("cookie name is a constant")
}

fn bearer(headers: &HeaderMap) -> Option<&[u8]> {
    headers.get(header::AUTHORIZATION)?.as_bytes().strip_prefix(b"Bearer ")
}

fn cookie_value<'a>(headers: &'a HeaderMap, cookie: &str) -> Option<&'a str> {
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
        assert!(tokens_match(b"abcdefghijklmnop", b"abcdefghijklmnop"));
        assert!(!tokens_match(b"abcdefghijklmnoq", b"abcdefghijklmnop"));
        assert!(!tokens_match(b"abc", b"abcdefghijklmnop"));
    }

    #[test]
    fn reads_the_right_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("minifold_session=user-token; minifold_admin=admin-token"));
        assert_eq!(cookie_value(&headers, USER_COOKIE), Some("user-token"));
        assert_eq!(cookie_value(&headers, ADMIN_COOKIE), Some("admin-token"));
    }

    fn with(name: &str, value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(name.parse::<header::HeaderName>().unwrap(), HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn any_token_round_trips_through_the_cookie() {
        for token in ["a", "x y; z,=", "ünïcødé 🔑", "same"] {
            let set = session_cookie(USER_COOKIE, token);
            let pair = set.to_str().unwrap().split(';').next().unwrap().to_string();
            assert!(allowed(&with("cookie", &pair), USER_COOKIE, Some(token)), "{token:?}");
            assert!(!allowed(&with("cookie", &pair), USER_COOKIE, Some("other")));
        }
    }

    #[test]
    fn bearer_and_no_token() {
        assert!(allowed(&with("authorization", "Bearer x y; z"), USER_COOKIE, Some("x y; z")));
        assert!(!allowed(&with("authorization", "Bearer wrong"), USER_COOKIE, Some("x")));
        assert!(!allowed(&HeaderMap::new(), USER_COOKIE, Some("x")));
        assert!(allowed(&HeaderMap::new(), USER_COOKIE, None), "no token set: sign-in is off");
    }
}
