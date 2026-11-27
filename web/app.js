const MEDIA_EXTS = new Set([
  "ogg", "fsb", "wav", "mp3",
  "png", "tga", "jpg", "jpeg", "hdr", "gif", "ktx",
]);
const MEDIA_HEADER_LEN = 4096;
const ARCHIVE_EXTS = new Set(["mcworld", "mcaddon", "mcpack", "zip", "mctemplate"]);

const state = {
  items: [], // { id, kind: 'archive'|'dir', role: 'archive'|'bp'|'rp'|'folder', name, file?, files?, handle? }
  vanillaFiles: [], // { path, buffer }
  vanillaLabel: null,
  lastResult: null,
  lastElapsedMs: 0,
  sevFilter: "all",
  packFilter: "all",
  search: "",
  wasmReady: false,
  running: false,
  pendingDirRole: "folder",
  deferredInstallPrompt: null,
};

let nextItemId = 1;

const $ = (id) => document.getElementById(id);

const els = {
  versionBadge: $("version-badge"),
  installBtn: $("install-btn"),
  themeBtn: $("theme-btn"),
  dropzone: $("dropzone"),
  pickArchivesBtn: $("pick-archives-btn"),
  pickBpBtn: $("pick-bp-btn"),
  pickRpBtn: $("pick-rp-btn"),
  pickFolderBtn: $("pick-folder-btn"),
  archivesInput: $("archives-input"),
  dirInput: $("dir-input"),
  queueWrap: $("queue-wrap"),
  queueSummary: $("queue-summary"),
  queueList: $("queue-list"),
  clearBtn: $("clear-btn"),
  pickVanillaBtn: $("pick-vanilla-btn"),
  clearVanillaBtn: $("clear-vanilla-btn"),
  vanillaInput: $("vanilla-input"),
  vanillaStatus: $("vanilla-status"),
  minSeverity: $("min-severity"),
  optLink: $("opt-link"),
  optLints: $("opt-lints"),
  validateBtn: $("validate-btn"),
  statusBar: $("status-bar"),
  statusText: $("status-text"),
  resultsSection: $("results-section"),
  summaryLine: $("summary-line"),
  revalidateBtn: $("revalidate-btn"),
  copyReportBtn: $("copy-report-btn"),
  fatalBox: $("fatal-box"),
  packsStrip: $("packs-strip"),
  packFilter: $("pack-filter"),
  searchInput: $("search-input"),
  diagnosticsList: $("diagnostics-list"),
  countAll: $("count-all"),
  countError: $("count-error"),
  countWarning: $("count-warning"),
  countInfo: $("count-info"),
};

// --- Theme & PWA ---

function initTheme() {
  const saved = localStorage.getItem("shulker-theme");
  if (saved === "light" || saved === "dark") {
    document.documentElement.dataset.theme = saved;
  }
}

els.themeBtn.addEventListener("click", () => {
  const current = document.documentElement.dataset.theme === "light" ? "light" : "dark";
  const next = current === "light" ? "dark" : "light";
  document.documentElement.dataset.theme = next;
  localStorage.setItem("shulker-theme", next);
});

window.addEventListener("beforeinstallprompt", (e) => {
  e.preventDefault();
  state.deferredInstallPrompt = e;
  els.installBtn.hidden = false;
});

els.installBtn.addEventListener("click", async () => {
  if (!state.deferredInstallPrompt) return;
  state.deferredInstallPrompt.prompt();
  await state.deferredInstallPrompt.userChoice;
  state.deferredInstallPrompt = null;
  els.installBtn.hidden = true;
});

if ("serviceWorker" in navigator) {
  navigator.serviceWorker.register("./sw.js").catch((err) => {
    console.warn("Service worker registration failed:", err);
  });
}

// --- Web Worker ---

const worker = new Worker("./worker.js", { type: "module" });

