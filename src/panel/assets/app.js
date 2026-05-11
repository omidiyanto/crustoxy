// ═══════════════════════════════════════════════════════════════════════════
// Crustoxy-Panel — Dashboard Application
// ═══════════════════════════════════════════════════════════════════════════

(function () {
  "use strict";

  let authToken = localStorage.getItem("crustoxy_token") || "";
  let config = null;
  let status = null;
  let currentPage = null;
  let activeKeyProvider = null;
  let providers = [];

  const $ = (s) => document.querySelector(s);
  const $$ = (s) => document.querySelectorAll(s);
  const HTML_ESCAPE = {
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
  };

  const escapeHtml = (value) => String(value ?? "").replace(/[&<>"']/g, (ch) => HTML_ESCAPE[ch]);
  const escapeAttr = escapeHtml;
  const safeNumber = (value) => Number.isFinite(Number(value)) ? Number(value) : 0;

  function normalizePage(page) {
    if (page === "providers") return "keys";
    return page || "dashboard";
  }

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
      renderPage(currentPage || localStorage.getItem("crustoxy_page") || "dashboard", { force: true });
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

  function renderPage(page, opts = {}) {
    const nextPage = normalizePage(page);
    if (currentPage === nextPage && !opts.force) return;
    currentPage = nextPage;
    if (nextPage !== "keys") activeKeyProvider = null;
    localStorage.setItem('crustoxy_page', nextPage);

    $$(".nav-item").forEach((el) => {
      el.classList.toggle("active", normalizePage(el.dataset.page) === nextPage);
    });
    $("#page-title").textContent = pageTitle(nextPage);
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
    (renderers[nextPage] || renderDashboard)(c);
  }

  function pageTitle(p) {
    const map = {
      dashboard: "Dashboard",
      models: "Model Mapping",
      keys: "Providers",
      features: "Features",
      routing: "Routing Strategy",
      profiles: "Profiles",
      settings: "Settings",
    };
    return map[p] || "Dashboard";
  }

  function rerenderCurrentPage() {
    const c = $("#main-content");
    if (!c) return;
    if (currentPage === "keys" && activeKeyProvider) {
      renderProviderDetails(c, activeKeyProvider);
      return;
    }
    renderPage(currentPage || "dashboard", { force: true });
  }

  // ── Dashboard ────────────────────────────────────────────────────────────

  function renderDashboard(c) {
    const p = activeProfile();
    if (!p) return (c.innerHTML = '<div class="empty-state"><div class="empty-state-text">[NO PROFILE]</div></div>');

    const keyCount = Object.values(p.provider_keys || {}).reduce((s, v) => s + v.split(";").filter((x) => x.trim()).length, 0);
    const modelCount = ["default", "opus", "sonnet", "haiku"].reduce((s, t) => {
      const v = (p.model_mapping && p.model_mapping[t]) || "";
      return s + v.split(";").filter((x) => x.trim()).length;
    }, 0);

    c.innerHTML = `
      <div class="dashboard-grid">
        <div class="stat-card">
          <div class="stat-label">ACTIVE PROFILE</div>
          <div class="stat-value">${escapeHtml(p.name || "Default")}</div>
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
        html += `<tr><td>${escapeHtml(prov)}</td><td>${escapeHtml(k.key_preview)}</td><td><span class="status-dot ${cls}"></span>${label}</td><td>${safeNumber(k.total_requests)}</td><td>${safeNumber(k.total_errors)}</td></tr>`;
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
        html += `<tr><td style="text-transform:uppercase">${escapeHtml(tier)}</td><td>${escapeHtml(m.provider)}</td><td>${escapeHtml(m.model_name)}</td><td><span class="status-dot ${cls}"></span>${label}</td></tr>`;
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

    let html = '<div class="form-hint" style="margin-bottom:16px">Select a Claude tier to view and configure its model routing map.</div>';
    html += '<div class="dashboard-grid">';

    for (const tier of tiers) {
      const models = ((p.model_mapping && p.model_mapping[tier]) || "").split(";").filter(s => s.trim());
      html += `
        <div class="stat-card" style="display:flex; flex-direction:column; justify-content:space-between">
          <div>
            <div class="stat-label">${tier === "default" ? "DEFAULT (FALLBACK)" : tier}</div>
            <div class="stat-value">${safeNumber(models.length)}</div>
            <div class="stat-sub">models configured</div>
          </div>
          <button class="btn btn-sm btn-prov-cfg" style="margin-top:16px" data-tier="${tier}">OPEN</button>
        </div>`;
    }
    html += '</div>';

    c.innerHTML = html;

    c.querySelectorAll("button[data-tier]").forEach(btn => {
      btn.addEventListener("click", () => {
        renderTierDetails(c, btn.dataset.tier);
      });
    });
  }

  function renderTierDetails(c, tier) {
    const p = activeProfile();
    const rawVal = (p.model_mapping && p.model_mapping[tier]) || "";
    const models = rawVal.split(";").map(s => s.trim()).filter(Boolean);

    let html = `<div style="margin-bottom: 16px; display:flex; justify-content:space-between; align-items:center;">
      <button class="btn btn-sm" id="back-to-models">← BACK</button>
      <button class="btn btn-sm btn-primary" id="cfg-tier">CONFIGURE</button>
    </div>`;

    html += `<div class="card" style="min-height: 300px; display:flex; align-items:center; justify-content:center; padding: 40px; overflow-x:auto;">
      <div style="display:flex; align-items:center; gap:0;">
        <!-- Left node (Tier) -->
        <div style="padding:16px 24px; background:var(--surface-raised); border:1px solid var(--accent); border-radius:8px; text-align:center; z-index:2; min-width:140px;">
          <div style="font-family:'Space Mono',monospace; font-size:11px; color:var(--text-secondary); margin-bottom:4px;">ROUTING TIER</div>
          <div style="font-size:18px; font-weight:500; color:var(--text-display); text-transform:uppercase;">${escapeHtml(tier === "default" ? "DEFAULT" : tier)}</div>
        </div>
        
        <!-- Center connector line -->
        ${models.length ? `<div style="width:40px; height:2px; background:var(--border-visible);"></div>` : ''}
        
        <!-- Right nodes container (vertical list) -->
        <div style="display:flex; flex-direction:column; gap:16px; position:relative;">
    `;

    if (models.length === 0) {
      html += `<div class="empty-state-text" style="margin-left:40px">[NO MODELS CONFIGURED]</div>`;
    } else {
      // Draw vertical stem line for multiple items
      if (models.length > 1) {
        html += `<div style="position:absolute; left:0; top:50%; transform:translateY(-50%); width:2px; height:calc(100% - 64px); background:var(--border-visible);"></div>`;
      }

      for (const m of models) {
        const parts = m.split("/");
        const prov = parts[0] || "unknown";
        const modelName = parts.slice(1).join("/") || "unknown";

        const hasKey = p.provider_keys && p.provider_keys[prov] && p.provider_keys[prov].trim().length > 0;
        const keyWarning = hasKey ? '' : '<span style="color:var(--warning);font-size:10px;margin-left:8px;border:1px solid var(--warning);padding:2px 4px;border-radius:4px;">NO API KEY</span>';

        html += `
          <div style="display:flex; align-items:center; position:relative; z-index:2;">
            <div style="width:20px; height:2px; background:var(--border-visible);"></div>
            <div style="padding:12px 16px; background:var(--surface-raised); border:1px solid var(--border); border-radius:8px; width: 280px; transition:border-color 0.2s;">
              <div style="font-family:'Space Mono',monospace; font-size:11px; color:var(--text-secondary); margin-bottom:4px; display:flex; align-items:center; justify-content:space-between;">
                <span>${escapeHtml(prov.toUpperCase())}</span>
                ${keyWarning}
              </div>
              <div style="font-family:'Space Mono',monospace; font-size:13px; color:var(--text-primary); white-space:nowrap; overflow:hidden; text-overflow:ellipsis;" title="${escapeAttr(modelName)}">${escapeHtml(modelName)}</div>
            </div>
          </div>
        `;
      }
    }

    html += `
        </div>
      </div>
    </div>`;

    c.innerHTML = html;

    $("#back-to-models").addEventListener("click", () => renderModels(c));
    $("#cfg-tier").addEventListener("click", () => {
      const val = ((p.model_mapping && p.model_mapping[tier]) || "").split(";").map(s => s.trim()).filter(Boolean).join("\n");

      const modalHtml = `
        <div class="form-group">
          <label class="form-label">MODELS (one per line)</label>
          <textarea id="modal-tier-models" class="form-textarea" rows="6" placeholder="provider/model\\nprovider/model">${escapeHtml(val)}</textarea>
          <div class="form-hint" style="margin-top:8px">Order determines routing priority (top to bottom).</div>
        </div>
        <button id="modal-tier-save" class="btn btn-primary" style="width:100%">SAVE MAPPING</button>
      `;

      openModal("Configure " + tier.toUpperCase() + " Tier", modalHtml, () => {
        $("#modal-tier-save").addEventListener("click", async () => {
          const newVal = $("#modal-tier-models").value.split("\n").map(s => s.trim()).filter(Boolean).join(" ; ");
          if (!p.model_mapping) p.model_mapping = {};
          p.model_mapping[tier] = newVal;
          closeModal();
          await saveConfig();
          renderTierDetails(c, tier);
        });
      });
    });
  }

  // ── Keys Page ────────────────────────────────────────────────────────────

  function renderKeys(c) {
    const p = activeProfile();
    if (!p) return;
    activeKeyProvider = null;
    let html = `<div class="form-hint" style="margin-bottom:16px">Configure API key pools per provider. Multiple keys enable load-balanced rotation.</div>
      <div class="card">
        <div class="card-header" style="display:flex; justify-content:space-between; align-items:center;">
          <div class="card-title" style="margin-bottom:0">PROVIDERS</div>
          <input type="text" id="prov-search" class="form-input" placeholder="Search providers..." style="width:250px; font-size:13px; padding:6px 12px">
        </div>
        <div id="key-list"></div>
      </div>`;
    c.innerHTML = html;
    renderKeyList();

    $("#prov-search").addEventListener("input", (e) => {
      renderKeyList(e.target.value.trim().toLowerCase());
    });
  }

  function renderKeyList(searchQuery = "") {
    const el = $("#key-list");
    if (!el) return;
    const p = activeProfile();

    const sortedProvs = [...providers].sort((a, b) => {
      const aName = String(a.name || "");
      const bName = String(b.name || "");
      if (aName.toLowerCase() === "custom") return -1;
      if (bName.toLowerCase() === "custom") return 1;
      return aName.localeCompare(bName);
    });

    let html = '<div class="table-wrap"><table><tr><th>Provider</th><th>Keys Configured</th><th></th></tr>';
    let countMatch = 0;

    for (const pr of sortedProvs) {
      const prov = String(pr.name || "");
      if (searchQuery && !prov.toLowerCase().includes(searchQuery)) continue;
      countMatch++;

      const rawKeys = (p.provider_keys && p.provider_keys[prov]) || "";
      const count = rawKeys.split(";").filter((x) => x.trim()).length;
      html += `<tr><td>${escapeHtml(prov)}</td><td>${safeNumber(count)} key(s)</td><td style="text-align:right"><button class="btn btn-sm btn-prov-cfg" data-prov="${escapeAttr(prov)}">CONFIGURE</button></td></tr>`;
    }

    if (countMatch === 0) {
      html += `<tr><td colspan="3" style="text-align:center; color:var(--text-disabled)">[NO PROVIDERS FOUND]</td></tr>`;
    }

    html += "</table></div>";
    el.innerHTML = html;

    el.querySelectorAll(".btn-prov-cfg").forEach((btn) => {
      btn.addEventListener("click", () => {
        renderProviderDetails($("#main-content"), btn.dataset.prov);
      });
    });
  }

  function renderProviderDetails(c, prov) {
    const p = activeProfile();
    activeKeyProvider = prov;
    const providerObj = providers.find(x => x.name === prov) || { default_base_url: "" };
    const existingUrl = (p.provider_base_urls && p.provider_base_urls[prov]) || "";
    const rawKeys = (p.provider_keys && p.provider_keys[prov]) || "";
    const keysArray = rawKeys.split(";").map(s => s.trim()).filter(Boolean);

    let html = `<div style="margin-bottom: 16px; display:flex; justify-content:space-between; align-items:center;">
      <button class="btn btn-sm" id="back-to-provs">← BACK</button>
    </div>`;

    html += `
      <div class="card"><div class="card-header"><div class="card-title">${escapeHtml(prov.toUpperCase())} SETTINGS</div></div>
        <div class="form-group" style="margin-bottom:0">
          <label class="form-label">BASE URL OVERRIDE</label>
          <div style="display:flex;gap:8px">
            <input id="prov-url" class="form-input" placeholder="${escapeAttr(providerObj.default_base_url || 'https://...')}" value="${escapeAttr(existingUrl)}" style="flex:1">
            <button id="save-prov-url" class="btn btn-sm">SAVE URL</button>
          </div>
          <div class="form-hint" style="margin-top:8px">Leave empty to use default. Save immediately to apply.</div>
        </div>
      </div>
      
      <div class="card"><div class="card-header"><div class="card-title">API KEYS POOL</div></div>
        <div class="table-wrap">
          <table>
            <tr><th>Key</th><th>Status</th><th>Reqs</th><th>Errs</th><th></th></tr>
    `;

    const provStatus = (status && status.key_pools && status.key_pools[prov]) || [];

    if (keysArray.length === 0) {
      html += `<tr><td colspan="5" style="text-align:center; color:var(--text-disabled)">[NO KEYS CONFIGURED]</td></tr>`;
    } else {
      keysArray.forEach((fullKey, idx) => {
        // Match backend mask_key(): first 3 + "..." + last 3 chars (or "***" for short keys)
        const masked = fullKey.length > 8
          ? fullKey.substring(0, 3) + "..." + fullKey.substring(fullKey.length - 3)
          : "***";
        const kStats = provStatus.find(s => s.key_preview === masked) || { healthy: false, on_cooldown: false, total_requests: 0, total_errors: 0, _unknown: true };

        let statLabel = "UNKNOWN";
        let statCls = "";
        if (!kStats._unknown) {
          statCls = kStats.on_cooldown ? "cooldown" : kStats.healthy ? "healthy" : "unhealthy";
          statLabel = kStats.on_cooldown ? "COOLDOWN" : kStats.healthy ? "HEALTHY" : "ERROR";
        }

        let displayKey = fullKey;
        if (fullKey.length > 20) {
          displayKey = fullKey.substring(0, 12) + "••••••••••••" + fullKey.substring(fullKey.length - 4);
        }

        html += `
          <tr>
            <td style="font-family:'Space Mono',monospace;">${escapeHtml(displayKey)}</td>
            <td><span class="status-dot ${statCls}"></span>${statLabel}</td>
            <td>${safeNumber(kStats.total_requests)}</td>
            <td>${safeNumber(kStats.total_errors)}</td>
            <td style="text-align:right"><button class="btn btn-sm del-key-btn" data-idx="${idx}" style="color:var(--error);border-color:transparent;background:transparent">✕</button></td>
          </tr>
        `;
      });
    }

    html += `
          </table>
        </div>
        
        <div style="margin-top:24px; border-top:1px solid var(--border); padding-top:16px;">
          <div class="form-group" style="margin-bottom:0">
            <label class="form-label">ADD NEW KEY</label>
            <div style="display:flex;gap:8px">
              <input id="new-key-val" class="form-input" placeholder="sk-..." style="flex:1">
              <button id="add-key-btn" class="btn btn-sm btn-primary">ADD KEY</button>
            </div>
          </div>
        </div>
      </div>
    `;

    c.innerHTML = html;

    $("#back-to-provs").addEventListener("click", () => renderKeys(c));

    $("#save-prov-url").addEventListener("click", async () => {
      const u = $("#prov-url").value.trim();
      if (!p.provider_base_urls) p.provider_base_urls = {};
      if (u) p.provider_base_urls[prov] = u;
      else delete p.provider_base_urls[prov];
      await saveConfig();

      const btn = $("#save-prov-url");
      btn.textContent = "SAVED";
      btn.style.borderColor = "var(--success)";
      btn.style.color = "var(--success)";
      setTimeout(() => {
        btn.textContent = "SAVE URL";
        btn.style.borderColor = "";
        btn.style.color = "";
      }, 2000);
    });

    $("#add-key-btn").addEventListener("click", async () => {
      const nk = $("#new-key-val").value.trim();
      if (nk) {
        if (!p.provider_keys) p.provider_keys = {};
        keysArray.push(nk);
        p.provider_keys[prov] = keysArray.join(" ; ");
        await saveConfig();
        renderProviderDetails(c, prov);
      }
    });

    c.querySelectorAll(".del-key-btn").forEach(btn => {
      btn.addEventListener("click", async () => {
        const idx = parseInt(btn.dataset.idx);
        keysArray.splice(idx, 1);
        if (keysArray.length > 0) {
          if (!p.provider_keys) p.provider_keys = {};
          p.provider_keys[prov] = keysArray.join(" ; ");
        } else if (p.provider_keys) {
          delete p.provider_keys[prov];
        }
        await saveConfig();
        renderProviderDetails(c, prov);
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
      <input class="form-input" type="number" id="tool-retry-max" value="${safeNumber(f.tool_retry_max || 2)}" min="0" max="10" style="width:100px">
    </div>`;
    html += `<div class="card"><div class="card-header"><div class="card-title">SYSTEM PROMPT OVERRIDE</div></div>
      <div class="form-group" style="margin-bottom:0">
        <textarea id="override-sys-prompt" class="form-textarea" rows="4" placeholder="Leave empty to use Claude's default system prompt.">${escapeHtml(f.override_system_prompt || "")}</textarea>
        <div class="form-hint" style="margin-top:8px">Overrides the default system prompt sent to the LLM. Applies globally to this profile.</div>
      </div>
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
    const sysPromptInput = $("#override-sys-prompt");
    if (sysPromptInput) sysPromptInput.addEventListener("change", () => { f.override_system_prompt = sysPromptInput.value.trim() || null; });
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
      <div class="card"><div class="card-header"><div class="card-title">API KEYS POOL ROUTING STRATEGY</div></div>
        <select class="form-select" id="r-key-strat">${strategies.map((s) => `<option value="${s}" ${r.key_strategy === s ? "selected" : ""}>${s.replace(/_/g, " ").toUpperCase()}</option>`).join("")}</select>
      </div>
      <div class="dashboard-grid">
        <div class="card"><div class="card-header"><div class="card-title">RATE LIMIT COOLDOWN (s)</div></div>
          <input class="form-input" type="number" id="r-cooldown" value="${safeNumber(r.rate_limit_cooldown)}" min="5">
        </div>
        <div class="card"><div class="card-header"><div class="card-title">MAX CONSECUTIVE ERRORS</div></div>
          <input class="form-input" type="number" id="r-maxerr" value="${safeNumber(r.max_consecutive_errors)}" min="1">
        </div>
        <div class="card"><div class="card-header"><div class="card-title">HEALTH RECOVERY (s)</div></div>
          <input class="form-input" type="number" id="r-recovery" value="${safeNumber(r.health_recovery_interval)}" min="10">
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
        if (config.profiles[key]) {
          alert("A profile with that name already exists!");
          return;
        }
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
      const safeKey = escapeHtml(key);
      const attrKey = escapeAttr(key);
      const safeName = escapeHtml(prof.name);
      html += `<tr>
        <td>${safeKey}</td><td>${safeName}</td>
        <td>${isActive ? '<span style="color:var(--success)">● ACTIVE</span>' : '<button class="btn btn-sm prof-activate" data-key="' + attrKey + '">ACTIVATE</button>'}</td>
        <td style="text-align:right">
          <button class="btn btn-sm prof-rename" data-key="${attrKey}" title="Rename Profile">✎ RENAME</button>
          <button class="btn btn-sm prof-duplicate" data-key="${attrKey}" title="Duplicate Profile">⧉ DUP</button>
          ${!isActive ? `<button class="btn btn-sm prof-delete" data-key="${attrKey}" style="color:var(--error);border-color:transparent" title="Delete Profile">✕</button>` : ""}
        </td>
      </tr>`;
    }
    html += "</table></div>";
    el.innerHTML = html;

    el.querySelectorAll(".prof-activate").forEach((btn) => {
      btn.addEventListener("click", async () => {
        await api("POST", "/profiles/" + encodeURIComponent(btn.dataset.key) + "/activate");
        await loadAll();
      });
    });

    el.querySelectorAll(".prof-delete").forEach((btn) => {
      btn.addEventListener("click", async () => {
        if (!confirm("Are you sure you want to delete this profile?")) return;
        await api("DELETE", "/profiles/" + encodeURIComponent(btn.dataset.key));
        await loadAll();
      });
    });

    el.querySelectorAll(".prof-rename").forEach((btn) => {
      btn.addEventListener("click", () => {
        const oldKey = btn.dataset.key;
        const prof = config.profiles[oldKey];
        
        const modalHtml = `
          <div class="form-group">
            <label class="form-label">NEW PROFILE NAME</label>
            <input id="modal-rename-name" class="form-input" value="${escapeAttr(prof.name)}">
          </div>
          <button id="modal-rename-save" class="btn btn-primary" style="width:100%">RENAME</button>
        `;

        openModal("Rename Profile", modalHtml, () => {
          $("#modal-rename-save").addEventListener("click", async () => {
            const newName = $("#modal-rename-name").value.trim();
            const newKey = newName.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '');
            
            if (!newName || !newKey) {
              alert("Name cannot be empty!");
              return;
            }

            if (newKey !== oldKey) {
              if (config.profiles[newKey]) {
                alert("A profile with that name/slug already exists!");
                return;
              }
              // Create new profile with copied data
              config.profiles[newKey] = JSON.parse(JSON.stringify(prof));
              config.profiles[newKey].name = newName;
              
              // Update active profile pointer if needed
              if (config.general.active_profile === oldKey) {
                config.general.active_profile = newKey;
              }
              
              // Remove old profile
              delete config.profiles[oldKey];
              
              closeModal();
              await saveConfig();
              await loadAll();
            } else if (newName !== prof.name) {
              // Just rename the display name (slug hasn't changed)
              config.profiles[oldKey].name = newName;
              closeModal();
              await saveConfig();
              await loadAll();
            } else {
              closeModal();
            }
          });
        });
      });
    });

    el.querySelectorAll(".prof-duplicate").forEach((btn) => {
      btn.addEventListener("click", () => {
        const srcKey = btn.dataset.key;
        const prof = config.profiles[srcKey];
        
        const modalHtml = `
          <div class="form-group">
            <label class="form-label">NEW PROFILE NAME</label>
            <input id="modal-dup-name" class="form-input" value="${escapeAttr((prof.name || "") + " (Copy)")}">
          </div>
          <button id="modal-dup-save" class="btn btn-primary" style="width:100%">DUPLICATE</button>
        `;

        openModal("Duplicate Profile", modalHtml, () => {
          $("#modal-dup-save").addEventListener("click", async () => {
            const newName = $("#modal-dup-name").value.trim();
            const newKey = newName.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '');
            
            if (!newName || !newKey) {
              alert("Name cannot be empty!");
              return;
            }

            if (config.profiles[newKey]) {
              alert("A profile with that name/slug already exists!");
              return;
            }

            config.profiles[newKey] = JSON.parse(JSON.stringify(prof));
            config.profiles[newKey].name = newName;
            
            closeModal();
            await saveConfig();
            await loadAll();
          });
        });
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
            <input class="form-input" type="number" id="s-ratelimit" value="${safeNumber(rl.provider_rate_limit)}"></div>
          <div class="form-group"><label class="form-label">WINDOW (SECONDS)</label>
            <input class="form-input" type="number" id="s-ratewindow" value="${safeNumber(rl.provider_rate_window)}"></div>
          <div class="form-group"><label class="form-label">MAX CONCURRENCY</label>
            <input class="form-input" type="number" id="s-maxconc" value="${safeNumber(rl.provider_max_concurrency)}"></div>
        </div>
      </div>
      <div class="card"><div class="card-header"><div class="card-title">TIMEOUTS</div></div>
        <div class="dashboard-grid">
          <div class="form-group"><label class="form-label">READ TIMEOUT (s)</label>
            <input class="form-input" type="number" id="s-readto" value="${safeNumber(to.http_read_timeout)}"></div>
          <div class="form-group"><label class="form-label">CONNECT TIMEOUT (s)</label>
            <input class="form-input" type="number" id="s-connto" value="${safeNumber(to.http_connect_timeout)}"></div>
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
      const lastPage = localStorage.getItem('crustoxy_page') || "dashboard";
      renderPage(lastPage, { force: true });
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

    // Auto-refresh status every 10s
    setInterval(async () => {
      try {
        status = await api("GET", "/status");
        updateStatusIndicator();
        if (currentPage === "dashboard" || currentPage === "keys") {
          rerenderCurrentPage();
        }
      } catch { /* ignore */ }
    }, 10000);
  }

  document.addEventListener("DOMContentLoaded", init);
})();
