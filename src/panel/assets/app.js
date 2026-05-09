// ═══════════════════════════════════════════════════════════════════════════
// Crustoxy-Panel — Dashboard Application
// ═══════════════════════════════════════════════════════════════════════════

(function () {
  "use strict";

  let authToken = localStorage.getItem("crustoxy_token") || "";
  let config = null;
  let status = null;
  let currentPage = "dashboard";
  let providers = [];

  const $ = (s) => document.querySelector(s);
  const $$ = (s) => document.querySelectorAll(s);

  // ── API helpers ──────────────────────────────────────────────────────────

  async function api(method, path, body) {
    const headers = { "Content-Type": "application/json" };
    if (authToken) headers["Authorization"] = "Bearer " + authToken;
    const opts = { method, headers };
    if (body) opts.body = JSON.stringify(body);
    const res = await fetch("/api" + path, opts);
    if (res.status === 401) {
      showAuth();
      throw new Error("Unauthorized");
    }
    return res.json();
  }

  // ── Auth ─────────────────────────────────────────────────────────────────

  function showAuth() {
    $("#loading").style.display = "none";
    $("#app").style.display = "none";
    $("#auth-screen").style.display = "flex";
    setTimeout(() => $("#auth-input").focus(), 100);
  }

  function showApp() {
    $("#loading").style.display = "none";
    $("#auth-screen").style.display = "none";
    $("#app").style.display = "flex";
  }

  async function tryAuth(token) {
    try {
      const res = await fetch("/api/auth", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ token }),
      });
      const data = await res.json();
      if (data.authenticated) {
        authToken = token;
        localStorage.setItem("crustoxy_token", token);
        await loadAll();
        showApp();
        return true;
      }
      return false;
    } catch {
      return false;
    }
  }

  // ── Data loading ─────────────────────────────────────────────────────────

  async function loadAll() {
    try {
      [config, status, providers] = await Promise.all([
        api("GET", "/config"),
        api("GET", "/status"),
        api("GET", "/providers").then((r) => r.providers || []),
      ]);
      $("#version").textContent = "v" + (status.version || "?");
      updateProfileSelect();
      renderPage(currentPage);
      updateStatusIndicator();
    } catch (e) {
      console.error("Load failed:", e);
    }
  }

  function updateStatusIndicator() {
    const dot = $("#status-dot");
    const label = $("#status-label");
    if (status && status.status === "running") {
      dot.className = "status-dot healthy pulse";
      label.className = "status-text running";
      label.textContent = "RUNNING";
    } else {
      dot.className = "status-dot cooldown";
      label.className = "status-text setup";
      label.textContent = "SETUP";
    }
  }

  function updateProfileSelect() {
    const sel = $("#profile-select");
    if (!config || !config.profiles) return;
    sel.innerHTML = "";
    for (const key of Object.keys(config.profiles)) {
      const opt = document.createElement("option");
      opt.value = key;
      opt.textContent = config.profiles[key].name || key;
      if (key === config.general.active_profile) opt.selected = true;
      sel.appendChild(opt);
    }
  }

  function activeProfile() {
    if (!config) return null;
    return config.profiles[config.general.active_profile] || null;
  }

  // ── Save ─────────────────────────────────────────────────────────────────

  async function saveConfig() {
    const btn = $("#save-btn");
    const st = $("#save-status");
    btn.disabled = true;
    btn.textContent = "SAVING...";
    try {
      await api("PUT", "/config", config);
      status = await api("GET", "/status");
      st.textContent = "[APPLIED]";
      st.className = "inline-status success visible";
      updateStatusIndicator();
      setTimeout(() => {
        st.className = "inline-status success";
      }, 3000);
    } catch (e) {
      st.textContent = "[ERROR]";
      st.className = "inline-status error visible";
    }
    btn.disabled = false;
    btn.textContent = "SAVE & APPLY";
  }

  // ── Modal ────────────────────────────────────────────────────────────────

  function openModal(title, html, onBind) {
    $("#modal-title").textContent = title;
    $("#modal-body").innerHTML = html;
    $("#modal-overlay").classList.add("visible");
    if (onBind) onBind();
  }

  function closeModal() {
    $("#modal-overlay").classList.remove("visible");
  }

  // ── Navigation ───────────────────────────────────────────────────────────

  function renderPage(page) {
    currentPage = page;
    $$(".nav-item").forEach((el) => {
      el.classList.toggle("active", el.dataset.page === page);
    });
    $("#page-title").textContent = pageTitle(page);
    const c = $("#main-content");
    c.innerHTML = "";
    c.className = "main-content fade-in";
    const renderers = {
      dashboard: renderDashboard,
      models: renderModels,
      keys: renderKeys,
      features: renderFeatures,
      routing: renderRouting,
      profiles: renderProfiles,
      settings: renderSettings,
    };
    (renderers[page] || renderDashboard)(c);
  }

  function pageTitle(p) {
    const map = {
      dashboard: "Dashboard",
      models: "Model Mapping",
      keys: "API Keys",
      features: "Features",
      routing: "Routing Strategy",
      profiles: "Profiles",
      settings: "Settings",
    };
    return map[p] || "Dashboard";
  }

  // ── Dashboard ────────────────────────────────────────────────────────────

  function renderDashboard(c) {
    const p = activeProfile();
    if (!p) return (c.innerHTML = '<div class="empty-state"><div class="empty-state-text">[NO PROFILE]</div></div>');

    const keyCount = Object.values(p.provider_keys || {}).reduce((s, v) => s + v.split(";").filter((x) => x.trim()).length, 0);
    const modelCount = ["default", "opus", "sonnet", "haiku"].reduce((s, t) => {
      const v = p.model_mapping[t] || "";
      return s + v.split(";").filter((x) => x.trim()).length;
    }, 0);

    c.innerHTML = `
      <div class="dashboard-grid">
        <div class="stat-card">
          <div class="stat-label">ACTIVE PROFILE</div>
          <div class="stat-value">${p.name || "Default"}</div>
        </div>
        <div class="stat-card">
          <div class="stat-label">TOTAL MODELS</div>
          <div class="stat-value">${modelCount}</div>
          <div class="stat-sub">Across all tiers</div>
        </div>
        <div class="stat-card">
          <div class="stat-label">API KEYS</div>
          <div class="stat-value">${keyCount}</div>
          <div class="stat-sub">In key pool</div>
        </div>
      </div>
      <div class="card"><div class="card-header"><div class="card-title">KEY POOL HEALTH</div></div>
        <div id="dash-keys"></div>
      </div>
      <div class="card"><div class="card-header"><div class="card-title">MODEL ROUTER STATUS</div></div>
        <div id="dash-models"></div>
      </div>`;

    renderKeyHealth($("#dash-keys"));
    renderModelHealth($("#dash-models"));
  }

  function renderKeyHealth(el) {
    if (!status || !status.key_pools) {
      el.innerHTML = '<div class="empty-state"><div class="empty-state-text">[NO KEY DATA]</div></div>';
      return;
    }
    let html = '<div class="table-wrap"><table><tr><th>Provider</th><th>Key</th><th>Status</th><th>Requests</th><th>Errors</th></tr>';
    for (const [prov, keys] of Object.entries(status.key_pools)) {
      for (const k of keys) {
        const cls = k.on_cooldown ? "cooldown" : k.healthy ? "healthy" : "unhealthy";
        const label = k.on_cooldown ? "COOLDOWN" : k.healthy ? "HEALTHY" : "DOWN";
        html += `<tr><td>${prov}</td><td>${k.key_preview}</td><td><span class="status-dot ${cls}"></span>${label}</td><td>${k.total_requests}</td><td>${k.total_errors}</td></tr>`;
      }
    }
    html += "</table></div>";
    el.innerHTML = Object.keys(status.key_pools).length ? html : '<div class="empty-state"><div class="empty-state-text">[NO KEYS CONFIGURED]</div></div>';
  }

  function renderModelHealth(el) {
    if (!status || !status.model_router) {
      el.innerHTML = '<div class="empty-state"><div class="empty-state-text">[NO MODEL DATA]</div></div>';
      return;
    }
    let html = '<div class="table-wrap"><table><tr><th>Tier</th><th>Provider</th><th>Model</th><th>Status</th></tr>';
    for (const [tier, models] of Object.entries(status.model_router)) {
      for (const m of models) {
        const cls = m.on_cooldown ? "cooldown" : m.healthy ? "healthy" : "unhealthy";
        const label = m.on_cooldown ? "COOLDOWN" : m.healthy ? "READY" : "DOWN";
        html += `<tr><td style="text-transform:uppercase">${tier}</td><td>${m.provider}</td><td>${m.model_name}</td><td><span class="status-dot ${cls}"></span>${label}</td></tr>`;
      }
    }
    html += "</table></div>";
    el.innerHTML = Object.keys(status.model_router).length ? html : '<div class="empty-state"><div class="empty-state-text">[NO MODELS CONFIGURED]</div></div>';
  }

  // ── Models Page ──────────────────────────────────────────────────────────

  function renderModels(c) {
    const p = activeProfile();
    if (!p) return;
    const tiers = ["default", "opus", "sonnet", "haiku"];
    let html = '<div class="form-hint" style="margin-bottom:16px">Configure model mappings per Claude tier. Multiple models enable auto-routing.</div>';
    for (const tier of tiers) {
      const val = (p.model_mapping[tier] || "").split(";").map(s => s.trim()).filter(s => s).join("\\n");
      html += `
        <div class="tier-card">
          <div class="tier-name">${tier === "default" ? "DEFAULT (FALLBACK)" : tier}</div>
          <div class="form-group">
            <label class="form-label">MODELS (one per line)</label>
            <textarea class="form-textarea" data-tier="${tier}" rows="3" placeholder="provider/model\\nprovider/model">${val}</textarea>
            <div class="form-hint">e.g. openrouter/deepseek/deepseek-r1</div>
          </div>
        </div>`;
    }
    c.innerHTML = html;
    c.querySelectorAll("textarea[data-tier]").forEach((el) => {
      el.addEventListener("input", () => {
        const tier = el.dataset.tier;
        activeProfile().model_mapping[tier] = el.value.split("\\n").map(s => s.trim()).filter(s => s).join(" ; ");
      });
    });
  }

  // ── Keys Page ────────────────────────────────────────────────────────────

  function renderKeys(c) {
    const p = activeProfile();
    if (!p) return;
    let html = `<div class="form-hint" style="margin-bottom:16px">Configure API key pools per provider. Multiple keys enable load-balanced rotation.</div>
      <div class="card"><div class="card-header"><div class="card-title">PROVIDERS</div></div>
        <div id="key-list"></div>
      </div>`;
    c.innerHTML = html;
    renderKeyList();
  }

  function renderKeyList() {
    const el = $("#key-list");
    if (!el) return;
    const p = activeProfile();
    
    let html = '<div class="table-wrap"><table><tr><th>Provider</th><th>Keys Configured</th><th></th></tr>';
    for (const pr of providers) {
      const prov = pr.name;
      const rawKeys = p.provider_keys[prov] || "";
      const count = rawKeys.split(";").filter((x) => x.trim()).length;
      html += `<tr><td>${prov}</td><td>${count} key(s)</td><td style="text-align:right"><button class="btn btn-sm btn-prov-cfg" data-prov="${prov}">CONFIGURE</button></td></tr>`;
    }
    html += "</table></div>";
    el.innerHTML = html;
    
    el.querySelectorAll(".btn-prov-cfg").forEach((btn) => {
      btn.addEventListener("click", () => {
        openProviderConfigModal(btn.dataset.prov);
      });
    });
  }

  function openProviderConfigModal(prov) {
    const p = activeProfile();
    const providerObj = providers.find(x => x.name === prov) || { default_base_url: "" };
    
    const existingKeys = (p.provider_keys[prov] || "").split(";").map(s => s.trim()).filter(s => s).join("\\n");
    const existingUrl = p.provider_base_urls[prov] || "";
    
    const html = `
      <div class="form-group">
        <label class="form-label">BASE URL OVERRIDE</label>
        <input id="modal-prov-url" class="form-input" placeholder="${providerObj.default_base_url || 'https://...'}" value="${existingUrl}">
        <div class="form-hint">Leave empty to use default</div>
      </div>
      <div class="form-group">
        <label class="form-label">API KEYS (one per line)</label>
        <textarea id="modal-prov-keys" class="form-textarea" rows="5" placeholder="key1\\nkey2">${existingKeys}</textarea>
      </div>
      <button id="modal-prov-save" class="btn btn-primary" style="width:100%">SAVE CHANGES</button>
    `;
    
    openModal("Configure " + prov, html, () => {
      $("#modal-prov-save").addEventListener("click", () => {
        const newUrl = $("#modal-prov-url").value.trim();
        const newKeysText = $("#modal-prov-keys").value;
        const newKeys = newKeysText.split("\\n").map(s => s.trim()).filter(s => s).join(" ; ");
        
        if (newUrl) p.provider_base_urls[prov] = newUrl;
        else delete p.provider_base_urls[prov];
        
        if (newKeys) p.provider_keys[prov] = newKeys;
        else delete p.provider_keys[prov];
        
        closeModal();
        renderKeyList(); // Re-render the table
      });
    });
  }

  // ── Features Page ────────────────────────────────────────────────────────

  function renderFeatures(c) {
    const p = activeProfile();
    if (!p) return;
    const f = p.features;
    const toggles = [
      ["enable_ip_rotation", "IP Rotation (WARP)", "Rotate Cloudflare WARP IP on 429 errors"],
      ["enable_network_probe_mock", "Network Probe Mock", "Mock Claude's network test to save API calls"],
      ["enable_title_generation_skip", "Title Generation Skip", "Skip background title generation"],
      ["enable_suggestion_mode_skip", "Suggestion Mode Skip", "Mock suggestion queries"],
      ["fast_prefix_detection", "Fast Prefix Detection", "Accelerate chunk prefix parsing"],
      ["enable_filepath_extraction_mock", "Filepath Extraction Mock", "Mock intensive filepath searches"],
      ["enable_tool_retry", "Auto Tool Retry", "Retry when model fails structured tool JSON"],
      ["enable_rtk", "RTK Compaction", "Compact Claude Code system prompts to save tokens"],
    ];
    let html = "";
    for (const [key, label, desc] of toggles) {
      const active = f[key] ? "active" : "";
      html += `<div class="card" style="padding:16px 24px;display:flex;justify-content:space-between;align-items:center">
        <div><div style="font-size:14px;color:var(--text-primary)">${label}</div><div class="form-hint">${desc}</div></div>
        <div class="toggle ${active}" data-key="${key}"><div class="toggle-track"><div class="toggle-thumb"></div></div></div>
      </div>`;
    }
    html += `<div class="card"><div class="card-header"><div class="card-title">TOOL RETRY MAX</div></div>
      <input class="form-input" type="number" id="tool-retry-max" value="${f.tool_retry_max || 2}" min="0" max="10" style="width:100px">
    </div>`;
    c.innerHTML = html;

    c.querySelectorAll(".toggle").forEach((el) => {
      el.addEventListener("click", () => {
        const key = el.dataset.key;
        f[key] = !f[key];
        el.classList.toggle("active", f[key]);
      });
    });
    const retryInput = $("#tool-retry-max");
    if (retryInput) retryInput.addEventListener("change", () => { f.tool_retry_max = parseInt(retryInput.value) || 2; });
  }

  // ── Routing Page ─────────────────────────────────────────────────────────

  function renderRouting(c) {
    const p = activeProfile();
    if (!p) return;
    const r = p.routing;
    const strategies = ["round_robin", "random", "least_errors"];
    c.innerHTML = `
      <div class="card"><div class="card-header"><div class="card-title">MODEL ROUTING STRATEGY</div></div>
        <select class="form-select" id="r-model-strat">${strategies.map((s) => `<option value="${s}" ${r.model_strategy === s ? "selected" : ""}>${s.replace(/_/g, " ").toUpperCase()}</option>`).join("")}</select>
      </div>
      <div class="card"><div class="card-header"><div class="card-title">KEY ROUTING STRATEGY</div></div>
        <select class="form-select" id="r-key-strat">${strategies.map((s) => `<option value="${s}" ${r.key_strategy === s ? "selected" : ""}>${s.replace(/_/g, " ").toUpperCase()}</option>`).join("")}</select>
      </div>
      <div class="dashboard-grid">
        <div class="card"><div class="card-header"><div class="card-title">RATE LIMIT COOLDOWN (s)</div></div>
          <input class="form-input" type="number" id="r-cooldown" value="${r.rate_limit_cooldown}" min="5">
        </div>
        <div class="card"><div class="card-header"><div class="card-title">MAX CONSECUTIVE ERRORS</div></div>
          <input class="form-input" type="number" id="r-maxerr" value="${r.max_consecutive_errors}" min="1">
        </div>
        <div class="card"><div class="card-header"><div class="card-title">HEALTH RECOVERY (s)</div></div>
          <input class="form-input" type="number" id="r-recovery" value="${r.health_recovery_interval}" min="10">
        </div>
      </div>`;

    const bind = (id, key, parse) => {
      const el = document.getElementById(id);
      if (el) el.addEventListener("change", () => { r[key] = parse ? parse(el.value) : el.value; });
    };
    bind("r-model-strat", "model_strategy");
    bind("r-key-strat", "key_strategy");
    bind("r-cooldown", "rate_limit_cooldown", Number);
    bind("r-maxerr", "max_consecutive_errors", Number);
    bind("r-recovery", "health_recovery_interval", Number);
  }

  // ── Profiles Page ────────────────────────────────────────────────────────

  function renderProfiles(c) {
    let html = `<div class="card"><div class="card-header"><div class="card-title">CREATE PROFILE</div></div>
      <div style="display:flex;gap:8px">
        <input class="form-input" id="new-prof-name" placeholder="Profile Name" style="flex:1">
        <button class="btn btn-sm" id="create-prof-btn">CREATE</button>
      </div>
    </div>
    <div class="card"><div class="card-header"><div class="card-title">ALL PROFILES</div></div>
      <div id="prof-list"></div>
    </div>`;
    c.innerHTML = html;
    renderProfileList();

    $("#create-prof-btn").addEventListener("click", async () => {
      const name = $("#new-prof-name").value.trim();
      const key = name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '');
      if (key && name) {
        await api("POST", "/profiles", { key, name });
        await loadAll();
      }
    });
  }

  function renderProfileList() {
    const el = $("#prof-list");
    if (!el || !config) return;
    let html = '<div class="table-wrap"><table><tr><th>Key</th><th>Name</th><th>Status</th><th></th></tr>';
    for (const [key, prof] of Object.entries(config.profiles)) {
      const isActive = key === config.general.active_profile;
      html += `<tr>
        <td>${key}</td><td>${prof.name}</td>
        <td>${isActive ? '<span style="color:var(--success)">● ACTIVE</span>' : '<button class="btn btn-sm prof-activate" data-key="' + key + '">ACTIVATE</button>'}</td>
        <td>${!isActive ? '<span class="model-remove prof-delete" data-key="' + key + '">✕</span>' : ""}</td>
      </tr>`;
    }
    html += "</table></div>";
    el.innerHTML = html;

    el.querySelectorAll(".prof-activate").forEach((btn) => {
      btn.addEventListener("click", async () => {
        await api("POST", "/profiles/" + btn.dataset.key + "/activate");
        await loadAll();
      });
    });
    el.querySelectorAll(".prof-delete").forEach((btn) => {
      btn.addEventListener("click", async () => {
        await api("DELETE", "/profiles/" + btn.dataset.key);
        await loadAll();
      });
    });
  }

  // ── Settings Page ────────────────────────────────────────────────────────

  function renderSettings(c) {
    const p = activeProfile();
    if (!p) return;
    const rl = p.rate_limiting;
    const to = p.timeouts;
    c.innerHTML = `
      <div class="card"><div class="card-header"><div class="card-title">RATE LIMITING</div></div>
        <div class="dashboard-grid">
          <div class="form-group"><label class="form-label">REQUESTS PER WINDOW</label>
            <input class="form-input" type="number" id="s-ratelimit" value="${rl.provider_rate_limit}"></div>
          <div class="form-group"><label class="form-label">WINDOW (SECONDS)</label>
            <input class="form-input" type="number" id="s-ratewindow" value="${rl.provider_rate_window}"></div>
          <div class="form-group"><label class="form-label">MAX CONCURRENCY</label>
            <input class="form-input" type="number" id="s-maxconc" value="${rl.provider_max_concurrency}"></div>
        </div>
      </div>
      <div class="card"><div class="card-header"><div class="card-title">TIMEOUTS</div></div>
        <div class="dashboard-grid">
          <div class="form-group"><label class="form-label">READ TIMEOUT (s)</label>
            <input class="form-input" type="number" id="s-readto" value="${to.http_read_timeout}"></div>
          <div class="form-group"><label class="form-label">CONNECT TIMEOUT (s)</label>
            <input class="form-input" type="number" id="s-connto" value="${to.http_connect_timeout}"></div>
        </div>
      </div>`;

    const bindNum = (id, obj, key) => {
      const el = document.getElementById(id);
      if (el) el.addEventListener("change", () => { obj[key] = parseInt(el.value) || 0; });
    };
    bindNum("s-ratelimit", rl, "provider_rate_limit");
    bindNum("s-ratewindow", rl, "provider_rate_window");
    bindNum("s-maxconc", rl, "provider_max_concurrency");
    bindNum("s-readto", to, "http_read_timeout");
    bindNum("s-connto", to, "http_connect_timeout");
  }

  // ── Init ─────────────────────────────────────────────────────────────────

  async function init() {
    // Try loading without auth first
    try {
      config = await api("GET", "/config");
      status = await api("GET", "/status");
      providers = (await api("GET", "/providers")).providers || [];
      $("#version").textContent = "v" + (status.version || "?");
      updateProfileSelect();
      showApp();
      renderPage("dashboard");
      updateStatusIndicator();
    } catch {
      // Needs auth
      showAuth();
    }

    // Event listeners
    $("#auth-btn").addEventListener("click", async () => {
      const ok = await tryAuth($("#auth-input").value);
      if (!ok) $("#auth-error").textContent = "[INVALID TOKEN]";
    });
    $("#auth-input").addEventListener("keydown", (e) => {
      if (e.key === "Enter") $("#auth-btn").click();
    });

    $$(".nav-item").forEach((el) => {
      el.addEventListener("click", () => renderPage(el.dataset.page));
    });

    $("#save-btn").addEventListener("click", saveConfig);
    
    $("#modal-close").addEventListener("click", closeModal);
    $("#modal-overlay").addEventListener("click", (e) => {
      if (e.target.id === "modal-overlay") closeModal();
    });

    $("#profile-select").addEventListener("change", async (e) => {
      config.general.active_profile = e.target.value;
      await saveConfig();
      await loadAll();
    });

    // Auto-refresh status every 30s
    setInterval(async () => {
      try {
        status = await api("GET", "/status");
        updateStatusIndicator();
        if (currentPage === "dashboard") renderPage("dashboard");
      } catch { /* ignore */ }
    }, 30000);
  }

  document.addEventListener("DOMContentLoaded", init);
})();
