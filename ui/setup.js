// First-run/current-session readiness. Native code independently enforces the
// same requirements; this panel is not the authority that permits recording.
export class PermissionSetup {
  constructor({ invoke, changed, before, error, notice = () => {} }) {
    Object.assign(this, { invoke, changed, before, error, notice });
    this.report = null;
    this.running = false;
    this.lastError = "";
    this.sequence = 0;
    this.panel = document.querySelector("#setup-panel");
    this.button = document.querySelector("#setup-permissions");
    this.button.addEventListener("click", () => this.run());
  }
  get ready() { return Boolean(this.report?.ready && !this.report.busy && !this.running); }
  get busy() { return this.running || Boolean(this.report?.busy); }
  render() {
    const missing = (this.report?.checks || []).filter(check => check.required && !check.ready);
    this.panel.hidden = this.ready || (!this.report && !this.lastError && !this.busy);
    this.panel.setAttribute("aria-busy", String(this.busy));
    const names = { microphone: "microphone", desktop: "desktop input", shortcut: "global shortcut", model: "transcription model" };
    const required = missing.map(check => names[check.id] || check.label).join(", ");
    let text = required ? `Required: ${required}.` : "Checking required access…";
    // Never display an old failure as the status of a pending request.
    if (this.busy) text += " Checking access; complete any system permission dialog.";
    else if (this.lastError) text += ` Last attempt failed: ${this.lastError}`;
    const message = document.querySelector("#setup-message");
    message.textContent = text;
    message.title = text;
    this.button.disabled = !this.invoke || this.busy;
    this.button.textContent = this.busy ? "Waiting…" : "Retry setup";
    this.button.title = this.busy ? "Approve or cancel in the system permission dialog; there is no approval deadline." : "Recheck outstanding setup requirements";
    this.changed();
  }
  async refresh() {
    if (!this.invoke) return;
    const sequence = ++this.sequence;
    try {
      const report = await this.invoke("setup_status");
      if (sequence !== this.sequence) return;
      this.report = report;
      if (report.ready) this.lastError = "";
    } catch (error) {
      if (sequence !== this.sequence) return;
      this.report = null; // A failed status query is not permission to record.
      this.lastError = String(error);
    }
    this.render();
  }
  async run() {
    if (!this.invoke || this.busy) return;
    this.running = true;
    this.lastError = "";
    this.render();
    this.notice("Checking required access. Complete any system permission dialog.");
    try {
      await this.before();
      this.report = await this.invoke("setup_permissions");
      this.notice(this.report.ready ? "Required access verified." : "Access checks finished; see the remaining setup requirements.");
    } catch (error) {
      this.lastError = String(error);
      this.error(this.lastError);
    } finally {
      this.running = false;
      await this.refresh();
    }
  }
  async initialize() {
    await this.refresh();
    // One automatic setup attempt per launch. Never repeatedly prompt after a
    // denial, timeout or revocation. These require an explicit Retry action.
    if (this.report?.checks.some(check => check.required && !check.ready && ["microphone", "desktop"].includes(check.id))) {
      setTimeout(() => this.run(), 0);
    }
    this.timer = setInterval(() => this.refresh(), 1500);
    window.addEventListener("beforeunload", () => clearInterval(this.timer), { once: true });
  }
}
