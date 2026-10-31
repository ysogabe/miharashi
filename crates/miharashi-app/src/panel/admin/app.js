"use strict";
// Miharashi 管理ページ(docs/admin-web/design.md)。フレームワークなし。
// 応答の文字列は必ず textContent で出す(HTML として解釈させない)。URL はリンクにしない。

const TOKEN_KEY = "miharashi-admin-token";
const POLL_MS = 5000;
const MAX_TEXT = 300;
const RESTART_CHECK_MS = 5000;
const RESTART_GIVE_UP_MS = 90000;
const REQUEST_TIMEOUT_MS = 8000;

const BOOT_FAILED_TEXT =
  "この上書きで起動した直後に終了した(原因が上書きとは限らない)。直すか取り消してから再起動する";

const ITEM_LABELS = {
  "geoframe.url": "GEOFRAME の URL",
  "home_assistant.url": "Home Assistant の URL",
  "agents.url": "T3(エージェント)の URL",
  "agents.enabled": "T3(エージェント)画面",
  "aircraft.url": "tar1090 の URL",
  "aircraft.enabled": "航空機の画面",
  "room.enabled": "室内の画面",
  "cube.enabled": "キューブの画面",
  "vrm.enabled": "VRM の画面",
};

const FEED_STATES = {
  not_running: ["動いていない", "idle"],
  connecting: ["接続中", "warn"],
  ok: ["正常", "ok"],
  failing: ["失敗中", "failing"],
  auth_required: ["認証が必要", "failing"],
  forbidden: ["権限なし", "failing"],
};

const TOKEN_LABELS = {
  home_assistant: "Home Assistant",
  agents: "T3(エージェント)",
  panel: "操作パネル",
  admin: "管理ページ",
};

const TOKEN_STATES = {
  ok: ["正常", "ok"],
  missing: ["無い", "failing"],
  unreadable: ["開けない", "failing"],
  wrong_owner: ["所有者が違う", "warn"],
  loose_permissions: ["権限が緩い", "warn"],
};

const $ = (id) => document.getElementById(id);

const state = {
  token: null,
  // 画面に出している(フォームを作った)ときの overview と世代
  formGeneration: "none",
  fields: new Map(),
  dirty: false,
  stale: false,
  loggedIn: false,
  timer: null,
  inflight: false,
  saving: false,
  restartWaiting: false,
  restartPrevUptime: 0,
  restartStartedAt: 0,
  lastOverview: null,
};

// ---- 小さな道具 ----

function clip(value) {
  const s = String(value);
  return s.length > MAX_TEXT ? s.slice(0, MAX_TEXT) + "…" : s;
}

function el(tag, text, className) {
  const node = document.createElement(tag);
  if (text !== undefined && text !== null) node.textContent = clip(text);
  if (className) node.className = className;
  return node;
}

function showBand(node, text, kind) {
  node.className = "band " + kind;
  node.textContent = clip(text);
  node.hidden = false;
}

function hideBand(node) {
  node.hidden = true;
  node.textContent = "";
}

function setStatus(text, kind) {
  showBand($("status"), text, kind || "info");
}

function clearStatus() {
  hideBand($("status"));
}

function formatUptime(sec) {
  if (typeof sec !== "number" || !isFinite(sec) || sec < 0) return "—";
  const total = Math.floor(sec);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  if (h > 0) return h + " 時間 " + m + " 分";
  return m + " 分 " + (total % 60) + " 秒";
}

