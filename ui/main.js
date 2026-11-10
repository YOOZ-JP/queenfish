const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $search = document.getElementById("search");
const $usage = document.getElementById("usage");
const $chips = document.getElementById("chips");
const $list = document.getElementById("list");
const $preview = document.getElementById("preview");
const $status = document.getElementById("status");
const $refresh = document.getElementById("refresh");
const $manage = document.getElementById("manage");

let data = { accounts: [], artifacts: [], last_refresh: null };
let q = "";
let accountFilter = null;
let relFilter = null; // null | "mine" | "shared"
let sel = 0;
let visibleItems = [];

/* ---------- formatting ---------- */

function relTime(iso) {
  if (!iso) return "";
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return "";
  const diff = Date.now() - t;
  const m = Math.floor(diff / 60000);
  if (m < 1) return "방금";
  if (m < 60) return `${m}분 전`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}시간 전`;
  const d = Math.floor(h / 24);
  if (d < 7) return `${d}일 전`;
  const dt = new Date(t);
  return `${dt.getMonth() + 1}/${dt.getDate()}`;
}

function resetTime(iso) {
  if (!iso) return "";
  const dt = new Date(iso);
  if (Number.isNaN(dt.getTime())) return "";
  const days = ["일", "월", "화", "수", "목", "금", "토"];
  const hh = String(dt.getHours()).padStart(2, "0");
  const mm = String(dt.getMinutes()).padStart(2, "0");
  return `${days[dt.getDay()]} ${hh}:${mm} 초기화`;
}

function pctClass(p) {
  if (p >= 85) return "hot";
  if (p >= 60) return "warn";
  return "";
}

function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  })[c]);
}

/* ---------- usage ---------- */

function metricsFor(acc) {
  const u = acc.usage || {};
  const m = [];
  const direct = (obj, label) => {
    if (obj && typeof obj.utilization === "number") {
      m.push({ label, pct: obj.utilization, resets: obj.resets_at });
    }
  };
  direct(u.five_hour, "5시간");
  direct(u.seven_day, "주간");
  if (Array.isArray(u.limits)) {
    for (const l of u.limits) {
      if (l && l.kind === "weekly_scoped" && typeof l.percent === "number") {
        const name = l.scope?.model?.display_name || "모델";
        m.push({ label: name, pct: l.percent, resets: l.resets_at });
      }
    }
  }
  return m;
}

function renderUsage() {
  const rows = data.accounts
    .map((acc) => {
      const metrics = metricsFor(acc);
      const usageStale = acc.error && (acc.error.includes("429") || acc.error.includes("만료"));
      const staleMark = acc.error && metrics.length > 0
        ? usageStale
          ? `<span class="metric" title="${esc(acc.error)} (이전 데이터 표시 중)">⏳</span>`
          : `<span class="metric" title="${esc(acc.error)}">ⓘ</span>`
        : "";
      const resetSrc =
        metrics.find((m) => m.label === "주간" && m.resets) || metrics.find((m) => m.resets);
      const resetHtml = resetSrc
        ? `<span class="metric reset" title="한도 초기화 시각">${esc(resetTime(resetSrc.resets))}</span>`
        : "";
      const inner = acc.error && metrics.length === 0
        ? `<span class="usage-err" title="${esc(acc.error)}">${esc(acc.error)}</span>`
        : staleMark + metrics
            .map(
              (m) => `
        <span class="metric" title="${esc(m.label)} ${m.pct.toFixed(0)}% · ${esc(resetTime(m.resets))}">
          ${esc(m.label)}
          <span class="bar"><span class="bar-fill ${pctClass(m.pct)}" style="width:${Math.min(100, Math.max(0, m.pct))}%"></span></span>
          ${m.pct.toFixed(0)}%
        </span>`
            )
            .join("") + resetHtml;
      return `<div class="usage-row">
        <span class="usage-label" title="${esc(acc.label)} (${esc(acc.source)})">${esc(acc.label)}</span>
        <span class="usage-metrics">${inner}</span>
      </div>`;
    })
    .join("");
  $usage.innerHTML = rows;
}

/* ---------- chips ---------- */

function renderChips() {
  let html = "";
  const hasShared = data.artifacts.some((a) => a.rel === "shared");
  if (hasShared) {
    html += [
      { v: "mine", label: "내 항목" },
      { v: "shared", label: "공유받음" },
    ]
      .map(
        (c) =>
          `<span class="chip rel-chip ${relFilter === c.v ? "active" : ""}" data-rel="${c.v}">${c.label}</span>`
      )
      .join("");
  }
  if (data.accounts.length >= 2) {
    const chips = [{ id: null, label: "전체" }, ...data.accounts.map((a) => ({ id: a.id, label: a.label }))];
    html += chips
      .map(
        (c) =>
          `<span class="chip acct-chip ${accountFilter === c.id ? "active" : ""}" data-id="${esc(c.id ?? "")}">${esc(c.label)}</span>`
      )
      .join("");
  }
  $chips.innerHTML = html;
  for (const node of $chips.querySelectorAll(".acct-chip")) {
    node.addEventListener("click", () => {
      accountFilter = node.dataset.id || null;
      sel = 0;
      render();
    });
  }
  for (const node of $chips.querySelectorAll(".rel-chip")) {
    node.addEventListener("click", () => {
      relFilter = relFilter === node.dataset.rel ? null : node.dataset.rel;
      sel = 0;
      render();
    });
  }
}

/* ---------- list ---------- */

const ARTIFACT_URL_RE = /https:\/\/(claude\.ai|claude\.com)\/\S*(artifact|frame)\/[A-Za-z0-9_-]+/g;

function extractArtifactUrls(s) {
  return [...new Set(s.match(ARTIFACT_URL_RE) || [])];
}

function isArtifactUrl(s) {
  return extractArtifactUrls(s).length > 0;
}

function filteredItems() {
  const needle = q.trim().toLowerCase();
  return data.artifacts.filter((a) => {
    if (accountFilter && a.account_id !== accountFilter) return false;
    if (relFilter && a.rel !== relFilter) return false;
    if (!needle) return true;
    const hay = `${a.title} ${a.description || ""} ${a.account_label}`.toLowerCase();
    return needle.split(/\s+/).every((w) => hay.includes(w));
  });
}

function renderList() {
  visibleItems = filteredItems();
  if (sel >= visibleItems.length) sel = Math.max(0, visibleItems.length - 1);
  if (isArtifactUrl(q)) {
    const n = extractArtifactUrls(q).length;
    $list.innerHTML = `<div class="empty">🔗 아티팩트 링크 ${n}개 감지<br/><b>↵ Enter</b>를 누르면 목록에 추가됩니다</div>`;
    $preview.hidden = true;
    return;
  }
  if (visibleItems.length === 0) {
    $list.innerHTML = `<div class="empty">${
      data.artifacts.length === 0 ? "아티팩트가 없습니다 · ↻로 새로고침" : "검색 결과 없음"
    }</div>`;
    $preview.hidden = true;
    return;
  }
  $list.innerHTML = visibleItems
    .map(
      (a, i) => `
    <div class="row ${i === sel ? "selected" : ""}" data-i="${i}">
      <span class="icon">${esc(a.favicon || "✦")}</span>
      <span class="body">
        <div class="title" title="${esc(a.description || a.title)}">${esc(a.title)}</div>
        <div class="sub"><span class="acct">${esc(a.account_label)}</span>${a.rel === "shared" ? '<span class="shared-badge">공유받음</span>' : ""}</div>
      </span>
      ${a.account_id === "pinned" ? `<button class="pin-x" data-url="${esc(a.url)}" title="고정 해제">✕</button>` : ""}
      <span class="time">${relTime(a.updated_at)}</span>
    </div>`
    )
    .join("");
  for (const node of $list.querySelectorAll(".row")) {
    const i = Number(node.dataset.i);
    node.addEventListener("click", () => openItem(visibleItems[i]));
    node.addEventListener("mousemove", () => {
      if (sel !== i) {
        sel = i;
        highlight();
      }
    });
  }
  for (const node of $list.querySelectorAll(".pin-x")) {
    node.addEventListener("click", async (e) => {
      e.stopPropagation();
      try {
        await invoke("remove_pinned_link", { url: node.dataset.url });
        $status.textContent = "링크 제거됨";
      } catch (err) {
        $status.textContent = String(err);
      }
    });
  }
  renderPreview();
}

function highlight() {
  const rows = $list.querySelectorAll(".row");
  rows.forEach((r, i) => r.classList.toggle("selected", i === sel));
  const node = rows[sel];
  if (node) node.scrollIntoView({ block: "nearest" });
  renderPreview();
}

function fullTime(iso) {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleString("ko-KR", {
    year: "numeric", month: "numeric", day: "numeric",
    hour: "2-digit", minute: "2-digit",
  });
}

function renderPreview() {
  const a = visibleItems[sel];
  if (!a) {
    $preview.hidden = true;
    return;
  }
  const metaBits = [
    a.account_label,
    a.type_title,
    a.rel === "shared" ? "공유받음" : null,
    fullTime(a.updated_at),
  ].filter(Boolean);
  $preview.hidden = false;
  $preview.innerHTML = `
    <div class="pv-head">
      <span class="pv-icon">${esc(a.favicon || "✦")}</span>
      <span class="pv-title">${esc(a.title)}</span>
    </div>
    ${a.description ? `<div class="pv-desc">${esc(a.description)}</div>` : ""}
    <div class="pv-meta">${metaBits.map((m) => `<span>${esc(m)}</span>`).join("<span class=\"pv-dot\">·</span>")}</div>
    <div class="pv-url" title="${esc(a.url)}">${esc(a.url)}</div>`;
}

function renderStatus() {
  const n = filteredItems().length;
  const t = data.last_refresh ? relTime(new Date(data.last_refresh).toISOString()) : "-";
  $status.textContent = `${n}개 · 갱신 ${t}`;
}

function render() {
  renderUsage();
  renderChips();
  renderList();
  renderStatus();
}

/* ---------- actions ---------- */

async function openItem(item) {
  if (!item) return;
  try {
    await invoke("open_artifact", { url: item.url });
    await invoke("hide_panel");
  } catch (e) {
    $status.textContent = String(e);
  }
}

function copySelected() {
  const item = visibleItems[sel];
  if (!item) return;
  const ta = document.createElement("textarea");
  ta.value = item.url;
  document.body.appendChild(ta);
  ta.select();
  document.execCommand("copy");
  ta.remove();
  $status.textContent = "링크 복사됨";
}

async function reload() {
  try {
    data = await invoke("get_data");
  } catch (e) {
    $status.textContent = String(e);
    return;
  }
  render();
}

/* ---------- events ---------- */

$search.addEventListener("input", () => {
  q = $search.value;
  sel = 0;
  renderList();
  renderStatus();
});

window.addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown") {
    e.preventDefault();
    sel = Math.min(visibleItems.length - 1, sel + 1);
    highlight();
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    sel = Math.max(0, sel - 1);
    highlight();
  } else if (e.key === "Enter") {
    e.preventDefault();
    if (isArtifactUrl(q)) {
      const urls = extractArtifactUrls(q);
      $status.textContent = `링크 ${urls.length}개 추가 중… (제목 가져오는 중)`;
      (async () => {
        let ok = 0;
        let lastTitle = "";
        for (const url of urls) {
          try {
            lastTitle = await invoke("add_pinned_link", { url, addedAt: new Date().toISOString() });
            ok++;
          } catch (err) {
            $status.textContent = String(err);
          }
        }
        if (ok > 0) {
          $status.textContent = ok === 1 ? `추가됨: ${lastTitle}` : `${ok}개 링크 추가됨`;
          $search.value = "";
          q = "";
        }
      })();
      return;
    }
    openItem(visibleItems[sel]);
  } else if (e.key === "Escape") {
    e.preventDefault();
    invoke("hide_panel");
  } else if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "c") {
    if (!window.getSelection()?.toString()) {
      e.preventDefault();
      copySelected();
    }
  } else if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "r") {
    e.preventDefault();
    triggerRefresh();
  } else if ((e.metaKey || e.ctrlKey) && e.key === ",") {
    e.preventDefault();
    invoke("open_settings");
  }
});

function triggerRefresh() {
  $refresh.classList.add("spinning");
  invoke("refresh_now");
  setTimeout(() => $refresh.classList.remove("spinning"), 3000);
}

$refresh.addEventListener("click", triggerRefresh);

$manage.addEventListener("click", () => {
  invoke("open_settings").catch((e) => {
    $status.textContent = String(e);
  });
});

listen("login-done", (ev) => {
  const p = String(ev.payload || "");
  if (p.startsWith("ok:")) {
    $status.textContent = `계정 추가됨: ${p.slice(3)}`;
  } else {
    $status.textContent = `계정 추가 실패 — ${p.replace(/^err:/, "")}`;
  }
});

listen("data-updated", () => {
  $refresh.classList.remove("spinning");
  reload();
});
listen("panel-shown", () => {
  $search.focus();
  $search.select();
  reload();
});

reload();
$search.focus();
