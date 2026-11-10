use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, State,
};
use tauri_plugin_global_shortcut::ShortcutState;
use tauri_plugin_opener::OpenerExt;

mod accounts;
mod api;
mod oauth;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Snapshot {
    pub accounts: Vec<AccountView>,
    pub artifacts: Vec<ArtifactView>,
    pub last_refresh: Option<u64>,
    #[serde(default)]
    pub hidden: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct AccountView {
    pub id: String,
    pub label: String,
    pub source: String,
    pub usage: Option<serde_json::Value>,
    pub error: Option<String>,
    #[serde(default)]
    pub removable: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ArtifactView {
    pub slug: String,
    pub title: String,
    pub description: Option<String>,
    pub favicon: Option<String>,
    pub url: String,
    pub updated_at: Option<String>,
    pub account_id: String,
    pub account_label: String,
    #[serde(default = "default_rel")]
    pub rel: String,
    #[serde(default)]
    pub type_title: Option<String>,
}

fn default_rel() -> String {
    "mine".to_string()
}

#[derive(Default)]
pub struct AppState {
    snapshot: Mutex<Snapshot>,
    last_refresh_started: Mutex<Option<Instant>>,
    cooldown_until: Mutex<Option<Instant>>,
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn cache_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("cache.json"))
}

fn toggle_panel(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
    } else {
        let _ = win.show();
        let _ = win.set_focus();
        let _ = app.emit("panel-shown", ());
        maybe_refresh(app, false);
    }
}

pub fn maybe_refresh(app: &AppHandle, force: bool) {
    let state: State<AppState> = app.state();
    {
        // While rate-limited, skip automatic refreshes entirely.
        let cooldown = state.cooldown_until.lock().unwrap();
        if !force {
            if let Some(until) = *cooldown {
                if Instant::now() < until {
                    return;
                }
            }
        }
    }
    {
        let mut last = state.last_refresh_started.lock().unwrap();
        if !force {
            if let Some(t) = *last {
                if t.elapsed() < Duration::from_secs(60) {
                    return;
                }
            }
        }
        *last = Some(Instant::now());
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        refresh_all(app).await;
    });
}

