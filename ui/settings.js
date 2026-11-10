const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $accounts = document.getElementById("accounts");
const $hidden = document.getElementById("hidden-section");
const $links = document.getElementById("links");
const $status = document.getElementById("status-line");

let data = { accounts: [], artifacts: [], hidden: [] };
let pending = null; // "acct:<id>" | "link:<url>" awaiting confirm

function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  })[c]);
}

function sourceLabel(src) {
  if (src.startsWith("queenfish")) return ["Queenfish", "Queenfish에서 로그인해 추가한 계정"];
  if (src.includes("file:") && src.includes("orca"))
    return ["Orca", "Orca 프로필 폴더의 자격증명 파일"];
  if (src.startsWith("keychain-"))
    return [
      "Claude Code 프로필",
      "별도 프로필용 Keychain 항목 — Orca 등이 계정 전환을 위해 만든 Claude Code 프로필의 토큰",
    ];
  if (src.startsWith("keychain")) return ["Claude Code", "Claude Code 기본 Keychain 항목"];
  if (src.startsWith("file:")) return ["파일", src];
  return [src, src];
}

function armConfirm(key, cb) {
  if (pending !== key) {
    pending = key;
    render();
    setTimeout(() => {
      if (pending === key) {
        pending = null;
        render();
      }
    }, 3000);
    return;
  }
  pending = null;
  cb();
}

function render() {
  // accounts
  $accounts.innerHTML =
    data.accounts
      .map((a) => {
        const confirming = pending === `acct:${a.id}`;
        const verb = a.removable ? "삭제" : "숨김";
        const nArts = data.artifacts.filter((x) => x.account_id === a.id).length;
        const sub = a.error
          ? `<div class="s-sub err" title="${esc(a.error)}">${esc(a.error)}</div>`
          : `<div class="s-sub">정상 · 아티팩트 ${nArts}개${a.usage ? " · 사용량 조회됨" : ""}</div>`;
        const [srcName, srcDesc] = sourceLabel(a.source);
        return `<div class="s-row">
          <div class="s-main">
            <div class="s-title">${esc(a.label)}<span class="tag" title="${esc(srcDesc)}">${esc(srcName)}</span></div>
            ${sub}
          </div>
          <button class="s-btn danger ${confirming ? "confirm" : ""}" data-act="acct" data-id="${esc(a.id)}" data-removable="${a.removable}"
            title="${a.removable ? "Queenfish Keychain에서 삭제" : "목록에서 숨김 (로그아웃은 Claude Code/Orca에서)"}">
            ${confirming ? verb + "?" : verb}
          </button>
        </div>`;
      })
      .join("") || `<div class="s-empty">발견된 계정이 없습니다</div>`;

  // hidden accounts
  $hidden.innerHTML = (data.hidden || []).length
    ? `<p class="hint">숨긴 계정: ${(data.hidden || [])
        .map(
          (h) =>
            `<button class="s-btn" data-act="unhide" data-id="${esc(h)}">${esc(h)} 복원</button>`
        )
        .join(" ")}</p>`
    : "";

  // pinned links
  const links = data.artifacts.filter((a) => a.account_id === "pinned");
  $links.innerHTML =
    links
      .map((l) => {
        const confirming = pending === `link:${l.url}`;
        return `<div class="s-row">
          <div class="s-main">
            <div class="s-title">${esc(l.favicon || "🔗")} ${esc(l.title)}</div>
            <div class="s-sub">${esc(l.url)}</div>
          </div>
          <button class="s-btn danger ${confirming ? "confirm" : ""}" data-act="link" data-url="${esc(l.url)}">
            ${confirming ? "제거?" : "제거"}
          </button>
        </div>`;
      })
      .join("") || `<div class="s-empty">고정된 링크가 없습니다</div>`;

  // handlers
  for (const btn of document.querySelectorAll("[data-act]")) {
    btn.addEventListener("click", () => {
      const act = btn.dataset.act;
      if (act === "acct") {
        const id = btn.dataset.id;
        const removable = btn.dataset.removable === "true";
        armConfirm(`acct:${id}`, async () => {
          try {
            const outcome = await invoke("remove_account", { id, removable });
            $status.textContent = outcome === "deleted" ? `${id} 삭제됨` : `${id} 숨김`;
          } catch (e) {
            $status.textContent = String(e);
          }
        });
      } else if (act === "unhide") {
        invoke("unhide_account", { id: btn.dataset.id }).catch((e) => {
          $status.textContent = String(e);
        });
      } else if (act === "link") {
        const url = btn.dataset.url;
        armConfirm(`link:${url}`, async () => {
          try {
            await invoke("remove_pinned_link", { url });
            $status.textContent = "링크 제거됨";
          } catch (e) {
            $status.textContent = String(e);
          }
        });
      }
    });
  }
}

async function load() {
  try {
    data = await invoke("get_data");
  } catch (e) {
    $status.textContent = String(e);
    return;
  }
  render();
}

document.getElementById("add").addEventListener("click", async () => {
  try {
    await invoke("add_account");
    $status.textContent = "브라우저에서 로그인을 완료하세요… (5분 내)";
  } catch (e) {
    $status.textContent = String(e);
  }
});

document.getElementById("s-refresh").addEventListener("click", () => {
  invoke("refresh_now");
  $status.textContent = "새로고침 중…";
});

listen("data-updated", load);
listen("accounts-changed", load);
listen("login-done", (ev) => {
  const p = String(ev.payload || "");
  $status.textContent = p.startsWith("ok:")
    ? `계정 추가됨: ${p.slice(3)}`
    : `계정 추가 실패 — ${p.replace(/^err:/, "")}`;
});

load();
