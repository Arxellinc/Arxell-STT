import { archiveTranscript, clearHistory, loadDraft, loadHistory, saveDraft, trimHistory } from "./history.js";
import { ShortcutRecorder } from "./shortcut.js";
import { PermissionSetup } from "./setup.js";

const $ = (selector) => document.querySelector(selector);
const invoke = window.__TAURI__?.core?.invoke;

// Follow the desktop light/dark preference, including live changes.
const darkPreference = window.matchMedia("(prefers-color-scheme: dark)");
function setTheme(theme) {
  document.documentElement.dataset.theme = theme;
}
setTheme(darkPreference.matches ? "dark" : "light");
darkPreference.addEventListener("change", (event) => setTheme(event.matches ? "dark" : "light"));
(async () => {
  const windowApi = window.__TAURI__?.window?.getCurrentWindow?.();
  if (!windowApi) return;
  try {
    const theme = await windowApi.theme();
    if (theme) setTheme(theme);
    await windowApi.onThemeChanged(({ payload }) => setTheme(payload));
  } catch {}
})();
const transcript = $("#transcript-text");
let configuration;
let initializing = true;
let active = false;
let pending = false;
let sessionId = 0;
let revision = 0;
let downloading = false;
let desktopAvailable = false;
let appBuild = "";
let modelState = "missing";
let modelCatalogue = [];
let resumePrefix = "";
let resumeOriginal = "";
let saveTimer = 0;
let saveChain = Promise.resolve();
let settingsDirty = false;
let shortcutRecorder;
let permissionSetup;
let restartPending = false;
let restartInProgress = false;
let displayedSessionId = 0;
let resumingCurrent = false;
let historyCount = 50;
let historyVisible = true;
let historyChain = Promise.resolve();
let draftTimer = 0;
let historyRender = 0;
const expandedHistory = new Set();

// Settings sections follow the Ubuntu Settings layout: title on the left,
// a rounded card of rows, and an optional advanced drawer per section.
const SECTIONS = [
  {
    id: "dictation",
    title: "Dictation",
    rows: [
      { path: "microphone", label: "Microphone", options: [["", "System default"]] },
      { path: "advanced.backend", label: "Processing", options: [["Auto", "Auto · GPU preferred"], ["CpuOnly", "CPU only"], ["GpuOnly", "GPU only"]] },
      { path: "advanced.stream_preset", label: "Streaming preset", options: [["efficient", "Efficient · 1.2 s lookahead"], ["responsive", "Responsive · 0.72 s lookahead"], ["accurate", "Accurate · 2.1 s lookahead"]] },
      { path: "advanced.vad_gating", label: "VAD gating (decode speech only)", type: "checkbox" },
      { path: "advanced.cues_enabled", label: "Spoken controls", type: "checkbox" },
    ],
    advanced: [
      { path: "advanced.cue.end_phrase", label: "Finish phrase" },
      { path: "advanced.cue.submit_phrase", label: "Finish + submit phrase" },
      { path: "advanced.cue.cancel_phrase", label: "Cancel phrase" },
      { path: "advanced.cue.pre_gap_ms", label: "Pause before cue (ms)", type: "number", min: 1, max: 119999, step: 1 },
      { path: "advanced.cue.confirm_ms", label: "Confirmation pause (ms)", type: "number", min: 2, max: 119999, step: 1 },
      { path: "advanced.cue.text_grace_ms", label: "Cue recognition grace (ms)", type: "number", min: 0, max: 5000, step: 1 },
      { path: "advanced.cue.inactivity_ms", label: "Inactivity timeout (ms)", type: "number", min: 3, max: 120000, step: 1 },
      { path: "advanced.cue.max_cue_ms", label: "Maximum cue duration (ms)", type: "number", min: 1, max: 10000, step: 1 },
      { path: "advanced.vad_threshold", label: "Earshot speech threshold", type: "number", min: .05, max: .95, step: .05 },
      { path: "advanced.speech_hold_ms", label: "Speech hold (ms; adds pause delay)", type: "number", min: 0, max: 200, step: 16 },
      { path: "advanced.max_session_seconds", label: "Maximum session (seconds)", type: "number", min: 10, max: 3600, step: 1 },
      { path: "advanced.diagnostics", label: "Metadata diagnostics (console only)", type: "checkbox" },
    ],
  },
  {
    id: "history",
    title: "Transcription history",
    rows: [
      { path: "history_limit", label: "Historical transcriptions to keep", options: [["3", "3"], ["5", "5"], ["10", "10"], ["20", "20"], ["100", "100"], ["0", "Unlimited"]] },
      { custom: "clear-history" },
      { note: "Stored locally on this device. Hiding history does not stop saving it. Reducing this limit permanently removes older entries. Unlimited history uses additional disk space." },
    ],
  },
  {
    id: "model",
    title: "Model",
    rows: [
      { custom: "model-select" },
      { custom: "install" },
      { custom: "file" },
    ],
    advanced: [
      { path: "model_path", label: "Model file path" },
      { path: "advanced.idle_unload_seconds", label: "Unload after idle (seconds; 0 = never)", type: "number", min: 0, max: 3600, step: 1 },
    ],
  },
  {
    id: "shortcuts",
    title: "Shortcuts & delivery",
    rows: [
      { path: "shortcut", label: "Dictation shortcut", placeholder: "Not set" },
      { custom: "shortcut-permission" },
      { path: "cancel_key", label: "Cancel key (global where supported)", options: [["Escape", "Escape"], ["Control+Escape", "Ctrl+Escape"], ["Alt+Escape", "Alt+Escape"]] },
      { path: "advanced.desktop_enabled", label: "Stream dictation into other applications", type: "checkbox" },
      { path: "advanced.terminal_enabled", label: "Allow typing into terminals", type: "checkbox" },
      { path: "advanced.x11_keyboard_enabled", label: "X11 keyboard delivery (no system permission exists)", type: "checkbox" },
      { custom: "desktop-permission" },
    ],
  },
  {
    id: "api",
    title: "Local API",
    rows: [{ path: "advanced.api_enabled", label: "Enable local API", type: "checkbox" }],
    advanced: [
      { path: "advanced.api_port", label: "Port", type: "number", min: 1024, max: 65535, step: 1 },
      { path: "advanced.api_max_seconds", label: "Maximum audio per request (seconds)", type: "number", min: 1, max: 600, step: 1 },
      { custom: "api-token" },
      { note: "Loopback only. Clients send audio; they cannot control desktop input. Token changes apply when saved." },
    ],
  },
];