async fn refresh_all(app: AppHandle) {
    let client = api::Client::new();
    let sources = accounts::discover_credentials(&app).await;
    let hidden = accounts::hidden_accounts(&app);
    let prev = app.state::<AppState>().snapshot.lock().unwrap().clone();
    let mut email_map = accounts::load_email_map(&app);
    let mut email_map_dirty = false;
    let mut rate_limited = false;
    let mut views = Vec::new();
    let mut arts: Vec<ArtifactView> = Vec::new();
    let mut seen_emails: std::collections::HashSet<String> = std::collections::HashSet::new();

    for src in &sources {
        let mut token = src.cred.access_token.clone();
        if let (Some(exp), Some(rt)) = (src.cred.expires_at, src.cred.refresh_token.as_ref()) {
            if now_ms() + 60_000 > exp {
                if let Some(new_tok) = client.refresh_token(rt).await {
                    token = new_tok;
                }
            }
        }

        // Identity: cached token→email mapping first; profile endpoint only on miss.
        let fp = accounts::token_fingerprint(&token);
        let mut email = email_map.get(&fp).cloned();
        if email.is_none() {
            email = client.fetch_profile_email(&token).await;
            if let Some(e) = &email {
                email_map.insert(fp, e.clone());
                email_map_dirty = true;
            }
        }
        // Last resort: reuse the previous label for this credential origin.
        if email.is_none() {
            email = prev
                .accounts
                .iter()
                .find(|a| a.source == src.origin)
                .map(|a| a.id.clone());
        }
        if let Some(e) = &email {
            if !seen_emails.insert(e.clone()) {
                continue; // same account reachable via two credential copies
            }
        }
        let label = email.clone().unwrap_or_else(|| src.origin.clone());
        let id = label.clone();
        if hidden.iter().any(|h| h == &id) {
            continue;
        }
        let prev_view = prev.accounts.iter().find(|a| a.id == id);

        let mut error: Option<String> = None;
        let mut usage: Option<serde_json::Value> = None;
        match client.fetch_usage(&token).await {
            Ok(u) => usage = Some(u),
            Err(e) => {
                if e.contains("429") {
                    rate_limited = true;
                    // keep showing the last known usage instead of blanking it
                    usage = prev_view.and_then(|v| v.usage.clone());
                }
                error = Some(e);
            }
        }
        match client.list_artifacts(&token).await {
            Ok(list) => {
                arts.extend(list.into_iter().map(|a| ArtifactView {
                    slug: a.slug,
                    title: a.title,
                    description: a.description,
                    favicon: a.favicon,
                    url: a.url,
                    updated_at: a.updated_at,
                    account_id: id.clone(),
                    account_label: label.clone(),
                    rel: a.rel,
                    type_title: a.type_title,
                }));
            }
            Err(e) => {
                // keep the previous artifact list for this account on any failure
                arts.extend(
                    prev.artifacts
                        .iter()
                        .filter(|a| a.account_id == id)
                        .cloned(),
                );
                if e.contains("429") {
                    rate_limited = true;
                }
                if error.is_none() {
                    error = Some(e);
                }
            }
        }
        views.push(AccountView {
            id,
            label,
            source: src.origin.clone(),
            usage,
            error,
            removable: src.origin.starts_with("queenfish"),
        });
    }

    if email_map_dirty {
        accounts::save_email_map(&app, &email_map);
    }
    if rate_limited {
        *app.state::<AppState>().cooldown_until.lock().unwrap() =
            Some(Instant::now() + Duration::from_secs(600));
    }

    // manually pinned shared links (cross-account shares aren't listable via API yet)
    for l in accounts::pinned_links(&app) {
        let Some(url) = l.get("url").and_then(|u| u.as_str()) else {
            continue;
        };
        arts.push(ArtifactView {
            slug: url.to_string(),
            title: l
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("공유 아티팩트")
                .to_string(),
            description: None,
            favicon: Some("🔗".to_string()),
            url: url.to_string(),
            updated_at: l.get("added_at").and_then(|t| t.as_str()).map(String::from),
            account_id: "pinned".to_string(),
            account_label: "링크".to_string(),
            rel: "shared".to_string(),
            type_title: None,
        });
    }

    arts.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let snap = Snapshot {
        accounts: views,
        artifacts: arts,
        last_refresh: Some(now_ms()),
        hidden,
    };
    if let Some(p) = cache_path(&app) {
        let _ = std::fs::write(p, serde_json::to_vec(&snap).unwrap_or_default());
    }
    let state: State<AppState> = app.state();
    *state.snapshot.lock().unwrap() = snap;
    let _ = app.emit("data-updated", ());
}

#[tauri::command]
fn get_data(state: State<AppState>) -> Snapshot {
    state.snapshot.lock().unwrap().clone()
}

#[tauri::command]
fn refresh_now(app: AppHandle) {
    maybe_refresh(&app, true);
}

#[tauri::command]
fn hide_panel(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

#[tauri::command]
async fn add_account(app: AppHandle) -> Result<(), String> {
    oauth::start_login(app).await
}

/// Queenfish-managed accounts are actually deleted; Claude Code/Orca accounts are only hidden.
#[tauri::command]
fn remove_account(app: AppHandle, id: String, removable: bool) -> Result<String, String> {
    let outcome = if removable {
        oauth::delete_account(&id)?;
        "deleted"
    } else {
        accounts::set_hidden(&app, &id, true)?;
        "hidden"
    };
    maybe_refresh(&app, true);
    Ok(outcome.to_string())
}

fn is_artifact_url(url: &str) -> bool {
    (url.starts_with("https://claude.ai/") || url.starts_with("https://claude.com/"))
        && (url.contains("/artifact/") || url.contains("/frame/"))
}

#[tauri::command]
async fn add_pinned_link(app: AppHandle, url: String, added_at: String) -> Result<String, String> {
    if !is_artifact_url(&url) {
        return Err("아티팩트 링크가 아닙니다".to_string());
    }
    // best-effort title from the page's <title> tag
    let mut title = "공유 아티팩트".to_string();
    if let Ok(resp) = reqwest::Client::new()
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)")
        .timeout(std::time::Duration::from_secs(6))
        .send()
        .await
    {
        if let Ok(html) = resp.text().await {
            if let Some(start) = html.find("<title>") {
                if let Some(end) = html[start + 7..].find("</title>") {
                    let t = html[start + 7..start + 7 + end]
                        .replace("&amp;", "&")
                        .replace("&#39;", "'")
                        .replace("&quot;", "\"");
                    let t = t
                        .trim_end_matches(" - Claude")
                        .trim_end_matches(" | Claude")
                        .trim();
                    if !t.is_empty() && t != "Claude" {
                        title = t.to_string();
                    }
                }
            }
        }
    }
    accounts::add_pinned_link(&app, &url, &title, &added_at)?;
    maybe_refresh(&app, true);
    Ok(title)
}