worker.addEventListener("message", (event) => {
  const msg = event.data;
  if (!msg) return;
  if (msg.type === "ready") {
    state.wasmReady = true;
    els.versionBadge.textContent = `v${msg.version} · WASM ready`;
    els.versionBadge.classList.add("ready");
    updateControls();
  } else if (msg.type === "init_error") {
    els.versionBadge.textContent = "WASM failed to load";
    showFatal(`Failed to initialize WebAssembly module: ${msg.message}`);
  } else if (msg.type === "status") {
    els.statusText.textContent = msg.text;
  } else if (msg.type === "result") {
    state.running = false;
    state.lastResult = msg.data;
    state.lastElapsedMs = msg.elapsedMs;
    els.statusBar.hidden = true;
    updateControls();
    renderResults();
  } else if (msg.type === "error") {
    state.running = false;
    els.statusBar.hidden = true;
    updateControls();
    showFatal(msg.message);
  }
});

function showFatal(message) {
  els.resultsSection.hidden = false;
  els.fatalBox.hidden = false;
  els.fatalBox.textContent = message;
  els.summaryLine.textContent = "Validation failed";
  els.summaryLine.className = "summary-line err";
}

// --- File / Folder Helpers ---

function getExt(name) {
  const idx = name.lastIndexOf(".");
  return idx >= 0 ? name.slice(idx + 1).toLowerCase() : "";
}

async function readFileForDir(file, path) {
  const ext = getExt(path);
  const blob =
    MEDIA_EXTS.has(ext) && file.size > MEDIA_HEADER_LEN
      ? file.slice(0, MEDIA_HEADER_LEN)
      : file;
  const buffer = await blob.arrayBuffer();
  return { path, buffer };
}

async function readDirectoryHandle(handle, prefix = "") {
  const entries = [];
  for await (const [name, child] of handle.entries()) {
    const rel = prefix ? `${prefix}/${name}` : name;
    if (child.kind === "file") {
      const file = await child.getFile();
      entries.push(await readFileForDir(file, rel));
    } else if (child.kind === "directory") {
      const sub = await readDirectoryHandle(child, rel);
      entries.push(...sub);
    }
  }
  return entries;
}

function readFileListAsDir(fileList) {
  const files = Array.from(fileList);
  if (files.length === 0) return null;
  const firstRel = files[0].webkitRelativePath || files[0].name;
  const rootName = firstRel.includes("/") ? firstRel.split("/")[0] : "Folder";
  return {
    name: rootName,
    files: files.map((file) => {
      const rel = file.webkitRelativePath || file.name;
      const prefix = `${rootName}/`;
      const stripped = rel.startsWith(prefix) ? rel.slice(prefix.length) : rel;
      return { file, path: stripped };
    }),
  };
}

async function readDataTransferEntry(entry, prefix = "") {
  if (entry.isFile) {
    const file = await new Promise((resolve, reject) => entry.file(resolve, reject));
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    return [{ file, path: rel }];
  }
  if (entry.isDirectory) {
    const reader = entry.createReader();
    const children = [];
    while (true) {
      const batch = await new Promise((resolve, reject) => reader.readEntries(resolve, reject));
      if (!batch || batch.length === 0) break;
      children.push(...batch);
    }
    const nextPrefix = prefix ? `${prefix}/${entry.name}` : entry.name;
    const nested = [];
    for (const child of children) {
      nested.push(...(await readDataTransferEntry(child, nextPrefix)));
    }
    return nested;
  }
  return [];
}

// --- Queue Management ---

function addArchiveFiles(fileList) {
  for (const file of Array.from(fileList)) {
    state.items.push({
      id: nextItemId++,
      kind: "archive",
      role: "archive",
      name: file.name,
      file,
      size: file.size,
    });
  }
  renderQueue();
}