// Brief hover descriptions for each setting title.
const TIPS = {
  microphone: "Audio input device used for dictation.",
  "advanced.backend": "Auto prefers the GPU and falls back to the CPU once at load.",
  "advanced.stream_preset": "Efficient uses a responsive first utterance, then larger chunks to save compute. Responsive keeps the smaller chunks throughout.",
  "advanced.vad_gating": "Decode only speech. Saves compute and finalizes text when you pause.",
  "advanced.cues_enabled": "Use pause-delimited words such as over, out and cancel.",
  "advanced.cue.end_phrase": "Saying this phrase alone ends dictation without submitting.",
  "advanced.cue.submit_phrase": "Saying this phrase ends dictation and activates the field.",
  "advanced.cue.cancel_phrase": "Saying this phrase discards the session.",
  "advanced.cue.pre_gap_ms": "Silence required before a cue word is treated as a control.",
  "advanced.cue.confirm_ms": "Silence after a cue before its action is applied.",
  "advanced.cue.text_grace_ms": "How long to wait for recognition before giving up on a cue.",
  "advanced.cue.inactivity_ms": "End the session after this much silence.",
  "advanced.cue.max_cue_ms": "Longer phrases are dictated as text, not treated as cues.",
  "advanced.vad_threshold": "Higher values ignore more background noise but may clip quiet speech.",
  "advanced.speech_hold_ms": "Keep speech active briefly after the last detected voice frame.",
  "advanced.max_session_seconds": "Hard limit for a single dictation session.",
  "advanced.diagnostics": "Print timing and resource details to the console.",
  "model-select": "Choose the transcription model used for dictation.",
  install: "Download the selected model with verification and cancel.",
  file: "Verify the model hash or unload it from memory.",
  model_path: "Path to the verified Parakeet GGUF file.",
  "advanced.idle_unload_seconds": "Unload the model after inactivity. 0 keeps it loaded.",
  shortcut: "Press to start dictation from another app. It stops automatically after silence; press again to finish early. Leave blank to disable.",
  cancel_key: "Key to cancel the current session.",
  "advanced.desktop_enabled": "Stream committed words into one verified external text field. Focus discovery does not block recording.",
  "advanced.terminal_enabled": "Allow Unicode typing into a verified terminal. Never sends Enter, Escape, Tab or other control keys.",
  "advanced.x11_keyboard_enabled": "X11 sessions cannot show a system permission dialog. Enable to let Arxell type into other applications on X11 using the standard XTest mechanism; focus is verified before each chunk.",
  "advanced.submit_enabled": "Allow activation of the field after a submit cue.",
  "advanced.submit_key": "Action used to submit after inserting text.",
  "advanced.api_enabled": "Serve local transcription requests over loopback.",
  "advanced.api_port": "Loopback port for the local API.",
  "advanced.api_max_seconds": "Maximum audio accepted per API request.",
  "api-token": "Bearer token required by local API clients.",
};

function get(object, path) { return path.split(".").reduce((value, key) => value[key], object); }
function set(object, path, value) {
  const parts = path.split(".");
  const key = parts.pop();
  parts.reduce((value, key) => value[key], object)[key] = value;
}
function field(path) { return document.getElementById(`setting-${path}`); }

