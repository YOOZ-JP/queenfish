//! Codex (ChatGPT plan) usage, read from the Codex CLI's own login files.

use std::path::PathBuf;

use base64::Engine;
use serde_json::Value;

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const UA: &str = concat!("queenfish/", env!("CARGO_PKG_VERSION"));
const AUTH_CLAIM: &str = "https://api.openai.com/auth";

/// One Codex login found on this machine.
pub struct CodexSource {
    pub email: Option<String>,
    pub access_token: String,
    pub account_id: Option<String>,
    pub expires_at: Option<u64>, // epoch seconds
    pub origin: String,          // "codex" | "codex-orca" | "codex-custom"
}

impl CodexSource {
    pub fn label(&self) -> String {
        self.email
            .clone()
            .or_else(|| self.account_id.clone())
            .unwrap_or_else(|| self.origin.clone())
    }

    fn is_expired(&self, now_secs: u64) -> bool {
        self.expires_at.is_some_and(|exp| exp <= now_secs)
    }
}

fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn parse_auth(bytes: &[u8], origin: &str) -> Option<CodexSource> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let tokens = v.get("tokens")?;
    let access_token = tokens.get("access_token")?.as_str()?.to_string();
    let id_claims = tokens
        .get("id_token")
        .and_then(|t| t.as_str())
        .and_then(jwt_claims);
    let access_claims = jwt_claims(&access_token);
    let email = id_claims
        .as_ref()
        .and_then(|c| c.get("email"))
        .and_then(|e| e.as_str())
        .map(String::from);
    let account_id = tokens
        .get("account_id")
        .and_then(|a| a.as_str())
        .map(String::from)
        .or_else(|| {
            id_claims
                .as_ref()?
                .get(AUTH_CLAIM)?
                .get("chatgpt_account_id")?
                .as_str()
                .map(String::from)
        });
    Some(CodexSource {
        email,
        access_token,
        account_id,
        expires_at: access_claims
            .as_ref()
            .and_then(|c| c.get("exp"))
            .and_then(|e| e.as_u64()),
        origin: origin.to_string(),
    })
}

/// Codex homes that may hold an `auth.json`: `$CODEX_HOME`, `~/.codex`,
/// Orca-managed Codex accounts, and `custom_codex_homes` from config.json.
fn candidate_homes(app: &tauri::AppHandle) -> Vec<(PathBuf, &'static str)> {
    let mut homes: Vec<(PathBuf, &'static str)> = Vec::new();
    let orca = dirs::data_dir().map(|d| d.join("orca"));
    if let Some(p) = std::env::var_os("CODEX_HOME").filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        let in_orca = orca.as_ref().is_some_and(|o| p.starts_with(o));
        homes.push((p, if in_orca { "codex-orca" } else { "codex" }));
    }
    if let Some(home) = dirs::home_dir() {
        homes.push((home.join(".codex"), "codex"));
    }
    if let Some(orca) = orca {
        if let Ok(rd) = std::fs::read_dir(orca.join("codex-accounts")) {
            for entry in rd.flatten() {
                homes.push((entry.path().join("home"), "codex-orca"));
            }
        }
        homes.push((orca.join("codex-runtime-home").join("home"), "codex-orca"));
    }
    if let Some(arr) = crate::accounts::read_config(app)
        .get("custom_codex_homes")
        .and_then(|x| x.as_array())
    {
        for p in arr.iter().filter_map(|x| x.as_str()) {
            homes.push((PathBuf::from(p), "codex-custom"));
        }
    }
    homes
}

/// The same account is often logged in under several homes; keep one login per
/// account, preferring the token that expires last.
fn dedupe(sources: Vec<CodexSource>) -> Vec<CodexSource> {
    let mut out: Vec<CodexSource> = Vec::new();
    for src in sources {
        let key = src.label();
        match out.iter_mut().find(|s| s.label() == key) {
            Some(existing) => {
                if src.expires_at > existing.expires_at {
                    *existing = src;
                }
            }
            None => out.push(src),
        }
    }
    out
}