async function pickDirectory(role) {
  if (typeof window.showDirectoryPicker === "function") {
    try {
      const handle = await window.showDirectoryPicker({ mode: "read" });
      state.items.push({
        id: nextItemId++,
        kind: "dir",
        role,
        name: handle.name,
        handle,
      });
      renderQueue();
      return;
    } catch (err) {
      if (err && err.name === "AbortError") return;
      console.warn("showDirectoryPicker failed, falling back to file input:", err);
    }
  }
  state.pendingDirRole = role;
  els.dirInput.value = "";
  els.dirInput.click();
}

function renderQueue() {
  els.queueList.innerHTML = "";
  els.queueWrap.hidden = state.items.length === 0;
  els.queueSummary.textContent = `${state.items.length} ${state.items.length === 1 ? "input" : "inputs"} queued`;

  for (const item of state.items) {
    const li = document.createElement("li");
    li.className = "queue-item";

    const main = document.createElement("div");
    main.className = "queue-item-main";

    const pill = document.createElement("span");
    pill.className = `pill ${rolePillClass(item.role)}`;
    pill.textContent = roleLabel(item.role);

    const name = document.createElement("span");
    name.className = "queue-name";
    name.textContent = item.name;
    name.title = item.name;

    const meta = document.createElement("span");
    meta.className = "queue-meta";
    if (item.kind === "archive") {
      meta.textContent = formatBytes(item.size);
    } else if (item.handle) {
      meta.textContent = "Live folder";
    } else if (item.files) {
      meta.textContent = `${item.files.length} files`;
    }

    main.append(pill, name, meta);

    const removeBtn = document.createElement("button");
    removeBtn.type = "button";
    removeBtn.className = "remove-btn";
    removeBtn.setAttribute("aria-label", `Remove ${item.name}`);
    removeBtn.textContent = "×";
    removeBtn.addEventListener("click", () => {
      state.items = state.items.filter((x) => x.id !== item.id);
      renderQueue();
    });

    li.append(main, removeBtn);
    els.queueList.appendChild(li);
  }
  updateControls();
}

function roleLabel(role) {
  switch (role) {
    case "bp": return "BP";
    case "rp": return "RP";
    case "archive": return "Archive";
    default: return "Folder";
  }
}

function rolePillClass(role) {
  switch (role) {
    case "bp": return "pill-bp";
    case "rp": return "pill-rp";
    case "archive": return "pill-archive";
    default: return "pill-folder";
  }
}

