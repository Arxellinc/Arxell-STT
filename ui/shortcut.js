// Match the native shortcut parser's key names; never store a typed character
// such as '+' (which is also the accelerator separator) for Shift+Equal.
const NAMED_KEYS = new Set([
  "Backquote", "Backslash", "BracketLeft", "BracketRight", "Comma", "Equal",
  "Minus", "Period", "Quote", "Semicolon", "Slash", "Backspace", "CapsLock",
  "Enter", "Space", "Tab", "Delete", "End", "Home", "Insert", "PageDown", "PageUp",
  "PrintScreen", "ScrollLock", "Pause", "ArrowDown", "ArrowLeft", "ArrowRight", "ArrowUp",
  "NumLock", "NumpadAdd", "NumpadDecimal", "NumpadDivide", "NumpadEnter", "NumpadEqual",
  "NumpadMultiply", "NumpadSubtract", "Escape", "AudioVolumeDown", "AudioVolumeUp",
  "AudioVolumeMute", "MediaPlay", "MediaPause", "MediaPlayPause", "MediaStop",
  "MediaTrackNext", "MediaTrackPrevious",
]);

export function shortcutFromEvent(event) {
  if (event.repeat) return null;
  if (event.isComposing || event.key === "Dead" || event.key === "Process"
      || event.getModifierState?.("AltGraph")) {
    throw new Error("Choose a shortcut without composed text or AltGr.");
  }
  if (/^(Control|Shift|Alt|Meta|Super|OS)(Left|Right)?$/.test(event.code || event.key)) return null;
  const modifiers = [];
  if (event.ctrlKey) modifiers.push("Ctrl");
  if (event.altKey) modifiers.push("Alt");
  if (event.shiftKey) modifiers.push("Shift");
  if (event.metaKey) modifiers.push("Super");
  if (!modifiers.length) throw new Error("Include Ctrl, Alt, Shift, or the Super/Command key.");
  const code = event.code;
  let key;
  if (/^Key[A-Z]$/.test(code)) key = code.slice(3);
  else if (/^Digit[0-9]$/.test(code)) key = code.slice(5);
  else if (/^(F([1-9]|1[0-9]|2[0-4])|Numpad[0-9])$/.test(code) || NAMED_KEYS.has(code)) key = code;
  else throw new Error("This key is not supported. Try another combination.");
  return [...modifiers, key].join("+");
}

export class ShortcutRecorder {
  constructor({ input, button, hint, fieldset, begin, end, commit, saved, changed, error }) {
    Object.assign(this, { input, button, hint, fieldset, begin, end, commit, saved, changed, error });
    this.phase = "idle";
    this.disabled = new Map();
    button.addEventListener("click", event => {
      if (this.recording && (event.ctrlKey || event.altKey || event.shiftKey || event.metaKey)) {
        this.hint.textContent = "Release the shortcut keys, then click Stop recording.";
        return;
      }
      this.toggle();
    });
    document.addEventListener("keydown", event => this.keydown(event), true);
    document.addEventListener("keyup", event => {
      if (!this.recording) return;
      event.preventDefault();
      event.stopImmediatePropagation();
    }, true);
    window.addEventListener("blur", () => this.cancel());
    document.addEventListener("visibilitychange", () => { if (document.hidden) this.cancel(); });
  }
  get recording() { return this.phase === "recording"; }
  get locked() { return this.phase !== "idle"; }
  sync() {
    this.input.readOnly = this.locked;
    this.button.textContent = this.recording ? "Stop recording" : this.locked ? "Please wait…" : "Record shortcut";
    this.button.setAttribute("aria-pressed", String(this.recording));
    this.button.disabled = this.locked && !this.recording;
    if (this.locked) {
      for (const element of this.fieldset.querySelectorAll("input, select, button")) {
        if (element === this.button) continue;
        if (!this.disabled.has(element)) this.disabled.set(element, element.disabled);
        element.disabled = true;
      }
    }
  }
  state(phase) {
    this.phase = phase;
    if (!this.locked) {
      for (const [element, disabled] of this.disabled) element.disabled = disabled;
      this.disabled.clear();
    }
    this.sync();
    this.changed();
  }
  async toggle() {
    if (this.recording) return this.finish(true);
    if (this.locked) return;
    this.original = this.saved();
    this.input.value = this.original;
    this.candidate = null;
    this.cancelRequested = false;
    this.input.removeAttribute("aria-invalid");
    this.state("starting");
    try {
      await this.begin();
      if (this.cancelRequested) {
        await this.end();
        this.hint.textContent = "Recording cancelled. Previous shortcut kept.";
        this.state("idle");
        return;
      }
      this.state("recording");
      this.button.focus();
      this.hint.textContent = "Press your combination, then click Stop recording to set it. Escape cancels. System-reserved shortcuts may not be available.";
    } catch (error) {
      // begin may have succeeded before a subsequent operation failed.
      await this.end().catch(() => {});
      this.input.value = this.saved();
      this.state("idle");
      this.error(error);
    }
  }
  capture(value) {
    if (!this.recording) return;
    this.candidate = value;
    this.input.value = value;
    this.input.removeAttribute("aria-invalid");
    this.hint.textContent = `${value.split("+").join(" + ")} captured. Click Stop recording to set it, or press Enter. Escape cancels.`;
  }
  keydown(event) {
    if (!this.recording) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    if (event.key === "Escape" && !event.ctrlKey && !event.altKey && !event.shiftKey && !event.metaKey) {
      this.cancel();
      return;
    }
    if (event.key === "Enter" && this.candidate && !event.ctrlKey && !event.altKey && !event.shiftKey && !event.metaKey) {
      this.finish(true);
      return;
    }
    try {
      const value = shortcutFromEvent(event);
      if (value) this.capture(value);
    } catch (error) {
      this.candidate = null;
      this.input.value = this.original;
      this.input.setAttribute("aria-invalid", "true");
      this.hint.textContent = error.message;
    }
  }
  cancel() {
    if (this.phase === "starting") this.cancelRequested = true;
    if (this.recording) return this.finish(false);
  }
  async finish(save) {
    if (!this.recording) return;
    const candidate = this.candidate;
    this.state("finishing");
    try {
      if (save && candidate) {
        this.input.value = candidate;
        await this.commit();
        this.hint.textContent = "Shortcut saved. On Wayland, approve the system dialog if one appears.";
      } else {
        this.input.value = this.original;
        this.hint.textContent = save ? "No shortcut recorded. Previous shortcut kept." : "Recording cancelled. Previous shortcut kept.";
      }
    } catch (error) {
      this.input.value = this.saved();
      this.hint.textContent = "Shortcut could not be set. Previous shortcut kept; try another combination.";
      this.error(error);
    } finally {
      await this.end().catch(error => this.error(error));
      this.input.removeAttribute("aria-invalid");
      this.state("idle");
      if (document.hasFocus()) this.button.focus();
    }
  }
}