#[tauri::command]
fn remove_pinned_link(app: AppHandle, url: String) -> Result<(), String> {
    accounts::remove_pinned_link(&app, &url)?;
    maybe_refresh(&app, true);
    Ok(())
}

#[tauri::command]
fn open_settings(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    tauri::WebviewWindowBuilder::new(
        &app,
        "settings",
        tauri::WebviewUrl::App("settings.html".into()),
    )
    .title("Queenfish 설정")
    .inner_size(500.0, 620.0)
    .resizable(false)
    .build()
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn unhide_account(app: AppHandle, id: String) -> Result<(), String> {
    accounts::set_hidden(&app, &id, false)?;
    maybe_refresh(&app, true);
    Ok(())
}

#[tauri::command]
fn open_artifact(app: AppHandle, url: String) -> Result<(), String> {
    if !url.starts_with("https://claude.ai/") && !url.starts_with("https://claude.com/") {
        return Err("허용되지 않은 URL".to_string());
    }
    app.opener()
        .open_url(url, None::<String>)
        .map_err(|e| e.to_string())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_shortcuts(["CmdOrCtrl+Shift+A"])
                .expect("단축키 등록 실패")
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_panel(app);
                    }
                })
                .build(),
        )
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_data,
            refresh_now,
            hide_panel,
            open_artifact,
            add_account,
            remove_account,
            unhide_account,
            add_pinned_link,
            remove_pinned_link,
            open_settings
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // load cached snapshot so the panel is instantly useful
            if let Some(p) = cache_path(app.handle()) {
                if let Ok(bytes) = std::fs::read(&p) {
                    if let Ok(snap) = serde_json::from_slice::<Snapshot>(&bytes) {
                        *app.state::<AppState>().snapshot.lock().unwrap() = snap;
                    }
                }
            }

            let toggle_i = MenuItem::with_id(app, "toggle", "패널 열기/닫기  ⌘⇧A", true, None::<&str>)?;
            let refresh_i = MenuItem::with_id(app, "refresh", "지금 새로고침", true, None::<&str>)?;
            let add_acct_i =
                MenuItem::with_id(app, "add-account", "계정 추가 (claude.ai 로그인)…", true, None::<&str>)?;
            let manage_i = MenuItem::with_id(app, "manage", "계정 관리…", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "Queenfish 종료", true, None::<&str>)?;
            let sep1 = PredefinedMenuItem::separator(app)?;
            let sep2 = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(
                app,
                &[&toggle_i, &refresh_i, &sep1, &add_acct_i, &manage_i, &sep2, &quit_i],
            )?;
            TrayIconBuilder::with_id("tray")
                .title("✦")
                .tooltip("Queenfish — Claude 아티팩트 & 사용량")
                .menu(&menu)
                .on_menu_event(|app, e| match e.id().as_ref() {
                    "toggle" => toggle_panel(app),
                    "refresh" => maybe_refresh(app, true),
                    "add-account" => {
                        let app2 = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = oauth::start_login(app2).await;
                        });
                    }
                    "manage" => {
                        let _ = open_settings(app.clone());
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    maybe_refresh(&handle, true);
                    tokio::time::sleep(Duration::from_secs(900)).await;
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            use tauri::WindowEvent;
            if window.label() != "main" {
                return;
            }
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                WindowEvent::Focused(false) => {
                    let _ = window.hide();
                }
                _ => {}
            }
        })
        .run(tauri::generate_context!())
        .expect("queenfish 실행 실패");
}