function controlRow(entry) {
  const row = document.createElement("div");
  row.className = "setting-row";

  if (entry.note) {
    const note = document.createElement("p");
    note.className = "card-note";
    note.innerHTML = entry.note;
    row.appendChild(note);
    return row;
  }
  if (entry.custom === "shortcut-permission") {
    row.id = "shortcut-permission-row";
    const label = document.createElement("span");
    label.textContent = "Global shortcut permission";
    const button = document.createElement("button");
    button.type = "button";
    button.id = "shortcut-permission";
    button.textContent = "Authorize / retry";
    button.addEventListener("click", () => action(async () => {
      await flushSettings();
      await invoke("reauthorize_shortcut");
      message("Check the system shortcut permission dialog if one appears.");
    }));
    row.append(label, button);
    return row;
  }
  if (entry.custom === "desktop-permission") {
    const label = document.createElement("span");
    label.textContent = "System keyboard access";
    const actions = document.createElement("span");
    actions.className = "inline";
    actions.innerHTML = '<span id="desktop-keyboard-status" role="status"></span><button type="button" id="desktop-permission">Grant access</button><button type="button" id="desktop-refresh">Refresh status</button>';
    row.append(label, actions);
    return row;
  }
  if (entry.custom === "clear-history") {
    const label = document.createElement("span");
    label.textContent = "Saved history";
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = "Clear history";
    button.addEventListener("click", () => {
      if (!window.confirm("Permanently delete historical transcriptions? The current transcript will be kept.")) return;
      queueHistory(async () => {
        await clearHistory();
        expandedHistory.clear();
        historyCount = 50;
        await renderHistory();
        message("Transcription history cleared.");
      });
    });
    row.append(label, button);
    return row;
  }
  if (entry.custom === "model-select") {
    const label = document.createElement("label");
    label.htmlFor = "model-choice";
    label.textContent = "Model";
    label.dataset.tip = TIPS["model-select"];
    const select = document.createElement("select");
    select.id = "model-choice";
    row.append(label, select);
    return row;
  }
  if (entry.custom === "install") {
    row.id = "model-install-row";
    row.hidden = true;
    const label = document.createElement("span");
    label.textContent = "Model installation";
    label.dataset.tip = TIPS.install;
    const actions = document.createElement("span");
    actions.className = "inline";
    actions.innerHTML = '<button type="button" id="download" class="accent-button">Download now</button><button type="button" id="cancel-download" hidden>Cancel</button>';
    row.append(label, actions);
    return row;
  }
  if (entry.custom === "file") {
    const label = document.createElement("span");
    label.textContent = "Model file";
    label.dataset.tip = TIPS.file;
    const actions = document.createElement("span");
    actions.className = "inline";
    actions.innerHTML = '<button type="button" id="verify">Verify</button><button type="button" id="unload">Unload</button>';
    row.append(label, actions);
    return row;
  }
  if (entry.custom === "api-token") {
    const label = document.createElement("span");
    label.textContent = "Access token";
    label.dataset.tip = TIPS["api-token"];
    const actions = document.createElement("span");
    actions.className = "inline";
    actions.innerHTML = '<button type="button" id="copy-token">Copy token</button><button type="button" id="regenerate-token">Regenerate</button>';
    row.append(label, actions);
    return row;
  }

  const label = document.createElement("label");
  label.htmlFor = `setting-${entry.path}`;
  label.textContent = entry.label;
  if (TIPS[entry.path]) label.dataset.tip = TIPS[entry.path];
  const control = document.createElement(entry.options ? "select" : "input");
  control.id = label.htmlFor;
  if (entry.options) {
    for (const [value, name] of entry.options) control.add(new Option(name, value));
  } else {
    control.type = entry.type ?? "text";
    if (control.type === "number") {
      Object.assign(control, { min: entry.min, max: entry.max, step: entry.step, required: true });
    }
    if (control.type === "text") {
      control.autocomplete = "off";
      control.spellcheck = false;
      if (entry.placeholder) control.placeholder = entry.placeholder;
    }
  }
  if (entry.path === "shortcut") {
    row.classList.add("shortcut-row");
    const actions = document.createElement("div");
    actions.className = "shortcut-controls";
    const button = document.createElement("button");
    button.type = "button";
    button.id = "record-shortcut";
    button.textContent = "Record shortcut";
    button.setAttribute("aria-pressed", "false");
    button.setAttribute("aria-describedby", "shortcut-hint");
    control.setAttribute("aria-describedby", "shortcut-hint");
    actions.append(control, button);
    const hint = document.createElement("p");
    hint.id = "shortcut-hint";
    hint.className = "shortcut-hint";
    hint.setAttribute("role", "status");
    hint.textContent = "Click Record shortcut, press your combination, then click Stop recording to set it. Escape cancels. Clear the field to disable.";
    row.append(label, actions, hint);
  } else row.append(label, control);
  return row;
}

function renderSettings() {
  const host = $("#settings-groups");
  for (const section of SECTIONS) {
    const group = document.createElement("section");
    group.className = "group";
    group.id = `group-${section.id}`;

    const head = document.createElement("div");
    head.className = "group-head";
    const title = document.createElement("h2");
    title.textContent = section.title;
    head.append(title);

    const card = document.createElement("div");
    card.className = "card";
    const basic = document.createElement("div");
    basic.className = "basic-rows";
    for (const entry of section.rows) basic.append(controlRow(entry));
    card.append(basic);

    if (section.advanced?.length) {
      const drawer = document.createElement("div");
      drawer.className = "advanced-rows";
      drawer.hidden = true;
      for (const entry of section.advanced) drawer.append(controlRow(entry));
      const toggle = document.createElement("button");
      toggle.type = "button";
      toggle.className = "advanced-toggle";
      toggle.setAttribute("aria-expanded", "false");
      toggle.innerHTML = 'Show Advanced <svg class="chevron" viewBox="0 0 12 12" width="12" height="12" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" d="m3 4.5 3 3 3-3"/></svg>';
      toggle.addEventListener("click", () => {
        const open = drawer.hidden;
        drawer.hidden = !open;
        toggle.setAttribute("aria-expanded", String(open));
        toggle.firstChild.textContent = open ? "Hide Advanced " : "Show Advanced ";
      });
      head.append(toggle);
      card.append(drawer);
    }
    group.append(head, card);
    host.append(group);
  }
}
renderSettings();

