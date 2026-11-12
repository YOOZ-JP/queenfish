use serde_json::{json, Value};

/// Claude Code's public OAuth client id (used only for refresh-token grant).
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const UA: &str = "claude-code/2.1.0";
const OAUTH_BETA: &str = "oauth-2025-04-20";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const ARTIFACTS_MCP_URL: &str = "https://claude.ai/api/artifacts/mcp";

pub const TOKEN_EXPIRED: &str = "토큰 만료 — 이 계정으로 Claude를 한 번 실행하면 갱신됩니다";

pub struct RefreshedToken {
    pub access_token: String,
    /// Set when the server rotated the refresh token; the old one is then dead.
    pub refresh_token: Option<String>,
    pub expires_in: u64, // seconds
}

pub struct Artifact {
    pub slug: String,
    pub title: String,
    pub description: Option<String>,
    pub favicon: Option<String>,
    pub url: String,
    pub updated_at: Option<String>,
    pub rel: String, // "mine" | "shared"
    pub type_title: Option<String>,
}

pub struct Client {
    http: reqwest::Client,
}

impl Client {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent(UA)
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("http client"),
        }
    }

    /// Authoritative owner email for a token (claude.ai account).
    pub async fn fetch_profile_email(&self, token: &str) -> Option<String> {
        let resp = self
            .http
            .get(PROFILE_URL)
            .bearer_auth(token)
            .header("anthropic-beta", OAUTH_BETA)
            .header("Accept", "application/json")
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: Value = resp.json().await.ok()?;
        v.get("account")?
            .get("email")?
            .as_str()
            .map(String::from)
    }

    /// Same endpoint Orca and `claude /usage` use: 5h session / weekly / per-model limits.
    pub async fn fetch_usage(&self, token: &str) -> Result<Value, String> {
        let resp = self
            .http
            .get(USAGE_URL)
            .bearer_auth(token)
            .header("anthropic-beta", OAUTH_BETA)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| format!("사용량 요청 실패: {e}"))?;
        let status = resp.status();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(TOKEN_EXPIRED.to_string());
        }
        if status.as_u16() == 429 {
            return Err("HTTP 429 — 요청 한도, 잠시 후 자동 재시도".to_string());
        }
        if !status.is_success() {
            return Err(format!("사용량 HTTP {status}"));
        }
        resp.json().await.map_err(|e| format!("사용량 파싱 실패: {e}"))
    }

    /// POST a JSON-RPC message to the artifacts MCP endpoint.
    /// Responses may be plain JSON or SSE ("data:" lines).
    async fn mcp_call(&self, token: &str, body: Value) -> Result<Option<Value>, String> {
        let resp = self
            .http
            .post(ARTIFACTS_MCP_URL)
            .bearer_auth(token)
            .header("anthropic-beta", OAUTH_BETA)
            .header("Accept", "application/json, text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("아티팩트 요청 실패: {e}"))?;
        let status = resp.status();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(TOKEN_EXPIRED.to_string());
        }
        if status.as_u16() == 404 {
            return Err("이 계정은 아티팩트 목록 API가 제공되지 않아요 (사용량만 표시)".to_string());
        }
        if !status.is_success() {
            return Err(format!("아티팩트 HTTP {status}"));
        }
        let is_sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.contains("text/event-stream"))
            .unwrap_or(false);
        let text = resp.text().await.map_err(|e| e.to_string())?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        if is_sse || !text.trim_start().starts_with('{') {
            let mut last = None;
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("data:") {
                    if let Ok(v) = serde_json::from_str::<Value>(rest.trim()) {
                        last = Some(v);
                    }
                }
            }
            Ok(last)
        } else {
            serde_json::from_str(&text)
                .map(Some)
                .map_err(|e| format!("MCP 파싱 실패: {e}"))
        }
    }

    /// List the account's artifacts (own + shared-with-me) through the same MCP
    /// server Claude Code uses. Tries a direct tools/call first (1 request);
    /// falls back to a full initialize handshake only if that fails.
    pub async fn list_artifacts(&self, token: &str) -> Result<Vec<Artifact>, String> {
        let mine = match self.try_list(token, false).await {
            Ok(v) => Ok(v),
            Err(e) if e.contains("429") || e.contains("제공되지") || e.contains("토큰 만료") => {
                Err(e)
            }
            Err(_) => {
                let _ = self
                    .mcp_call(
                        token,
                        json!({
                            "jsonrpc": "2.0", "id": 1, "method": "initialize",
                            "params": {
                                "protocolVersion": "2025-06-18",
                                "capabilities": {},
                                "clientInfo": {"name": "queenfish", "version": "0.1.0"}
                            }
                        }),
                    )
                    .await?;
                let _ = self
                    .mcp_call(
                        token,
                        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                    )
                    .await;
                self.try_list(token, false).await
            }
        }?;
        let mut all = mine;
        // shared-with-me: best effort — failure here never hides the user's own list
        if let Ok(shared) = self.try_list(token, true).await {
            let known: std::collections::HashSet<String> =
                all.iter().map(|a| a.slug.clone()).collect();
            all.extend(shared.into_iter().filter(|a| !known.contains(&a.slug)));
        }
        Ok(all)
    }

    async fn try_list(&self, token: &str, shared_only: bool) -> Result<Vec<Artifact>, String> {
        let mut args = json!({"limit": 500});
        if shared_only {
            args["shared_only"] = json!(true);
        }
        let res = self
            .mcp_call(
                token,
                json!({
                    "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": {"name": "list_artifacts", "arguments": args}
                }),
            )
            .await?
            .ok_or_else(|| "아티팩트 응답 없음".to_string())?;

        if let Some(err) = res.get("error") {
            return Err(format!("MCP 오류: {err}"));
        }
        let result = res.get("result").ok_or("MCP result 없음")?;
        if result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false) {
            return Err("아티팩트 목록 조회 실패".to_string());
        }
        let content = result
            .get("content")
            .and_then(|c| c.as_array())
            .ok_or("content 없음")?;
        for block in content {
            let Some(text) = block.get("text").and_then(|t| t.as_str()) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(text) else {
                continue;
            };
            let Some(items) = v.get("artifacts").and_then(|a| a.as_array()) else {
                continue;
            };
            let mut out = Vec::new();
            for it in items {
                let slug = it
                    .get("slug")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let url = it
                    .get("url")
                    .and_then(|x| x.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| format!("https://claude.ai/code/artifact/{slug}"));
                let default_rel = if shared_only { "shared" } else { "mine" };
                out.push(Artifact {
                    slug,
                    title: it
                        .get("title")
                        .and_then(|x| x.as_str())
                        .unwrap_or("(제목 없음)")
                        .to_string(),
                    description: it.get("description").and_then(|x| x.as_str()).map(String::from),
                    favicon: it.get("favicon").and_then(|x| x.as_str()).map(String::from),
                    updated_at: it.get("updated_at").and_then(|x| x.as_str()).map(String::from),
                    url,
                    rel: it
                        .get("rel")
                        .and_then(|x| x.as_str())
                        .unwrap_or(default_rel)
                        .to_string(),
                    type_title: it.get("type_title").and_then(|x| x.as_str()).map(String::from),
                });
            }
            return Ok(out);
        }
        Err("아티팩트 목록을 파싱하지 못했습니다".to_string())
    }

    /// Exchange a refresh token for a new access token. The caller must persist
    /// the result: the refresh token passed in is consumed by a successful call.
    pub async fn refresh_token(&self, refresh_token: &str) -> Option<RefreshedToken> {
        for url in [
            crate::oauth::TOKEN_URL,
            "https://console.anthropic.com/v1/oauth/token",
        ] {
            let body = json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
                "client_id": CLIENT_ID
            });
            let Ok(resp) = self.http.post(url).json(&body).send().await else {
                continue;
            };
            if !resp.status().is_success() {
                continue;
            }
            let Ok(v) = resp.json::<Value>().await else {
                continue;
            };
            if let Some(tok) = v.get("access_token").and_then(|x| x.as_str()) {
                return Some(RefreshedToken {
                    access_token: tok.to_string(),
                    refresh_token: v
                        .get("refresh_token")
                        .and_then(|x| x.as_str())
                        .map(String::from),
                    expires_in: v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(28800),
                });
            }
        }
        None
    }
}