pub fn discover(app: &tauri::AppHandle) -> Vec<CodexSource> {
    let found = candidate_homes(app)
        .into_iter()
        .filter_map(|(home, origin)| {
            let bytes = std::fs::read(home.join("auth.json")).ok()?;
            parse_auth(&bytes, origin)
        })
        .collect();
    dedupe(found)
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(UA)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("http client")
}

/// Plan + rate-limit windows for a Codex login.
///
/// Expired tokens are reported, never refreshed: Codex refresh tokens are
/// single-use, so refreshing here would invalidate the login the CLI has stored.
pub async fn fetch_usage(http: &reqwest::Client, src: &CodexSource) -> Result<Value, String> {
    const EXPIRED: &str = "토큰 만료 — Codex를 한 번 실행하면 갱신됩니다";
    if src.is_expired(crate::now_ms() / 1000) {
        return Err(EXPIRED.to_string());
    }
    let mut req = http
        .get(USAGE_URL)
        .bearer_auth(&src.access_token)
        .header("Accept", "application/json");
    if let Some(id) = &src.account_id {
        req = req.header("ChatGPT-Account-Id", id);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("Codex 사용량 요청 실패: {e}"))?;
    let status = resp.status();
    match status.as_u16() {
        401 | 403 => return Err(EXPIRED.to_string()),
        429 => return Err("HTTP 429 — 요청 한도, 잠시 후 자동 재시도".to_string()),
        _ if !status.is_success() => return Err(format!("Codex 사용량 HTTP {status}")),
        _ => {}
    }
    let mut v: Value = resp
        .json()
        .await
        .map_err(|e| format!("Codex 사용량 파싱 실패: {e}"))?;
    if let Some(obj) = v.as_object_mut() {
        // identifiers the panel never shows; keep them out of cache.json
        for k in ["user_id", "account_id", "email"] {
            obj.remove(k);
        }
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn jwt(claims: Value) -> String {
        let enc = |v: &Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap())
        };
        format!("{}.{}.sig", enc(&json!({"alg": "none"})), enc(&claims))
    }

    fn auth_json(email: &str, exp: u64, account_id: Option<&str>) -> Vec<u8> {
        let mut tokens = json!({
            "id_token": jwt(json!({
                "email": email,
                AUTH_CLAIM: {"chatgpt_account_id": "from-claim"}
            })),
            "access_token": jwt(json!({"exp": exp})),
            "refresh_token": "r",
        });
        if let Some(id) = account_id {
            tokens["account_id"] = json!(id);
        }
        serde_json::to_vec(&json!({"auth_mode": "chatgpt", "tokens": tokens})).unwrap()
    }

    #[test]
    fn parses_identity_and_expiry_from_tokens() {
        let src = parse_auth(&auth_json("a@example.com", 1_800_000_000, Some("acct-1")), "codex")
            .expect("parsed");
        assert_eq!(src.email.as_deref(), Some("a@example.com"));
        assert_eq!(src.account_id.as_deref(), Some("acct-1"));
        assert_eq!(src.expires_at, Some(1_800_000_000));
        assert!(!src.is_expired(1_799_999_999));
        assert!(src.is_expired(1_800_000_000));
    }

    #[test]
    fn account_id_falls_back_to_id_token_claim() {
        let src = parse_auth(&auth_json("a@example.com", 1, None), "codex").expect("parsed");
        assert_eq!(src.account_id.as_deref(), Some("from-claim"));
    }

    #[test]
    fn api_key_only_login_is_ignored() {
        let bytes = serde_json::to_vec(&json!({"OPENAI_API_KEY": "sk-x", "tokens": null})).unwrap();
        assert!(parse_auth(&bytes, "codex").is_none());
    }

    #[test]
    fn dedupe_keeps_the_longest_lived_login_per_account() {
        let stale = parse_auth(&auth_json("a@example.com", 100, Some("x")), "codex-orca").unwrap();
        let fresh = parse_auth(&auth_json("a@example.com", 900, Some("x")), "codex").unwrap();
        let other = parse_auth(&auth_json("b@example.com", 500, Some("y")), "codex-orca").unwrap();
        let out = dedupe(vec![stale, other, fresh]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].label(), "a@example.com");
        assert_eq!(out[0].expires_at, Some(900));
        assert_eq!(out[0].origin, "codex");
        assert_eq!(out[1].label(), "b@example.com");
    }
}