// One lightweight tooltip reused by every setting title.
const tooltip = document.createElement("div");
tooltip.className = "tooltip";
tooltip.hidden = true;
document.body.append(tooltip);
function showTip(target) {
  const text = target.dataset.tip;
  if (!text) return;
  tooltip.textContent = text;
  tooltip.hidden = false;
  const rect = target.getBoundingClientRect();
  tooltip.style.left = `${Math.max(8, Math.min(rect.left, window.innerWidth - tooltip.offsetWidth - 8))}px`;
  tooltip.style.top = `${rect.bottom + 6}px`;
}
document.addEventListener("mouseover", (event) => {
  const target = event.target.closest?.("[data-tip]");
  if (target) showTip(target);
});
document.addEventListener("mouseout", (event) => {
  if (event.target.closest?.("[data-tip]")) tooltip.hidden = true;
});
window.addEventListener("scroll", () => { tooltip.hidden = true; }, true);

const ALL_ROWS = SECTIONS.flatMap((section) => [...section.rows, ...(section.advanced ?? [])]).filter((entry) => entry.path);


function message(text = "", error = false) {
  const element = $("#message");
  element.textContent = text;
  element.title = text;
  element.hidden = !text;
  element.classList.toggle("error", error);
}
function state(label, kind = "") {
  const light = $("#state-label");
  const text = $("#state-text");
  if (text) text.textContent = label;
  const tone = kind || (/loading|starting|stopping|finishing|cancell|preview/i.test(label)
    ? "warn"
    : /no model|not ready|error|setup|failed/i.test(label) ? "bad" : "ok");
  light.className = `status-light ${tone}`;
  light.title = label;
  light.setAttribute("aria-label", label);
}

// Ready means the model AND every enabled access requirement are verified.
function refreshStatus() {
  if (active) return; // a live session owns the header
  if (!permissionSetup?.ready) { state("Setup required", "warn"); return; }
  if (modelState === "ready") state(configuration?.advanced.desktop_enabled ? "Ready" : "Ready · in-app only", "ok");
  else if (modelState === "loading") state("Loading model");
  else state("No model loaded", "bad");
}

let modelHint = false;
function applyModelState(next, name) {
  loadedModel(name);
  modelState = next || "missing";
  if ((modelState === "missing" || modelState === "error") && !restartPending && !restartInProgress) {
    message(modelState === "missing"
      ? "No model loaded. Use Download or Verify in Settings."
      : "Model failed to load. Check the file path in Settings.", true);
    modelHint = true;
  } else if (modelHint && !restartPending && !restartInProgress) {
    message();
    modelHint = false;
  }
  refreshStatus();
  controls();
}
function resizeTranscript() {
  if ($("#dictation").hidden) return;
  const following = transcript.scrollHeight - transcript.scrollTop - transcript.clientHeight < 30;
  transcript.style.height = "auto";
  const available = $("#dictation").clientHeight;
  const maximum = Math.max(93, available * (historyVisible ? .5 : 1) - 76);
  transcript.style.height = `${Math.min(transcript.scrollHeight, maximum)}px`;
  if (active && following) transcript.scrollTop = transcript.scrollHeight;
}

function applyTranscript(text) {
  const next = typeof text === "string" ? text : "";
  if (resumePrefix) {
    const addition = next.replace(/^\s+/, "");
    transcript.value = addition ? `${resumePrefix}${addition}` : resumePrefix;
  } else {
    transcript.value = next;
  }
  resizeTranscript();
}

function controls() {
  const record = $("#dictate");
  const canStart = Boolean(permissionSetup?.ready && modelState === "ready");
  record.disabled = !invoke || !configuration || initializing || pending || Boolean(shortcutRecorder?.locked) || (!active && !canStart);
  record.classList.toggle("active", active);
  record.setAttribute("aria-pressed", String(active));
  const label = active ? "Stop dictation" : "Start dictation";
  record.setAttribute("aria-label", label);
  record.title = label;
  $("#cancel").hidden = !active;
  $("#cancel").title = configuration?.advanced.desktop_enabled ? "Cancel: already typed desktop text will remain" : "Cancel dictation";
  const hasText = Boolean(transcript.value.trim());
  $("#copy").disabled = !hasText;
  $("#save").disabled = !hasText;
  const resume = $("#resume");
  resume.hidden = false;
  resume.disabled = !hasText || active || pending || initializing || !invoke || !configuration || !canStart;
  document.querySelectorAll(".history-resume").forEach((button) => {
    button.disabled = active || pending || initializing || !invoke || !configuration || !canStart;
  });
  transcript.readOnly = active;
  $("#settings-fields").disabled = active || initializing || !configuration || Boolean(permissionSetup?.busy);
  $("#download").disabled = downloading;
  $("#cancel-download").hidden = !downloading;
  field("advanced.desktop_enabled").disabled = !desktopAvailable;
  shortcutRecorder?.sync();
}

function stampTranscript(date = new Date().toISOString()) {
  const time = $("#timestamp");
  if (!time) return;
  const now = new Date(date);
  const two = (value) => String(value).padStart(2, "0");
  time.textContent = `${two(now.getMonth() + 1)}/${two(now.getDate())} ${two(now.getHours())}:${two(now.getMinutes())}`;
  time.dateTime = now.toISOString();
}