function formatTime(iso) {
  if (typeof iso !== "string" || iso === "") return "—";
  const d = new Date(iso);
  if (isNaN(d.getTime())) return clip(iso);
  return d.toLocaleString("ja-JP", {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function formatValue(kind, v) {
  if (v === null || v === undefined) return "—";
  if (kind === "bool") return v ? "有効" : "無効";
  return String(v);
}

function lookup(table, key) {
  return Object.prototype.hasOwnProperty.call(table, key) ? table[key] : null;
}

// ---- 通信 ----

async function api(method, path, body) {
  const headers = { Authorization: "Bearer " + state.token };
  const init = { method, headers, cache: "no-store" };
  if (body !== undefined) {
    headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  // 応答が来ないまま待ち続けない(本体が落ちている間は接続失敗として扱う)
  const ctl = new AbortController();
  init.signal = ctl.signal;
  const timer = setTimeout(() => ctl.abort(), REQUEST_TIMEOUT_MS);
  try {
    const resp = await fetch(path, init);
    let json = null;
    try {
      json = await resp.json();
    } catch (_) {
      json = null;
    }
    return { status: resp.status, json };
  } finally {
    clearTimeout(timer);
  }
}

function errorText(json, fallback) {
  if (json && typeof json.error === "string") return json.error;
  return fallback;
}

// ---- ログイン・ログアウト ----

function showLogin(message) {
  state.loggedIn = false;
  stopPolling();
  $("main").hidden = true;
  $("logout").hidden = true;
  $("login-section").hidden = false;
  $("token").value = "";
  if (message) setStatus(message, "warn");
}

function logout(message) {
  sessionStorage.removeItem(TOKEN_KEY);
  state.token = null;
  state.lastOverview = null;
  state.restartWaiting = false;
  state.dirty = false;
  state.stale = false;
  showLogin(message);
}

// ログイン直後の 1 回。成功したらフォームを作って定期更新を始める。
async function startSession(token, fromStorage) {
  state.token = token;
  let r;
  try {
    r = await api("GET", "/api/v1/admin/overview");
  } catch (_) {
    state.token = null;
    showLogin("接続できない。もう一度ログインする");
    return;
  }
  if (r.status === 401) {
    sessionStorage.removeItem(TOKEN_KEY);
    state.token = null;
    showLogin(fromStorage ? "ログインし直す" : "トークンが違う");
    return;
  }
  if (r.status !== 200 || !r.json) {
    state.token = null;
    showLogin(errorText(r.json, "読み込めなかった(" + r.status + ")"));
    return;
  }
  sessionStorage.setItem(TOKEN_KEY, token);
  clearStatus();
  state.loggedIn = true;
  $("login-section").hidden = true;
  $("main").hidden = false;
  $("logout").hidden = false;
  buildForm(r.json);
  renderLive(r.json);
  startPolling();
}

// ---- 定期更新(タブが見えているときだけ) ----

function startPolling() {
  stopPolling();
  if (!state.loggedIn || document.visibilityState !== "visible") return;
  state.timer = setInterval(poll, POLL_MS);
}

function stopPolling() {
  if (state.timer !== null) {
    clearInterval(state.timer);
    state.timer = null;
  }
}

// 失敗は短く出して次の周期に任せる。フォームには触らない。
async function fetchOverview() {
  if (state.inflight) return null;
  state.inflight = true;
  try {
    const r = await api("GET", "/api/v1/admin/overview");
    if (r.status === 200 && r.json) {
      clearStatus();
      return r.json;
    }
    if (r.status === 401) {
      logout("ログインし直す(トークンが通らなかった)");
    } else if (r.status === 429) {
      setStatus("混み合っている。次の周期で取り直す", "warn");
    } else {
      setStatus(errorText(r.json, "読み込めなかった(" + r.status + ")") + "。次の周期で取り直す", "warn");
    }
  } catch (_) {
    setStatus("接続できない。次の周期で取り直す", "warn");
  } finally {
    state.inflight = false;
  }
  // 取得に失敗し続けても、再起動の確認は期限で打ち切る
  if (state.loggedIn && state.restartWaiting) checkRestarted(null);
  return null;
}

async function poll() {
  if (!state.loggedIn) return;
  // 隠れているあいだは取りに行かない(再起動中のフラグは残し、戻ったときの取得で判定する)
  if (document.visibilityState !== "visible") return;
  const ov = await fetchOverview();
  if (ov && state.loggedIn) renderLive(ov);
}

document.addEventListener("visibilitychange", () => {
  if (!state.loggedIn) return;
  if (document.visibilityState === "visible") {
    poll();
    startPolling();
  } else {
    stopPolling();
  }
});

// ---- 描画: 稼働・取得元・トークン・再起動(5 秒ごとに描き直す。フォームは含まない) ----

function renderLive(ov) {
  state.lastOverview = ov;
  const overlay = ov.overlay || {};
  $("v-host").textContent = clip(ov.host === undefined ? "—" : ov.host);
  $("v-version").textContent = clip(ov.version === undefined ? "—" : ov.version);
  $("v-uptime").textContent = formatUptime(ov.uptime_s);
  const screen = ov.screen;
  $("v-screen").textContent = screen ? clip(screen.title || screen.id || "—") : "—";
  $("v-config-modified").textContent = formatTime(ov.config_file && ov.config_file.modified_at);

  const errBand = $("overlay-error");
  if (overlay.error) {
    const head = overlay.error_kind === "boot_failed" ? BOOT_FAILED_TEXT + "。 " : "上書きを無視している: ";
    showBand(errBand, head + String(overlay.error), "error");
  } else {
    hideBand(errBand);
  }

  renderFeeds(ov.feeds);
  renderTokens(ov.tokens);
  renderRestart(ov);

  const gen = typeof overlay.generation === "string" ? overlay.generation : "none";
  if (gen !== state.formGeneration) state.stale = true;
  $("stale").hidden = !state.stale;
}

function stateCell(table, key) {
  const known = lookup(table, key);
  const td = el("td", known ? known[0] : key, "state " + (known ? known[1] : "warn"));
  return td;
}

function renderFeeds(feeds) {
  const body = $("feeds");
  body.replaceChildren();
  for (const f of Array.isArray(feeds) ? feeds : []) {
    const tr = el("tr");
    tr.append(el("td", f.label || f.id));
    tr.append(stateCell(FEED_STATES, f.state));
    tr.append(el("td", formatTime(f.last_ok)));
    tr.append(el("td", f.error || "", "err"));
    body.append(tr);
  }
}

function renderTokens(tokens) {
  const body = $("tokens");
  body.replaceChildren();
  for (const t of Array.isArray(tokens) ? tokens : []) {
    const tr = el("tr");
    tr.append(el("td", lookup(TOKEN_LABELS, t.id) || t.id));
    tr.append(el("td", t.path || "", "path"));
    tr.append(stateCell(TOKEN_STATES, t.state));
    body.append(tr);
  }
}

function renderRestart(ov) {
  const overlay = ov.overlay || {};
  const restart = ov.restart || {};
  const btn = $("restart");
  const reason = $("restart-reason");
  $("pending-note").textContent = overlay.pending_restart
    ? "保存した値は、再起動すると反映される。"
    : "再起動を待っている変更は無い。";
  btn.className = overlay.pending_restart ? "primary" : "";

  let why = "";
  if (state.restartWaiting) {
    why = "再起動を待っている";
  } else if (overlay.error_kind === "boot_failed") {
    why = BOOT_FAILED_TEXT;
  } else if (restart.allowed === false) {
    if (restart.requested) {
      why = "再起動を受け付け済み。終了を待っている";
    } else if (typeof restart.retry_after_s === "number" && restart.retry_after_s > 0) {
      why = "起動から間もないので、あと約 " + Math.ceil(restart.retry_after_s) + " 秒たつと再起動できる";
    } else {
      why = "今は再起動できない";
    }
  }
  btn.disabled = why !== "";
  reason.textContent = why;

  if (state.restartWaiting) checkRestarted(ov);
}

// ov が null のときは取得に失敗したとき。期限の判定だけ行う。
function checkRestarted(ov) {
  const now = Date.now();
  const result = $("restart-result");
  if (ov && now - state.restartStartedAt >= RESTART_CHECK_MS && typeof ov.uptime_s === "number" && ov.uptime_s < state.restartPrevUptime) {
    state.restartWaiting = false;
    showBand(result, "再起動しました", "ok");
  } else if (now - state.restartStartedAt >= RESTART_GIVE_UP_MS) {
    state.restartWaiting = false;
    showBand(result, "再起動したか確かめられなかった。稼働の「起動からの時間」を見る", "warn");
    if (!ov) {
      // renderRestart が走らないので、ボタンと理由をここで戻す
      $("restart").disabled = false;
      $("restart-reason").textContent = "";
    }
  }
}

// ---- 描画: 接続先と画面のフォーム(ログイン直後と保存成功後にだけ作る) ----

function buildForm(ov) {
  const overlay = ov.overlay || {};
  state.formGeneration = typeof overlay.generation === "string" ? overlay.generation : "none";
  state.fields = new Map();
  state.dirty = false;
  state.stale = false;
  $("stale").hidden = true;
  const root = $("items");
  root.replaceChildren();
  const hosts = Array.isArray(ov.allowed_hosts) ? ov.allowed_hosts : [];

  for (const item of Array.isArray(ov.items) ? ov.items : []) {
    const key = String(item.key);
    const row = el("div", null, "item");
    const head = el("div", null, "item-head");
    head.append(el("strong", lookup(ITEM_LABELS, key) || key));
    head.append(el("code", key));
    row.append(head);

    const effectiveKind = item.kind === "bool" ? "bool" : "url";
    const effective = el("div", "効いている値: " + formatValue(effectiveKind, item.effective), "value effective");
    if (item.kind === "readonly") {
      row.append(effective);
    } else {
      const kind = item.kind === "bool" ? "bool" : "url";
      const editable = el("div", null, "editable");

      const ovLabel = el("label", null, "override");
      const cb = document.createElement("input");
      cb.type = "checkbox";
      cb.checked = item.overlay !== null && item.overlay !== undefined;
      ovLabel.append(cb, el("span", "上書き"));

      let input;
      let inputWrap;
      const initial = cb.checked ? item.overlay : item.file;
      if (kind === "bool") {
        input = document.createElement("input");
        input.type = "checkbox";
        input.checked = initial === true;
        inputWrap = el("label", null, "bool-value");
        inputWrap.append(input, el("span", "有効"));
      } else {
        input = document.createElement("input");
        input.type = "text";
        input.autocomplete = "off";
        input.spellcheck = false;
        input.value = typeof initial === "string" ? initial : "";
        inputWrap = input;
      }
      // 保存中に作り直されても、保存が終わる(setFormBusy(false))まで入力を受けない
      cb.disabled = state.saving;
      input.disabled = state.saving || !cb.checked;

      // 外すと「ファイルの値に戻す」(その項目は null)。入力はファイルの値の表示に戻して無効にする
      cb.addEventListener("change", () => {
        input.disabled = !cb.checked;
        if (!cb.checked) {
          if (kind === "bool") input.checked = item.file === true;
          else input.value = typeof item.file === "string" ? item.file : "";
        }
      });

      editable.append(ovLabel, inputWrap);
      row.append(editable);
      row.append(effective);
      state.fields.set(key, { kind, cb, input });
    }

    const refs = el("div", null, "refs");
    refs.append(el("span", "ファイルの値: " + formatValue(item.kind, item.file)));
    refs.append(el("span", "次回起動の値: " + formatValue(item.kind, item.next)));
    row.append(refs);
    if (item.note) row.append(el("div", item.note, "note"));
    if (key === "agents.url") {
      const text = hosts.length > 0 ? hosts.join(", ") : "(設定ファイルの接続先だけ)";
      row.append(el("div", "使えるホスト: " + text, "note hosts"));
    }
    root.append(row);
  }
}

$("config-form").addEventListener("input", () => {
  state.dirty = true;
});
$("config-form").addEventListener("change", () => {
  state.dirty = true;
});

// フォーム全体から Overlay の JSON を組み立てる(未変更の項目もそのまま。外した項目は null)
function buildOverlay() {
  const overlay = {};
  for (const [key, f] of state.fields) {
    const parts = key.split(".");
    if (parts.length !== 2 || !/^[a-z0-9_]+$/.test(parts[0]) || !/^[a-z0-9_]+$/.test(parts[1])) continue;
    if (!Object.prototype.hasOwnProperty.call(overlay, parts[0])) overlay[parts[0]] = {};
    let value = null;
    if (f.cb.checked) value = f.kind === "bool" ? f.input.checked : f.input.value.trim();
    overlay[parts[0]][parts[1]] = value;
  }
  return overlay;
}

// ---- 保存 ----

$("config-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  if (state.saving || !state.loggedIn) return;
  const body = { generation: state.formGeneration, overlay: buildOverlay() };
  state.saving = true;
  setFormBusy(true);
  const result = $("save-result");
  hideBand(result);
  try {
    const r = await api("PUT", "/api/v1/admin/config", body);
    if (r.status === 204) {
      showBand(result, "保存しました。再起動で反映します", "ok");
      await reloadForm(true);
    } else if (r.status === 401) {
      logout("ログインし直す(トークンが通らなかった)");
    } else if (r.status === 409) {
      state.stale = true;
      $("stale").hidden = false;
      showBand(result, errorText(r.json, "上書きファイルが変わっている") + "。読み直してからやり直す", "warn");
    } else if (r.status === 429) {
      const n = r.json && typeof r.json.retry_after_s === "number" ? r.json.retry_after_s : 1;
      showBand(result, "操作が続いている。" + Math.ceil(n) + " 秒後にもう一度保存する", "warn");
    } else if (r.status === 400) {
      showBand(result, errorText(r.json, "入力が正しくない"), "error");
    } else {
      showBand(result, errorText(r.json, "保存できなかった(" + r.status + ")"), "error");
    }
  } catch (_) {
    showBand(result, "接続できない。保存できたか分からないので、読み直して確かめる", "error");
  } finally {
    state.saving = false;
    setFormBusy(false);
  }
});

// 保存の送信から読み直しが終わるまで、入力を受けない(あとの読み直しで追加の入力を消さないため)
function setFormBusy(busy) {
  for (const f of state.fields.values()) {
    f.cb.disabled = busy;
    f.input.disabled = busy || !f.cb.checked;
  }
  $("save").disabled = busy;
  $("save").textContent = busy ? "保存中…" : "保存";
  $("reload").disabled = busy;
}

// overview を読み直してフォームを作り直す。保存直後は処理中で 429 になることがあるので少し待って繰り返す。
// 保存中は「読み直す」の応答でフォームを作らない(保存後の読み直し afterSave に任せる)。
async function reloadForm(afterSave = false) {
  for (let i = 0; i < 4; i += 1) {
    const ov = await fetchOverview();
    if (!afterSave && state.saving) return false;
    if (ov) {
      buildForm(ov);
      renderLive(ov);
      return true;
    }
    if (!state.loggedIn) return false;
    await new Promise((resolve) => setTimeout(resolve, 600));
  }
  return false;
}

$("reload").addEventListener("click", async () => {
  hideBand($("save-result"));
  const ok = await reloadForm();
  if (!ok && state.loggedIn && !state.saving) setStatus("読み直せなかった。もう一度押す", "warn");
});

// ---- 再起動 ----

$("restart").addEventListener("click", async () => {
  if (state.restartWaiting || !state.loggedIn) return;
  const msg = state.dirty
    ? "保存していない入力がある。それは反映されない。キオスクを再起動する?"
    : "キオスクを再起動する?";
  if (!window.confirm(msg)) return;
  const result = $("restart-result");
  hideBand(result);
  $("restart").disabled = true;
  try {
    const r = await api("POST", "/api/v1/admin/restart");
    if (r.status === 202) {
      state.restartWaiting = true;
      state.restartPrevUptime = state.lastOverview && typeof state.lastOverview.uptime_s === "number" ? state.lastOverview.uptime_s : Infinity;
      state.restartStartedAt = Date.now();
      showBand(result, "再起動中…", "info");
      $("restart-reason").textContent = "再起動を待っている";
      setTimeout(poll, RESTART_CHECK_MS);
    } else if (r.status === 401) {
      logout("ログインし直す(トークンが通らなかった)");
    } else if (r.status === 429) {
      const n = r.json && typeof r.json.retry_after_s === "number" ? "あと " + Math.ceil(r.json.retry_after_s) + " 秒" : "しばらく";
      showBand(result, errorText(r.json, "今は再起動できない") + "(" + n + "待つ)", "warn");
      $("restart").disabled = false;
    } else if (r.status === 409) {
      showBand(result, errorText(r.json, "今は再起動できない"), "warn");
      $("restart").disabled = false;
    } else {
      showBand(result, errorText(r.json, "再起動できなかった(" + r.status + ")"), "error");
      $("restart").disabled = false;
    }
  } catch (_) {
    showBand(result, "接続できない。再起動の指示が届いたか分からない", "error");
    $("restart").disabled = false;
  }
});

// ---- 起動 ----

$("login-form").addEventListener("submit", (ev) => {
  ev.preventDefault();
  const token = $("token").value.trim();
  if (token === "") return;
  $("token").value = "";
  startSession(token, false);
});

$("logout").addEventListener("click", () => logout(null));

(function init() {
  const saved = sessionStorage.getItem(TOKEN_KEY);
  if (saved) startSession(saved, true);
})();
