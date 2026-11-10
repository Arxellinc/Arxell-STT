// Isolated UI fixture. No microphone, downloads, API listener or desktop input.
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";

const root = new URL("../ui/", import.meta.url);
const configuration = {
  version: 4, history_limit: 10, history_visible: true, model_path: "/models/parakeet.gguf", microphone: "", shortcut: "",
  cancel_key: "Escape",
  advanced: {
    backend: "Auto", cues_enabled: true, vad_gating: true, vad_threshold: .5, speech_hold_ms: 0,
    max_session_seconds: 600, idle_unload_seconds: 0, stream_preset: "efficient", diagnostics: false,
    cue: { end_phrase: "over", submit_phrase: "out", cancel_phrase: "cancel", pre_gap_ms: 250, confirm_ms: 800, inactivity_ms: 3000, max_cue_ms: 2000, text_grace_ms: 1800 },
    api_enabled: false, api_port: 14321, api_token: "fixture-token-not-a-real-credential".repeat(2), api_max_seconds: 120,
    desktop_enabled: true, terminal_enabled: true, submit_enabled: true, submit_key: "Enter",
  },
};
const mock = `<script>
let config = JSON.parse(sessionStorage.getItem('fixture-settings') || 'null') || ${JSON.stringify(configuration)};
const listeners = new Map();
let id = 0;
let shortcutCapture = false;
const setupMode = new URLSearchParams(location.search).get('setup');
window.testPermissionsReady = !setupMode;
window.testPermissionHold = setupMode === 'pending';
window.testPermissionFail = setupMode === 'denied';
window.testModelState = 'ready';
window.testEvents = [];
window.testEmit = (name, payload) => {
  if (name === 'model-state') window.testModelState = payload.state;
  (listeners.get(name) || []).forEach(fn => fn({payload}));
};
window.__TAURI__ = {
  event: { listen: async (name, fn) => { listeners.set(name, [...(listeners.get(name) || []), fn]); return () => {}; } },
  core: { invoke: async (name, args) => {
    window.testEvents.push({name, args});
    if (name === "app_info") return {desktop_available:true};
    if (name === "desktop_status" || name === "prepare_desktop") return {supported:true, keyboard_ready:true, permission_required:false, platform:"Test", message:"Mock backend: no desktop input is sent."};
    if (name === "setup_status") {
      const mic = window.testMicReady ?? window.testPermissionsReady;
      const desktop = window.testDesktopReady ?? window.testPermissionsReady;
      const checks = [
        {id:'microphone',label:'Microphone',required:true,ready:mic,message:mic?'Input checked':'Microphone access required'},
        {id:'desktop',label:'Typing into other applications',required:config.advanced.desktop_enabled,ready:desktop,message:desktop?'Input access verified':'System keyboard access required'},
        {id:'shortcut',label:'Global shortcut',required:Boolean(config.shortcut),ready:true,message:'Registered'},
        {id:'model',label:'Transcription model',required:true,ready:window.testModelState==='ready',message:window.testModelState==='ready'?'Loaded':'Model not loaded'},
      ];
      return {ready:checks.every(c=>!c.required||c.ready),busy:false,checks};
    }
    if (name === 'setup_permissions') {
      if (window.testPermissionHold) await new Promise(resolve => { window.testCompletePermissions = resolve; });
      if (window.testPermissionFail) throw new Error('System permission denied or unavailable');
      window.testPermissionsReady = true;
      return window.__TAURI__.core.invoke('setup_status');
    }
    if (name === "get_settings") return structuredClone(config);
    if (name === "save_settings") {
      if (window.testFailSave) { window.testFailSave = false; throw new Error("Shortcut unavailable or already in use"); }
      config = structuredClone(args.settings);
      sessionStorage.setItem('fixture-settings', JSON.stringify(config)); return;
    }
    if (name === "begin_shortcut_capture") {
      if (window.testBeginDelay) await new Promise(resolve => setTimeout(resolve, window.testBeginDelay));
      shortcutCapture = true; return;
    }
    if (name === "end_shortcut_capture") { shortcutCapture = false; return; }
    if (name === "input_devices") return [{name:"Test microphone"}];
    if (name === "model_catalogue") return [{id:"parakeet-unified-en-0.6b", name:"Parakeet Unified · English · Q4_K_M", path:"/models/parakeet.gguf", installed: Boolean(window.__modelInstalled)}];
    if (name === "model_status") return {loaded_model:'Parakeet Unified · English · Q4_K_M', model_state:window.testModelState};
    if (name === "preload_model" || name === "reauthorize_shortcut") return;
    if (name === "start_dictation") {
      if (shortcutCapture) throw new Error("Finish recording the shortcut first");
      const ready = await window.__TAURI__.core.invoke('setup_status');
      if (!ready.ready) throw new Error('Setup required before dictating');
      ++id;
      queueMicrotask(() => {
        testEmit("model-state", {name:"Parakeet Unified · English · Q4_K_M", state:"ready"});
        testEmit("session-event", {session_id:id, kind:"state", state:"listening"});
        testEmit("session-event", {session_id:id, kind:"interim", text:"Test transcript"});
      });
      return id;
    }
    if (name === "stop_dictation" || name === "cancel_dictation") {
      testEmit("session-event", {session_id:id, kind:"final", action:name === "cancel_dictation" ? "Cancel" : null, text:name === "cancel_dictation" ? "" : "Test transcript."});
      testEmit("session-event", {session_id:id, kind:"state", state:name === "cancel_dictation" ? "cancelled" : "complete"});
      return;
    }
    if (name === "unload_model") { testEmit("model-state", {name:null, state:"missing"}); return; }
    if (name === "reset_settings") {
      config = structuredClone({...config, ...{
        microphone: "", shortcut: "", cancel_key: "Escape", history_limit: 10, history_visible: true,
        advanced: {...config.advanced, cues_enabled: true, backend: "Auto", desktop_enabled: true, terminal_enabled: true,
          cue: {...config.advanced.cue, end_phrase: "over", submit_phrase: "out", cancel_phrase: "cancel"}},
      }});
      sessionStorage.setItem('fixture-settings', JSON.stringify(config));
      return config;
    }
    if (name === "verify_model_file") return;
    if (name === "save_transcript") return "/tmp/" + args.filename;
    throw new Error("Unimplemented fixture command: " + name);
  } }
};
</script>`;
const files = new Map([["/", "index.html"], ["/index.html", "index.html"], ["/main.js", "main.js"], ["/styles.css", "styles.css"], ["/history.js", "history.js"], ["/shortcut.js", "shortcut.js"], ["/setup.js", "setup.js"]]);
const server = createServer(async (request, response) => {
  const file = files.get((request.url || "/").split("?")[0]);
  if (!file) { response.writeHead(404).end(); return; }
  let body = await readFile(new URL(file, root), "utf8");
  if (file === "index.html") body = body.replace('<script type="module"', `${mock}<script type="module"`);
  response.writeHead(200, { "Cache-Control": "no-store", "Content-Type": file.endsWith(".js") ? "text/javascript" : file.endsWith(".css") ? "text/css" : "text/html" }).end(body);
});
server.listen(0, "127.0.0.1", () => console.log(`UI fixture: http://127.0.0.1:${server.address().port}`));