function loadedModel(name) {
  const label = name || "No model loaded";
  $("#loaded-model").textContent = label;
  $("#loaded-model").title = label;
  if (!name) $("#backend").textContent = "";
}
function formatShortcut(value) {
  const parts = String(value || "").split("+").map((part) => part.trim()).filter(Boolean);
  return parts.length ? parts.join(" + ") : "None";
}
function updateShortcutLabel() {
  const label = $("#shortcut-label");
  if (!label) return;
  label.textContent = `Transcription Shortcut: ${formatShortcut(configuration?.shortcut)}`;
}
function showSettings() {
  for (const entry of ALL_ROWS) {
    if (entry.type === "checkbox") field(entry.path).checked = get(configuration, entry.path);
    else field(entry.path).value = get(configuration, entry.path);
  }
  updateShortcutLabel();
  controls();
}
function readSettings() {
  const next = structuredClone(configuration);
  next.history_visible = historyVisible;
  for (const entry of ALL_ROWS) {
    const element = field(entry.path);
    const value = entry.path === "shortcut" && shortcutRecorder?.recording ? configuration.shortcut
      : entry.type === "checkbox" ? element.checked : typeof get(next, entry.path) === "number" ? Number(element.value) : element.value.trim();
    set(next, entry.path, value);
  }
  return next;
}
function settingsNeedRestart(before, after) {
  return before.model_path !== after.model_path || before.advanced.backend !== after.advanced.backend;
}
function enqueueSave(options) {
  const run = saveChain.catch(() => {}).then(() => saveSettings(options));
  saveChain = run.catch(() => {});
  return run;
}
async function saveSettings({ announce = true, force = false } = {}) {
  if (!invoke || !configuration) return;
  if (active) throw new Error("Stop dictation before changing settings.");
  const next = readSettings();
  if (!force && JSON.stringify(next) === JSON.stringify(configuration)) {
    settingsDirty = false;
    if (announce) message("Settings saved.");
    return;
  }
  const needsAccess = (!configuration.advanced.desktop_enabled && next.advanced.desktop_enabled) || configuration.microphone !== next.microphone;
  const restart = settingsNeedRestart(configuration, next);
  const previousRestart = restartPending;
  if (restart) restartPending = true;
  try {
    await invoke("save_settings", { settings: next });
  } catch (error) {
    restartPending = previousRestart;
    throw error;
  }
  const limitChanged = configuration.history_limit !== next.history_limit;
  configuration = next;
  if (limitChanged) queueHistory(async () => {
    await trimHistory(configuration.history_limit);
    await renderHistory();
  });
  updateShortcutLabel();
  if (JSON.stringify(readSettings()) === JSON.stringify(next)) settingsDirty = false;
  controls();
  await refreshCatalogue();
  await permissionSetup?.refresh();
  if (needsAccess) setTimeout(() => permissionSetup?.run(), 0);
  if (announce) {
    message(restartPending
      ? "Settings saved · session will restart, please wait."
      : "Settings saved.");
  }
}
function scheduleSave() {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    saveTimer = 0;
    enqueueSave({ announce: true }).catch((error) => message(String(error), true));
  }, 250);
}
async function flushSettings() {
  if (settingsDirty || saveTimer) {
    clearTimeout(saveTimer);
    saveTimer = 0;
    await enqueueSave({ announce: true, force: true });
  } else {
    await saveChain;
  }
  if (!restartPending) return;
  restartPending = false;
  restartInProgress = true;
  message("Settings saved · session will restart, please wait.");
  invoke("preload_model").catch((error) => {
    restartInProgress = false;
    message(String(error), true);
  });
}
async function save() {
  clearTimeout(saveTimer);
  saveTimer = 0;
  await enqueueSave({ announce: false, force: true });
}

function updateInstall(installed) {
  const row = $("#model-install-row");
  if (row) row.hidden = Boolean(installed);
}

async function refreshCatalogue() {
  if (!invoke || !configuration) return;
  modelCatalogue = await invoke("model_catalogue").catch(() => modelCatalogue);
  const select = $("#model-choice");
  const choice = modelCatalogue.find((model) => model.path === configuration.model_path) ?? modelCatalogue[0];
  if (choice && select) {
    select.value = choice.id;
    updateInstall(choice.installed);
  }
}
async function action(fn) {
  try { await fn(); } catch (error) { message(String(error), true); }
}

shortcutRecorder = new ShortcutRecorder({
  input: field("shortcut"), button: $("#record-shortcut"), hint: $("#shortcut-hint"), fieldset: $("#settings-fields"),
  saved: () => configuration?.shortcut || "",
  begin: async () => { await flushSettings(); await invoke("begin_shortcut_capture"); },
  end: () => invoke("end_shortcut_capture"),
  commit: async () => { settingsDirty = true; await flushSettings(); },
  changed: controls,
  error: error => message(String(error), true),
});

permissionSetup = new PermissionSetup({
  invoke, before: flushSettings,
  changed: () => {
    refreshStatus(); controls();
    updateKeyboardStatus();
    if (active && !pending && !permissionSetup.ready) stopSession();
  },
  notice: text => message(text),
  error: error => message(`Last setup attempt failed: ${error}`, true),
});

