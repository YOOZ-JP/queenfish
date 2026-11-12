//! In-app "add account": standard OAuth authorization-code + PKCE flow.
//! The user signs in themselves in their browser; we only receive the code
//! on a localhost callback and exchange it. Tokens are stored in Queenfish's
//! own keychain items (never touching Claude Code's entries).

use rand::RngCore;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};
use tauri_plugin_opener::OpenerExt;

const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const SCOPES: &str =
    "user:inference user:profile user:sessions:claude_code user:plugins user:mcp_servers user:file_upload";

pub const KEYCHAIN_PREFIX: &str = "Queenfish-credentials-";

fn b64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub async fn start_login(app: AppHandle) -> Result<(), String> {
    let mut verifier_bytes = [0u8; 48];
    rand::thread_rng().fill_bytes(&mut verifier_bytes);
    let verifier = b64url(&verifier_bytes);
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    let mut state_bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut state_bytes);
    let state = b64url(&state_bytes);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("포트 바인드 실패: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://localhost:{port}/callback");

    let url = format!(
        "{AUTHORIZE_URL}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        crate::api::CLIENT_ID,
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(SCOPES),
        challenge,
        state,
    );
    app.opener()
        .open_url(url, None::<String>)
        .map_err(|e| e.to_string())?;

    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        match wait_for_code(listener, &state).await {
            Ok(code) => match exchange_and_store(&code, &verifier, &redirect_uri).await {
                Ok(email) => {
                    let _ = app2.emit("login-done", format!("ok:{email}"));
                    crate::maybe_refresh(&app2, true);
                }
                Err(e) => {
                    let _ = app2.emit("login-done", format!("err:{e}"));
                }
            },
            Err(e) => {
                let _ = app2.emit("login-done", format!("err:{e}"));
            }
        }
    });
    Ok(())
}

async fn wait_for_code(
    listener: tokio::net::TcpListener,
    expect_state: &str,
) -> Result<String, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fut = async {
        loop {
            let (mut sock, _) = listener.accept().await.map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let line = req.lines().next().unwrap_or("");
            let path = line.split_whitespace().nth(1).unwrap_or("");
            if !path.starts_with("/callback") {
                let _ = sock
                    .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                    .await;
                continue;
            }
            let qs = path.splitn(2, '?').nth(1).unwrap_or("");
            let mut code = None;
            let mut got_state = None;
            let mut error = None;
            for pair in qs.split('&') {
                let mut it = pair.splitn(2, '=');
                let k = it.next().unwrap_or("");
                let v = it.next().unwrap_or("");
                match k {
                    "code" => code = Some(v.to_string()),
                    "state" => got_state = Some(v.to_string()),
                    "error" => error = Some(v.to_string()),
                    _ => {}
                }
            }
            let body = "<html><meta charset=\"utf-8\"><body style=\"font-family:sans-serif;padding:40px\">\u{2705} Queenfish 계정 추가 완료 — 이 창은 닫아도 됩니다.</body></html>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            if let Some(e) = error {
                return Err(format!("로그인 거부: {e}"));
            }
            if got_state.as_deref() != Some(expect_state) {
                return Err("state 불일치".to_string());
            }
            return code.ok_or_else(|| "code 없음".to_string());
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(300), fut)
        .await
        .map_err(|_| "로그인 시간 초과 (5분)".to_string())?
}

async fn exchange_and_store(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<String, String> {
    let http = reqwest::Client::new();
    // code may arrive as "code#state" in some flows — keep only the code part
    let code = code.split('#').next().unwrap_or(code);
    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": redirect_uri,
        "client_id": crate::api::CLIENT_ID,
        "code_verifier": verifier,
    });
    let resp = http
        .post(TOKEN_URL)
        .header("User-Agent", "claude-code/2.1.0")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("토큰 교환 실패: {e}"))?;
    let status = resp.status();
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("토큰 응답 파싱 실패: {e}"))?;
    if !status.is_success() {
        let msg = v
            .get("error_description")
            .or_else(|| v.get("error"))
            .map(|e| e.to_string())
            .unwrap_or_default();
        return Err(format!("토큰 교환 HTTP {status} {msg}"));
    }
    let access = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .ok_or("access_token 없음")?;
    let refresh = v.get("refresh_token").and_then(|x| x.as_str());
    let expires_in = v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(28800);

    let email = crate::api::Client::new()
        .fetch_profile_email(access)
        .await
        .ok_or("프로필 조회 실패 (토큰은 수신됨)")?;

    store_credentials(&email, access, refresh, crate::now_ms() + expires_in * 1000)?;
    Ok(email)
}

/// Write a login to Queenfish's own keychain item for `email` (same JSON shape
/// Claude Code uses, so discovery parses both alike).
pub fn store_credentials(
    email: &str,
    access_token: &str,
    refresh_token: Option<&str>,
    expires_at_ms: u64,
) -> Result<(), String> {
    let blob = serde_json::json!({
        "claudeAiOauth": {
            "accessToken": access_token,
            "refreshToken": refresh_token,
            "expiresAt": expires_at_ms,
        }
    });
    let service = format!("{KEYCHAIN_PREFIX}{email}");
    let entry = keyring::Entry::new(&service, email).map_err(|e| e.to_string())?;
    entry
        .set_password(&blob.to_string())
        .map_err(|e| format!("Keychain 저장 실패: {e}"))
}

/// Delete a Queenfish-managed credential. Never touches Claude Code's items.
pub fn delete_account(email: &str) -> Result<(), String> {
    let service = format!("{KEYCHAIN_PREFIX}{email}");
    let entry = keyring::Entry::new(&service, email).map_err(|e| e.to_string())?;
    entry
        .delete_credential()
        .map_err(|e| format!("Keychain 삭제 실패: {e}"))
}