function formatBytes(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MiB`;
}

function updateControls() {
  els.validateBtn.disabled = !state.wasmReady || state.running || state.items.length === 0;
  els.revalidateBtn.disabled = !state.wasmReady || state.running || state.items.length === 0;
}

// --- Event Wiring ---

els.pickArchivesBtn.addEventListener("click", () => {
  els.archivesInput.value = "";
  els.archivesInput.click();
});

els.archivesInput.addEventListener("change", () => {
  if (els.archivesInput.files) addArchiveFiles(els.archivesInput.files);
});

els.pickBpBtn.addEventListener("click", () => pickDirectory("bp"));
els.pickRpBtn.addEventListener("click", () => pickDirectory("rp"));
els.pickFolderBtn.addEventListener("click", () => pickDirectory("folder"));

els.dirInput.addEventListener("change", () => {
  const parsed = readFileListAsDir(els.dirInput.files);
  if (!parsed) return;
  state.items.push({
    id: nextItemId++,
    kind: "dir",
    role: state.pendingDirRole,
    name: parsed.name,
    files: parsed.files,
  });
  renderQueue();
});

els.clearBtn.addEventListener("click", () => {
  state.items = [];
  renderQueue();
});

// Drag & drop for files and folders
els.dropzone.addEventListener("dragover", (e) => {
  e.preventDefault();
  els.dropzone.classList.add("dragover");
});

els.dropzone.addEventListener("dragleave", () => {
  els.dropzone.classList.remove("dragover");
});

els.dropzone.addEventListener("drop", async (e) => {
  e.preventDefault();
  els.dropzone.classList.remove("dragover");

  const items = e.dataTransfer?.items;
  if (items && items.length > 0 && typeof items[0].webkitGetAsEntry === "function") {
    const entries = Array.from(items)
      .map((item) => item.webkitGetAsEntry())
      .filter(Boolean);

    for (const entry of entries) {
      if (entry.isDirectory) {
        const files = await readDataTransferEntry(entry, "");
        const prefix = `${entry.name}/`;
        state.items.push({
          id: nextItemId++,
          kind: "dir",
          role: "folder",
          name: entry.name,
          files: files.map((f) => ({
            file: f.file,
            path: f.path.startsWith(prefix) ? f.path.slice(prefix.length) : f.path,
          })),
        });
      } else if (entry.isFile) {
        const file = await new Promise((resolve, reject) => entry.file(resolve, reject));
        if (ARCHIVE_EXTS.has(getExt(file.name)) || file.name) {
          addArchiveFiles([file]);
        }
      }
    }
    renderQueue();
    return;
  }

  if (e.dataTransfer?.files?.length) {
    addArchiveFiles(e.dataTransfer.files);
  }
});

// Vanilla data selection
els.pickVanillaBtn.addEventListener("click", async () => {
  if (typeof window.showDirectoryPicker === "function") {
    try {
      const handle = await window.showDirectoryPicker({ mode: "read" });
      await loadVanillaFromEntries(handle.name, await readDirectoryHandle(handle));
      return;
    } catch (err) {
      if (err && err.name === "AbortError") return;
    }
  }
  els.vanillaInput.value = "";
  els.vanillaInput.click();
});

els.vanillaInput.addEventListener("change", async () => {
  const parsed = readFileListAsDir(els.vanillaInput.files);
  if (!parsed) return;
  const loaded = [];
  for (const item of parsed.files) {
    loaded.push(await readFileForDir(item.file, item.path));
  }
  await loadVanillaFromEntries(parsed.name, loaded);
});

async function loadVanillaFromEntries(label, entries) {
  const kept = entries.filter(
    (entry) =>
      entry.path.startsWith("behavior_packs/") ||
      entry.path.startsWith("resource_packs/") ||
      entry.path.startsWith("definitions/") ||
      entry.path.startsWith("credits/")
  );
  state.vanillaFiles = kept;
  state.vanillaLabel = label;
  els.vanillaStatus.textContent = `${kept.length} files (${label})`;
  els.vanillaStatus.className = "pill pill-ok";
  els.clearVanillaBtn.hidden = false;
}

els.clearVanillaBtn.addEventListener("click", () => {
  state.vanillaFiles = [];
  state.vanillaLabel = null;
  els.vanillaStatus.textContent = "Not loaded";
  els.vanillaStatus.className = "pill pill-muted";
  els.clearVanillaBtn.hidden = true;
});

// --- Run Validation ---

async function runValidation() {
  if (!state.wasmReady || state.running || state.items.length === 0) return;

  state.running = true;
  updateControls();
  els.statusBar.hidden = false;
  els.statusText.textContent = "Reading selected files…";

  try {
    const archives = [];
    const dirs = [];
    const transferables = [];

    for (const item of state.items) {
      if (item.kind === "archive") {
        const buffer = await item.file.arrayBuffer();
        archives.push({ name: item.name, buffer });
        transferables.push(buffer);
      } else if (item.handle) {
        const files = await readDirectoryHandle(item.handle);
        for (const f of files) transferables.push(f.buffer);
        dirs.push({ name: item.name, files });
      } else if (item.files) {
        const files = [];
        for (const f of item.files) {
          const entry = await readFileForDir(f.file, f.path);
          transferables.push(entry.buffer);
          files.push(entry);
        }
        dirs.push({ name: item.name, files });
      }
    }

    // Clone vanilla buffers so they remain cached in the main thread across re-validations.
    const vanillaFiles = state.vanillaFiles.map((vf) => {
      const copy = vf.buffer.slice(0);
      transferables.push(copy);
      return { path: vf.path, buffer: copy };
    });

    const options = {
      min_severity: els.minSeverity.value,
      no_link: !els.optLink.checked,
      no_lints: !els.optLints.checked,
    };

    worker.postMessage(
      { type: "validate", archives, dirs, vanillaFiles, options },
      transferables
    );
  } catch (err) {
    state.running = false;
    els.statusBar.hidden = true;
    updateControls();
    showFatal(`Failed to read input files: ${err instanceof Error ? err.message : String(err)}`);
  }
}

els.validateBtn.addEventListener("click", runValidation);
els.revalidateBtn.addEventListener("click", runValidation);

// --- Render Results ---

function renderResults() {
  const res = state.lastResult;
  if (!res) return;

  els.resultsSection.hidden = false;

  if (res.fatal_error) {
    els.fatalBox.hidden = false;
    els.fatalBox.textContent = res.fatal_error;
  } else {
    els.fatalBox.hidden = true;
  }

  const { errors, warnings, infos } = res.tally;
  const total = res.diagnostics.length;
  els.countAll.textContent = String(total);
  els.countError.textContent = String(errors);
  els.countWarning.textContent = String(warnings);
  els.countInfo.textContent = String(infos);

  const packWord = res.packs.length === 1 ? "pack" : "packs";
  const vanillaNote = res.vanilla_pack_count > 0 ? ` on ${res.vanilla_pack_count} vanilla packs` : "";
  const timeNote = ` (${(state.lastElapsedMs / 1000).toFixed(2)}s)`;

  if (res.fatal_error) {
    els.summaryLine.textContent = `Could not complete validation${timeNote}`;
    els.summaryLine.className = "summary-line err";
  } else if (errors === 0 && warnings === 0 && infos === 0) {
    els.summaryLine.textContent = `Checked ${res.packs.length} ${packWord}${vanillaNote}: no problems found${timeNote}`;
    els.summaryLine.className = "summary-line ok";
  } else {
    const parts = [];
    if (errors > 0) parts.push(`${errors} ${errors === 1 ? "error" : "errors"}`);
    if (warnings > 0) parts.push(`${warnings} ${warnings === 1 ? "warning" : "warnings"}`);
    if (infos > 0) parts.push(`${infos} info`);
    els.summaryLine.textContent = `Checked ${res.packs.length} ${packWord}${vanillaNote}: ${parts.join(", ")}${timeNote}`;
    els.summaryLine.className = `summary-line ${errors > 0 ? "err" : warnings > 0 ? "warn" : "ok"}`;
  }

  // Packs strip + pack filter
  els.packsStrip.innerHTML = "";
  els.packFilter.innerHTML = `<option value="all">All packs</option>`;
  if (res.packs.length > 0) {
    els.packsStrip.hidden = false;
    const seenNames = new Set();
    for (const pack of res.packs) {
      const chip = document.createElement("div");
      chip.className = "pack-chip";

      const badge = document.createElement("span");
      badge.className = `pill ${
        pack.pack_type === "behavior" ? "pill-bp" : pack.pack_type === "resource" ? "pill-rp" : "pill-folder"
      }`;
      badge.textContent = pack.pack_type === "behavior" ? "BP" : pack.pack_type === "resource" ? "RP" : "Pack";

      const label = document.createElement("span");
      label.textContent = `${pack.name} v${pack.version}`;

      chip.append(badge, label);
      els.packsStrip.appendChild(chip);

      if (!seenNames.has(pack.name)) {
        seenNames.add(pack.name);
        const opt = document.createElement("option");
        opt.value = pack.name;
        opt.textContent = pack.name;
        els.packFilter.appendChild(opt);
      }
    }
  } else {
    els.packsStrip.hidden = true;
  }

  if (state.packFilter !== "all" && !Array.from(els.packFilter.options).some((o) => o.value === state.packFilter)) {
    state.packFilter = "all";
  }
  els.packFilter.value = state.packFilter;

  renderDiagnostics();
}

function renderDiagnostics() {
  const res = state.lastResult;
  if (!res) return;

  const q = state.search.trim().toLowerCase();
  const filtered = res.diagnostics.filter((item) => {
    if (state.sevFilter !== "all") {
      if (state.sevFilter === "info") {
        if (item.severity !== "info" && item.severity !== "debug") return false;
      } else if (item.severity !== state.sevFilter) {
        return false;
      }
    }
    if (state.packFilter !== "all" && item.pack_name !== state.packFilter) {
      return false;
    }
    if (q) {
      const hay = `${item.code} ${item.title} ${item.file || ""} ${item.pack_name || ""} ${item.snippet}`.toLowerCase();
      if (!hay.includes(q)) return false;
    }
    return true;
  });

  els.diagnosticsList.innerHTML = "";
  if (filtered.length === 0) {
    const empty = document.createElement("div");
    empty.className = "empty-state";
    empty.textContent =
      res.diagnostics.length === 0
        ? "No diagnostics reported at this severity level."
        : "No diagnostics match the current filter.";
    els.diagnosticsList.appendChild(empty);
    return;
  }

  for (const item of filtered) {
    const card = document.createElement("article");
    card.className = `diag-card sev-${item.severity}`;

    const head = document.createElement("div");
    head.className = "diag-head";

    const sev = document.createElement("span");
    sev.className = `sev-tag ${item.severity}`;
    sev.textContent = item.severity;

    const code = document.createElement("code");
    code.className = "diag-code";
    code.textContent = item.code;

    const title = document.createElement("span");
    title.className = "diag-title";
    title.textContent = item.title;

    head.append(sev, code, title);
    card.appendChild(head);

    if (item.file || item.pack_name || item.json_path) {
      const meta = document.createElement("div");
      meta.className = "diag-meta";
      if (item.file) {
        const loc = document.createElement("span");
        const suffix = item.line ? `:${item.line}${item.column ? `:${item.column}` : ""}` : "";
        loc.textContent = `📄 ${item.file}${suffix}`;
        meta.appendChild(loc);
      }
      if (item.pack_name) {
        const p = document.createElement("span");
        p.textContent = `📦 ${item.pack_name}`;
        meta.appendChild(p);
      }
      if (item.json_path) {
        const jp = document.createElement("span");
        jp.textContent = `at ${item.json_path}`;
        meta.appendChild(jp);
      }
      card.appendChild(meta);
    }

    if (item.snippet) {
      const pre = document.createElement("pre");
      pre.className = "diag-snippet";
      pre.textContent = item.snippet;
      card.appendChild(pre);
    }

    if (item.where_shown) {
      const note = document.createElement("div");
      note.className = "diag-note";
      note.textContent = `Note: ${item.where_shown}`;
      card.appendChild(note);
    }

    if (item.hints && item.hints.length > 0) {
      const ul = document.createElement("ul");
      ul.className = "diag-hints";
      for (const hint of item.hints) {
        const li = document.createElement("li");
        li.textContent = hint;
        ul.appendChild(li);
      }
      card.appendChild(ul);
    }

    els.diagnosticsList.appendChild(card);
  }
}

document.querySelectorAll(".tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((t) => t.classList.remove("active"));
    tab.classList.add("active");
    state.sevFilter = tab.dataset.sev || "all";
    renderDiagnostics();
  });
});

els.packFilter.addEventListener("change", () => {
  state.packFilter = els.packFilter.value;
  renderDiagnostics();
});

els.searchInput.addEventListener("input", () => {
  state.search = els.searchInput.value;
  renderDiagnostics();
});

els.copyReportBtn.addEventListener("click", async () => {
  if (!state.lastResult?.plain_report) return;
  try {
    await navigator.clipboard.writeText(state.lastResult.plain_report);
    const prev = els.copyReportBtn.textContent;
    els.copyReportBtn.textContent = "Copied!";
    setTimeout(() => {
      els.copyReportBtn.textContent = prev;
    }, 1500);
  } catch {
    // fallback ignored
  }
});

initTheme();