$("#settings-toggle").addEventListener("click", () => {
  shortcutRecorder.cancel();
  const opening = $("#settings").hidden;
  $("#settings").hidden = !opening;
  $("#dictation").hidden = opening;
  $("#settings-toggle").setAttribute("aria-expanded", String(opening));
  if (opening) return;
  resizeTranscript();
  action(flushSettings);
});
$("#model-choice").addEventListener("change", () => {
  const model = modelCatalogue.find((entry) => entry.id === $("#model-choice").value);
  if (!model) return;
  const advancedPath = field("model_path");
  if (advancedPath) advancedPath.value = model.path;
  updateInstall(model.installed);
});
$("#settings-form").addEventListener("change", (event) => {
  if (!event.target.closest?.("#settings-groups")) return;
  settingsDirty = true;
  scheduleSave();
});
$("#settings-form").addEventListener("submit", (event) => {
  event.preventDefault();
  action(flushSettings);
});
async function startSession(resume, entry) {
  if (!invoke || initializing || pending || active || shortcutRecorder?.locked || !permissionSetup.ready || modelState !== "ready") return;
  const prefix = entry?.text ?? transcript.value;
  resumingCurrent = resume && !entry;
  if (resume) {
    if (!prefix.trim()) return;
    resumePrefix = /\s$/.test(prefix) ? prefix : `${prefix} `;
    resumeOriginal = prefix;
  } else {
    resumePrefix = "";
    resumeOriginal = "";
  }
  pending = true;
  active = true;
  message();
  state("Starting");
  controls();
  try {
    const id = await invoke("start_dictation");
    sessionId = Math.max(sessionId, id);
  } catch (error) {
    active = false;
    resumePrefix = "";
    resumingCurrent = false;
    resumeOriginal = "";
    state("Not ready");
    message(String(error), true);
  } finally { pending = false; controls(); }
}

async function stopSession() {
  if (!invoke || pending || !active) return;
  pending = true;
  state("Stopping…");
  controls();
  try { await invoke("stop_dictation"); }
  catch (error) { state("Not ready"); message(String(error), true); }
  finally { pending = false; controls(); }
}

$("#dictate").addEventListener("click", () => { if (active) stopSession(); else startSession(false); });
$("#resume").addEventListener("click", () => startSession(true));
function cancelSession() {
  if (active && invoke) action(async () => { state("Cancelling…"); await invoke("cancel_dictation"); });
}
$("#cancel").addEventListener("click", cancelSession);
document.addEventListener("keydown", (event) => {
  const key = [event.ctrlKey ? "Control" : "", event.altKey ? "Alt" : "", event.key].filter(Boolean).join("+");
  if (active && !event.shiftKey && !event.metaKey && key === configuration?.cancel_key) { event.preventDefault(); cancelSession(); }
});
transcript.addEventListener("input", () => { resizeTranscript(); controls(); scheduleDraft(); });
window.addEventListener("resize", resizeTranscript);
async function copyText(text) {
  if (!text) return;
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      message("Copied.");
      return;
    }
  } catch {}
  const selection = document.createElement("textarea");
  selection.value = text;
  selection.className = "sr-only";
  document.body.append(selection);
  selection.select();
  let copied = false;
  try { copied = document.execCommand("copy"); } catch {}
  selection.remove();
  message(copied ? "Copied." : "Clipboard unavailable.", !copied);
}
$("#copy").addEventListener("click", () => action(() => copyText(transcript.value)));
async function saveText(value) {
  const text = value.trim();
  if (!text) return;
  const now = new Date();
  const pad = (value) => String(value).padStart(2, "0");
  const filename = `arxell-${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}.txt`;
  const path = await invoke("save_transcript", { text, filename });
  message(`Saved to ${path}`);
}
$("#save").addEventListener("click", () => action(() => saveText(transcript.value)));
function updateKeyboardStatus() {
  const check = permissionSetup?.report?.checks.find(check => check.id === "desktop");
  if (!check) return;
  $("#desktop-keyboard-status").textContent = !check.required ? "Disabled" : check.ready ? "Granted" : desktopAvailable ? "Access required" : "Unavailable";
  $("#desktop-keyboard-status").title = check.message;
  $("#desktop-permission").hidden = !check.required || check.ready || !desktopAvailable;
}
async function refreshDesktopStatus() {
  if (!invoke) return;
  const status = await invoke("desktop_status");
  $("#desktop-keyboard-status").textContent = status.keyboard_ready ? "Granted" : status.permission_required ? "Access required" : "Unavailable";
  $("#desktop-keyboard-status").title = status.message;
  $("#settings-version").textContent = appBuild ? `Build ${appBuild}` : "";
  $("#desktop-permission").hidden = !status.permission_required;
  const x11Row = field("advanced.x11_keyboard_enabled")?.closest(".setting-row");
  if (x11Row) x11Row.hidden = status.platform !== "Linux / X11";
  await permissionSetup.refresh();
  return status;
}
$("#desktop-refresh").addEventListener("click", () => action(refreshDesktopStatus));
$("#desktop-permission").addEventListener("click", () => action(async () => {
  await permissionSetup.run();
  await refreshDesktopStatus();
}));
$("#verify").addEventListener("click", () => action(async () => {
  $("#verify").disabled = true;
  message("Verifying model…");
  try { await invoke("verify_model_file", { path: field("model_path").value.trim() }); message("Model integrity verified."); }
  finally { $("#verify").disabled = false; }
}));
$("#unload").addEventListener("click", () => action(async () => { await invoke("unload_model"); loadedModel(null); }));
$("#download").addEventListener("click", () => action(async () => {
  if (!$("#settings-form").reportValidity()) return;
  await save();
  downloading = true;
  message("Starting model download…");
  controls();
  try { await invoke("download_model"); }
  catch (error) { downloading = false; controls(); throw error; }
}));
$("#cancel-download").addEventListener("click", () => action(async () => { await invoke("cancel_download"); message("Cancelling download…"); }));
$("#copy-token").addEventListener("click", () => action(async () => { await navigator.clipboard.writeText(configuration.advanced.api_token); message("API token copied."); }));
$("#regenerate-token").addEventListener("click", () => action(async () => {
  clearTimeout(saveTimer);
  saveTimer = 0;
  const next = readSettings();
  next.advanced.api_token = await invoke("new_api_token");
  await invoke("save_settings", { settings: next });
  configuration = next;
  settingsDirty = false;
  showSettings();
  message("New token saved. Existing API clients were disconnected.");
}));
$("#reset").addEventListener("click", () => action(async () => {
  if (!window.confirm("Restore defaults? Model files and the transcript will be kept.")) return;
  clearTimeout(saveTimer);
  saveTimer = 0;
  configuration = await invoke("reset_settings");
  settingsDirty = false;
  restartPending = false;
  restartInProgress = false;
  historyVisible = configuration.history_visible;
  updateHistoryVisibility();
  queueHistory(async () => { await trimHistory(configuration.history_limit); await renderHistory(); });
  showSettings();
  invoke("preload_model").catch(() => {});
  await permissionSetup.refresh();
  if (!permissionSetup.ready) setTimeout(() => permissionSetup.run(), 0);
  message("Defaults restored.");
}));

