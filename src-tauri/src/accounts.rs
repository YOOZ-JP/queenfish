use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Clone)]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>, // epoch millis
}

/// One OAuth credential found on this machine, before account attribution.
#[derive(Clone)]
pub struct CredSource {
    pub cred: Credentials,
    pub origin: String, // e.g. "keychain", "keychain-febc1163", "file:/path"
}

fn parse_credentials(bytes: &[u8]) -> Option<Credentials> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let o = v.get("claudeAiOauth").unwrap_or(&v);
    let access = o.get("accessToken")?.as_str()?.to_string();
    Some(Credentials {
        access_token: access,
        refresh_token: o
            .get("refreshToken")
            .and_then(|x| x.as_str())
            .map(String::from),
        expires_at: o.get("expiresAt").and_then(|x| x.as_u64()),
    })
}

/// Homes that may contain a `.credentials.json`: default home, Orca profiles, custom homes.
fn candidate_homes(app: &tauri::AppHandle) -> Vec<PathBuf> {
    use tauri::Manager;
    let mut homes = Vec::new();
    if let Some(home) = dirs::home_dir() {
        homes.push(home);
    }
    if let Some(data) = dirs::data_dir() {
        let profiles = data.join("orca").join("claude-profiles");
        if let Ok(rd) = std::fs::read_dir(&profiles) {
            for entry in rd.flatten() {
                let home = entry.path().join("home");
                if home.is_dir() {
                    homes.push(home);
                }
            }
        }
    }
    if let Ok(dir) = app.path().app_config_dir() {
        if let Ok(bytes) = std::fs::read(dir.join("config.json")) {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if let Some(arr) = v.get("custom_homes").and_then(|x| x.as_array()) {
                    for p in arr.iter().filter_map(|x| x.as_str()) {
                        homes.push(PathBuf::from(p));
                    }
                }
            }
        }
    }
    homes
}

/// Enumerate keychain generic-password services for Claude Code and Queenfish items.
/// `security dump-keychain` lists metadata only (no secrets, no ACL prompts).
async fn keychain_services() -> Vec<String> {
    let Ok(out) = tokio::process::Command::new("security")
        .arg("dump-keychain")
        .output()
        .await
    else {
        return vec!["Claude Code-credentials".to_string()];
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut services: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some(idx) = line.find("\"svce\"<blob>=\"") else {
            continue;
        };
        let rest = &line[idx + 14..];
        let Some(end) = rest.find('"') else { continue };
        let svc = &rest[..end];
        let interesting = svc.starts_with("Claude Code-credentials")
            || svc.starts_with(crate::oauth::KEYCHAIN_PREFIX);
        if interesting && !services.iter().any(|s| s == svc) {
            services.push(svc.to_string());
        }
    }
    if services.is_empty() {
        services.push("Claude Code-credentials".to_string());
    }
    services
}

/// Persistent token→email map so we don't hit the profile endpoint every refresh.
pub fn token_fingerprint(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(token.as_bytes());
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn email_map_path(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    use tauri::Manager;
    let dir = app.path().app_data_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("emails.json"))
}

pub fn load_email_map(app: &tauri::AppHandle) -> std::collections::HashMap<String, String> {
    email_map_path(app)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_email_map(app: &tauri::AppHandle, map: &std::collections::HashMap<String, String>) {
    if let Some(p) = email_map_path(app) {
        let _ = std::fs::write(p, serde_json::to_vec(map).unwrap_or_default());
    }
}

/// App config: {"custom_homes": [...], "hidden_accounts": ["email", ...]}
pub fn read_config(app: &tauri::AppHandle) -> serde_json::Value {
    use tauri::Manager;
    if let Ok(dir) = app.path().app_config_dir() {
        if let Ok(bytes) = std::fs::read(dir.join("config.json")) {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                return v;
            }
        }
    }
    serde_json::json!({})
}

pub fn write_config(app: &tauri::AppHandle, v: &serde_json::Value) -> Result<(), String> {
    use tauri::Manager;
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join("config.json"),
        serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

pub fn hidden_accounts(app: &tauri::AppHandle) -> Vec<String> {
    read_config(app)
        .get("hidden_accounts")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Manually pinned shared-artifact links: [{"url", "title", "added_at"}]
pub fn pinned_links(app: &tauri::AppHandle) -> Vec<serde_json::Value> {
    read_config(app)
        .get("pinned_links")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default()
}

pub fn add_pinned_link(
    app: &tauri::AppHandle,
    url: &str,
    title: &str,
    added_at: &str,
) -> Result<(), String> {
    let mut cfg = read_config(app);
    let mut list = pinned_links(app);
    list.retain(|l| l.get("url").and_then(|u| u.as_str()) != Some(url));
    list.push(serde_json::json!({"url": url, "title": title, "added_at": added_at}));
    cfg["pinned_links"] = serde_json::json!(list);
    write_config(app, &cfg)
}

pub fn remove_pinned_link(app: &tauri::AppHandle, url: &str) -> Result<(), String> {
    let mut cfg = read_config(app);
    let mut list = pinned_links(app);
    list.retain(|l| l.get("url").and_then(|u| u.as_str()) != Some(url));
    cfg["pinned_links"] = serde_json::json!(list);
    write_config(app, &cfg)
}

pub fn set_hidden(app: &tauri::AppHandle, email: &str, hidden: bool) -> Result<(), String> {
    let mut cfg = read_config(app);
    let mut list = hidden_accounts(app);
    if hidden {
        if !list.iter().any(|e| e == email) {
            list.push(email.to_string());
        }
    } else {
        list.retain(|e| e != email);
    }
    cfg["hidden_accounts"] = serde_json::json!(list);
    write_config(app, &cfg)
}

async fn read_keychain(svc: &str) -> Option<Credentials> {
    let out = tokio::process::Command::new("security")
        .args(["find-generic-password", "-s", svc, "-w"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_credentials(&out.stdout)
}

/// All distinct OAuth credentials on this machine (deduped by access token).
pub async fn discover_credentials(app: &tauri::AppHandle) -> Vec<CredSource> {
    let mut out: Vec<CredSource> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for home in candidate_homes(app) {
        for rel in [".claude/.credentials.json", ".credentials.json"] {
            let p = home.join(rel);
            let Ok(bytes) = std::fs::read(&p) else { continue };
            let Some(c) = parse_credentials(&bytes) else { continue };
            if seen.insert(c.access_token.clone()) {
                out.push(CredSource {
                    cred: c,
                    origin: format!("file:{}", home.display()),
                });
            }
        }
    }

    for svc in keychain_services().await {
        let Some(c) = read_keychain(&svc).await else {
            continue;
        };
        if seen.insert(c.access_token.clone()) {
            let origin = if let Some(email) = svc.strip_prefix(crate::oauth::KEYCHAIN_PREFIX) {
                format!("queenfish:{email}")
            } else {
                let suffix = svc.strip_prefix("Claude Code-credentials").unwrap_or("");
                if suffix.is_empty() {
                    "keychain".to_string()
                } else {
                    format!("keychain{suffix}")
                }
            };
            out.push(CredSource { cred: c, origin });
        }
    }

    out
}