function queueHistory(work) {
  const run = historyChain.then(work);
  historyChain = run.catch((error) => message(`Could not save or load local history: ${error}`, true));
  return historyChain;
}
function currentEntry() {
  return { text: transcript.value, date: $("#timestamp").dateTime || new Date().toISOString() };
}
function scheduleDraft() {
  clearTimeout(draftTimer);
  draftTimer = setTimeout(() => {
    const entry = currentEntry();
    queueHistory(() => saveDraft(entry));
  }, 250);
}
function beginTranscript(id) {
  clearTimeout(draftTimer);
  const previous = currentEntry();
  const keepDate = resumingCurrent;
  if (!resumingCurrent && previous.text.trim()) {
    queueHistory(async () => {
      await archiveTranscript(previous, configuration?.history_limit ?? 10);
      await renderHistory(true);
    });
  }
  displayedSessionId = id;
  transcript.value = resumePrefix.trimEnd();
  if (!keepDate) stampTranscript();
  resumingCurrent = false;
  resizeTranscript();
}
function updateHistoryVisibility() {
  $("#history-panel").hidden = !historyVisible;
  $("#history-toggle").setAttribute("aria-pressed", String(historyVisible));
  $("#history-toggle").title = historyVisible ? "Hide history" : "Show history";
  resizeTranscript();
}
$("#history-toggle").addEventListener("click", () => {
  historyVisible = !historyVisible;
  updateHistoryVisibility();
  if (configuration && !active) scheduleSave();
});
$("#history-more").addEventListener("click", () => {
  historyCount += 50;
  queueHistory(() => renderHistory());
});
async function renderHistory(follow = false) {
  const generation = ++historyRender;
  const { entries, hasMore } = await loadHistory(historyCount);
  if (generation !== historyRender) return;
  const host = $("#history-list");
  const panel = $("#history-panel");
  const bottom = panel.scrollHeight - panel.scrollTop - panel.clientHeight < 30;
  const oldHeight = panel.scrollHeight;
  const oldTop = panel.scrollTop;
  const fragment = document.createDocumentFragment();
  for (const entry of entries) {
    const card = document.createElement("article");
    card.className = "transcript-bubble history-bubble";
    const time = document.createElement("time");
    time.className = "history-time";
    time.dateTime = entry.date;
    const date = new Date(entry.date);
    time.textContent = date.toLocaleString(undefined, { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false });
    time.title = date.toLocaleString();
    const summary = document.createElement("button");
    summary.type = "button";
    summary.className = "history-summary";
    const lines = entry.text.trim().split(/\r?\n/);
    summary.textContent = lines[0] + (lines.length > 1 ? " …" : "");
    summary.title = "Expand or collapse transcription";
    const text = document.createElement("p");
    text.id = `history-text-${entry.id}`;
    text.className = "history-text";
    text.textContent = entry.text;
    text.hidden = !expandedHistory.has(entry.id);
    summary.setAttribute("aria-controls", text.id);
    summary.setAttribute("aria-expanded", String(!text.hidden));
    summary.addEventListener("click", () => {
      text.hidden = !text.hidden;
      summary.setAttribute("aria-expanded", String(!text.hidden));
      if (text.hidden) expandedHistory.delete(entry.id);
      else expandedHistory.add(entry.id);
    });
    const actions = document.createElement("div");
    actions.className = "transcript-actions";
    for (const [source, label, callback] of [
      ["resume", "Resume this transcription", () => startSession(true, entry)],
      ["save", "Save this transcription to file", () => saveText(entry.text)],
      ["copy", "Copy this transcription", () => copyText(entry.text)],
    ]) {
      const button = document.getElementById(source).cloneNode(true);
      button.removeAttribute("id");
      button.disabled = false;
      button.title = label;
      button.setAttribute("aria-label", label);
      if (source === "resume") button.classList.add("history-resume");
      button.addEventListener("click", () => action(callback));
      actions.append(button);
    }
    card.append(time, summary, actions, text);
    fragment.append(card);
  }
  host.replaceChildren(fragment);
  $("#history-more").hidden = !hasMore;
  if (follow || bottom) panel.scrollTop = panel.scrollHeight;
  else panel.scrollTop = oldTop + panel.scrollHeight - oldHeight;
  controls();
}

async function init() {
  controls(); // No settings or shortcut capture before native setup completes.
  resizeTranscript();
  stampTranscript();
  if (!invoke) { initializing = false; state("Browser preview"); controls(); return; }
  try {
    await window.__TAURI__.event.listen("session-event", ({ payload }) => {
      if (payload.session_id < sessionId || (payload.revision && payload.revision <= revision)) return;
      if (payload.session_id > displayedSessionId) beginTranscript(payload.session_id);
      sessionId = payload.session_id;
      revision = payload.revision || revision;
      if (payload.kind === "state") {
        active = !["complete", "cancelled", "error"].includes(payload.state);
        if (!active) {
          resumePrefix = "";
          resumeOriginal = "";
          scheduleDraft();
          if (configuration && configuration.history_visible !== historyVisible) scheduleSave();
        }
        const labels = { loading: "Loading model", listening: "Listening", finalizing: "Finishing…", cancelled: "Cancelled", error: "Error" };
        if (payload.state === "complete") refreshStatus();
        else state(labels[payload.state] ?? payload.state, payload.state === "error" ? "bad" : "");
      } else if (["interim", "final"].includes(payload.kind)) {
        if (payload.kind === "final" && payload.action === "Cancel" && resumeOriginal) {
          transcript.value = resumeOriginal;
          resizeTranscript();
        } else applyTranscript(payload.text);
        if (payload.kind === "final") resumePrefix = "";
        if (payload.kind === "final" && payload.action === "EndSubmit") message("Dictation finished. Press Enter in the destination yourself; no submission was sent.");
        if (payload.kind === "final" && payload.action === "Submitted") message("Destination activated.");
      } else if (payload.kind === "delivery") {
        $("#dictation").dataset.delivery = payload.state;
        message(payload.message, payload.state === "stopped");
      } else if (payload.kind === "backend") $("#backend").textContent = payload.backend;
      else if (["warning", "error"].includes(payload.kind)) message(payload.message, payload.kind === "error");
      controls();
    });
    await window.__TAURI__.event.listen("model-state", ({ payload }) => {
      if (restartInProgress && ["ready", "error"].includes(payload.state)) {
        restartInProgress = false;
        if (payload.state === "ready") {
          applyModelState(payload.state, payload.name);
          message("Settings saved.");
          return;
        }
      }
      applyModelState(payload.state, payload.name);
    });
    await window.__TAURI__.event.listen("shortcut-captured", ({ payload }) => shortcutRecorder.capture(payload));
    await window.__TAURI__.event.listen("shortcut-capture-cancelled", () => shortcutRecorder.cancel());
    await window.__TAURI__.event.listen("shortcut-error", ({ payload }) => message(payload, true));
    await window.__TAURI__.event.listen("shortcut-status", ({ payload }) => message(payload));
    await window.__TAURI__.event.listen("model-download", ({ payload }) => {
      downloading = ["downloading", "verifying"].includes(payload.state);
      if (payload.state === "complete") {
        field("model_path").value = payload.message;
        action(async () => {
          await save();
          restartPending = false;
          invoke("preload_model").catch(() => {});
          await refreshCatalogue();
          message("Model installed and selected.");
        });
      } else message(payload.message || `${payload.state === "verifying" ? "Verifying model" : "Downloading model"} · ${Math.floor(payload.received / payload.total * 100)}%`, payload.state === "error");
      controls();
    });
    await invoke("end_shortcut_capture"); // Recover cleanly from a WebView reload.
    const info = await invoke("app_info");
    desktopAvailable = info.desktop_available === true;
    appBuild = info.build_id || "";
    await refreshDesktopStatus().catch((error) => message(String(error), true));
    configuration = await invoke("get_settings");
    const microphones = await invoke("input_devices").catch(() => []);
    for (const { name } of microphones) field("microphone").add(new Option(name, name));
    if (configuration.microphone && !microphones.some((m) => m.name === configuration.microphone)) field("microphone").add(new Option(`${configuration.microphone} (unavailable)`, configuration.microphone));
    historyVisible = configuration.history_visible ?? true;
    updateHistoryVisibility();
    showSettings();
    await queueHistory(async () => {
      const draft = await loadDraft();
      if (draft && !active && !sessionId) {
        transcript.value = draft.text;
        stampTranscript(draft.date);
      }
      await trimHistory(configuration.history_limit ?? 10);
      await renderHistory();
    });
    modelCatalogue = await invoke("model_catalogue").catch(() => []);
    const modelSelect = $("#model-choice");
    for (const model of modelCatalogue) modelSelect.add(new Option(model.name, model.id));
    const selectedModel = modelCatalogue.find((model) => model.path === configuration.model_path) ?? modelCatalogue[0];
    if (selectedModel) {
      modelSelect.value = selectedModel.id;
      updateInstall(selectedModel.installed);
    }
    const runtime = await invoke("model_status");
    if (runtime.snapshot && runtime.snapshot.revision >= revision) {
      const snapshot = runtime.snapshot;
      sessionId = snapshot.session_id;
      revision = snapshot.revision;
      displayedSessionId = snapshot.session_id;
      if (snapshot.session_id || snapshot.text) {
        applyTranscript(snapshot.text);
      }
      if (runtime.loaded_model) $("#backend").textContent = snapshot.backend || "";
      active = ["loading", "listening", "finalizing"].includes(snapshot.state);
      if (active) state("Dictation active");
    }
    applyModelState(runtime.model_state, runtime.loaded_model);
    if (info.warning) message(info.warning, true);
    else if (!configuration.advanced.desktop_enabled) message("External typing is off. Enable Stream dictation into other applications under Shortcuts & delivery.");
    // Safety net: startup already preloads, but recover from a missed event.
    invoke("preload_model").catch(() => {});
  } catch (error) { state("Setup needed", "bad"); message(String(error), true); }
  initializing = false;
  resizeTranscript();
  controls();
  if (invoke) await permissionSetup.initialize();
}
init();
