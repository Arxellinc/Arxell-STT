# Production readiness — prioritized checklist

Production-ready means a normal user can install Arxell Speak, reach working
dictation, use it repeatedly without losing text or typing into the wrong place,
and recover from failures without developer help. Passing unit tests alone is
not enough.

This is the agreed starting backlog, ordered by priority. It consolidates the
previous plans and usability findings without treating every historical proposal
as mandatory. Product-scope decisions remain open where explicitly noted.

## How to track progress

- `[ ]` means acceptance is still pending, even if some implementation exists.
- `[x]` means the stated task has been completed and its evidence recorded.
- Keep implementation, automated testing, hardware validation, and release status
  separate. A passing mock or compile check is not native-delivery evidence.
- Platform-specific requirements apply to every platform advertised as supported;
  a narrower first release is acceptable if its limits are explicit.

## Priority 0 — Establish a trustworthy release baseline

### 1. Put the current project under version control and preserve the baseline

- [x] Initialize this checkout on `main` and configure the GitHub remote.
- [x] Commit the current source, lockfiles, build scripts, and retained documentation.
- [x] Exclude models, credentials, personal settings/history, and build output.
- [x] Verify the initial baseline is available on GitHub.
- [ ] Preserve relevant test evidence and existing installers separately.

**Evidence:** initial local commit `ac34025` (`Initial Arxell Speak baseline`);
remote: <https://github.com/Arxellinc/Arxell-STT>. The outdated documents were
intentionally removed before the baseline commit. Local test logs and installers
are not included in Git.

**Done when:** every subsequent change is reviewable and reversible.

### 2. Freeze the production scope and support targets

Agree explicitly on:

- [ ] Core workflow: local English dictation → transcript/history → optional external typing.
- [ ] No automatic submission.
- [ ] Operating systems, architectures, desktops, and application types supported by the first release.
- [ ] Whether API access and spoken controls are being removed, as proposed.

**Recommendation:** simplify to the core workflow and validate Linux first.
Windows/macOS can follow; they must not be advertised as supported solely because
their adapters compile.

### 3. Establish one authoritative release checklist

- [ ] Reconcile retained user guidance with the actual rc.4 source.
- [ ] Separate implemented, automatically tested, hardware-validated, and released work.
- [ ] Record each remaining blocker with its acceptance test.
- [ ] Run a fresh baseline suite before changing behavior.

**Done when:** there is one reliable answer to “what works, what fails, and what
blocks release?” This file is the starting checklist, not proof of acceptance.

## Priority 1 — Prevent data loss and unsafe behavior

### 4. Make active transcripts crash-recoverable

- [ ] Checkpoint live text at a bounded interval.
- [ ] Make current-draft/archive transitions non-destructive.
- [ ] Recover interrupted sessions after reload or restart.
- [ ] Prevent duplicate archives during recovery.

**Acceptance:** killing an isolated app mid-dictation recovers the latest
checkpoint and preserves the previous transcript.

### 5. Make storage failures survivable

- [ ] Handle IndexedDB open, transaction, quota, and write failures.
- [ ] Preserve the previous transcript if archiving fails.
- [ ] Keep dictation, copying, and export usable when history is unavailable.
- [ ] Surface a persistent recovery action rather than a transient footer error.

### 6. Fix destructive export and reset behavior

- [ ] Prevent silent overwrite when two exports receive the same filename.
- [ ] Provide a normal Save As workflow, or at minimum collision-safe file creation.
- [ ] Stop Restore defaults from silently pruning history.
- [ ] Clearly confirm intentional retention reductions and deletions.

### 7. Enforce single-instance ownership

- [ ] Allow only one process per profile to own recording, shortcuts, and settings writes.
- [ ] Bring the existing instance forward on a second launch.
- [ ] Define behavior when an old version is running during an upgrade.
- [ ] Isolate developer/test profiles from personal data.

### 8. Audit keyboard delivery safety before relaxing constraints

Verify:

- [ ] Password/secure/elevated-field refusal where detectable.
- [ ] Exact destination binding and stopping on focus changes.
- [ ] Physical modifier handling, especially on Wayland and terminals.
- [ ] No Enter, control-character, or accidental submit injection.
- [ ] No automatic replay after ambiguous delivery.

**Important:** do not implement historical constraint-relaxation proposals
unchanged. A quiet period does not prove modifiers were released, and final
acknowledgement cannot undo text sent to the wrong field. Each relaxation needs
its own safety justification and test.

Terminal support must state its limits: accessibility cannot reliably identify
every secure prompt or application input mode.

## Priority 2 — Get the real dictation workflow working reliably

### 9. Separate in-app readiness from external-delivery readiness

- [ ] Require only microphone access and a loaded model for in-app dictation.
- [ ] Require the relevant destination access additionally for external typing.
- [ ] Do not let an unavailable global shortcut disable the microphone button.
- [ ] Leave in-app transcription usable when desktop permission is denied.

**Acceptance:** a user can decline desktop access and still dictate into Arxell.

### 10. Finish permission onboarding and recovery

- [ ] Explain why microphone, shortcut, and keyboard permissions are needed.
- [ ] Keep requests tied to deliberate user actions.
- [ ] Handle denial, cancellation, revocation, portal restart, and app restart.
- [ ] Show pending versus granted access truthfully.
- [ ] Ensure permission dialogs are visibly associated with Arxell.

### 11. Prove native delivery on the intended Linux desktop

The latest native keyboard fixture at the time this checklist was created is not
passing. Test separately:

- [ ] Wayland portal keyboard delivery.
- [ ] True X11/XTest delivery.
- [ ] Direct AT-SPI insertion.
- [ ] Text-only Electron/browser fields.

Diagnose failures by stage: destination discovery → focus verification → modifier
release → transport → acknowledgement. Keep fixtures isolated; do not modify
Opencode or bypass field verification to manufacture a pass.

### 12. Pass the actual microphone-to-destination acceptance test

With the user approving permissions:

- [ ] First dictation into a disposable field.
- [ ] Second session into the same field without restarting.
- [ ] Early stop using the shortcut.
- [ ] Cancel during recording.
- [ ] Move focus mid-session and verify delivery stops.
- [ ] Test an empty Opencode input without submission.

**Done when:** these work repeatedly on the release candidate, not just once on a
development build.

### 13. Make destination acquisition and partial-delivery recovery understandable

- [ ] Show whether Arxell is waiting for a destination, bound, delivering, or stopped.
- [ ] Prevent the click-mic/app-switch workflow from silently choosing an intermediate field.
- [ ] Distinguish acknowledged, undelivered, and uncertain text.
- [ ] Offer conservative recovery without duplicating already-delivered text.
- [ ] Define insertion spacing and selected-text behavior.

### 14. Complete shortcut correctness

- [ ] Display the desktop-assigned shortcut, not merely the requested value.
- [ ] Compare parsed shortcuts when detecting start/cancel conflicts.
- [ ] Warn before taking common typing/editing combinations globally.
- [ ] Validate repeated activation, holding, release, cancellation, and restart.
- [ ] Test non-US layouts and platform-specific modifiers.

### 15. Provide usable background feedback

- [ ] Make recording, stopping, completion, and delivery failure apparent while Arxell is not foreground.
- [ ] Provide an unmistakable stop/cancel path.
- [ ] Decide and document whether closing the window quits.
- [ ] Keep important failures persistent and actionable.

This does not necessarily require a tray application, but background use needs
feedback beyond an invisible footer.

## Priority 3 — Reduce complexity and finish everyday usability

### 16. Remove confirmed dead code and obsolete submission paths

- [ ] Delete unused legacy delivery/session/target modules after confirming references.
- [ ] Remove obsolete submit settings and machinery.
- [ ] Keep the active host-owned delivery worker as the single implementation.

**Done when:** the production delivery path has no automatic-submission mechanism.

### 17. Execute the approved scope reductions

If approved:

- [ ] Remove the local API listener, token handling, settings, and client example.
- [ ] Remove spoken-cue handling and its timing/phrase settings.
- [ ] Preserve silence-stop, shortcut stop, and explicit cancellation.
- [ ] Migrate existing settings safely.

These are product-scope choices, not prerequisites merely because a review
proposed them. If retained, they need complete production validation.

### 18. Simplify streaming only after measuring it

- [ ] Benchmark the proposed single streaming path on CPU and GPU.
- [ ] Preserve acceptable first-text latency, sustained throughput, and accuracy.
- [ ] Remove redundant modes/presets only when the replacement is demonstrated.
- [ ] Keep bounded capture, explicit overflow errors, and reliable finalization.

### 19. Finish model setup and repair

- [ ] Make Download → Verify → Load → Ready unambiguous.
- [ ] Offer repair for a corrupt model even when its size looks correct.
- [ ] Add a supported local-file browse/import workflow.
- [ ] Handle interrupted download, cancellation, offline operation, disk-full, and relaunch.
- [ ] Ensure installed operation does not depend on checkout paths.

### 20. Finish microphone selection and diagnosis

- [ ] Refresh devices and handle disconnect/reconnect and OS-default changes.
- [ ] Show input activity without saving audio.
- [ ] Negotiate supported formats rather than unnecessarily rejecting a device.
- [ ] Test built-in, USB, Bluetooth, and virtual inputs.
- [ ] Make silence, mute, permission failure, and device failure distinguishable.

### 21. Make settings transactional and truthful

- [ ] Validate inline and distinguish unsaved from effective values.
- [ ] Roll back rejected changes or block proceeding until resolved.
- [ ] Clearly identify next-session settings.
- [ ] Test persistence, migration, reset, and actual runtime effects.
- [ ] Reduce advanced controls to meaningful user decisions.

### 22. Finish history, privacy, formatting, and accessibility

- [ ] Provide a clear way to delete an individual entry and erase the current draft/history intentionally.
- [ ] Explain that hiding history does not stop storage.
- [ ] Decide whether a no-history session mode is required for the first release.
- [ ] Define multiline/tab behavior per supported destination without reintroducing submission.
- [ ] Verify keyboard-only use, screen-reader announcements, focus order, accessible help, scaling, and contrast.

Search, bulk export, and other convenience features can remain post-release
unless the chosen workflow requires them.

## Priority 4 — Establish real platform and performance evidence

### 23. Run a human speech and performance acceptance suite

Measure:

- [ ] Warm/cold startup and first stable/delivered text.
- [ ] CPU/GPU throughput and fallback.
- [ ] Technical vocabulary, punctuation, accents, and ordinary speech.
- [ ] False stops, thinking pauses, noise, and final-word preservation.
- [ ] Memory, backlog, idle load, and long sessions.

Set explicit pass/fail budgets. Correct misleading diagnostics so they distinguish
inference time, waiting, and capture backlog.

### 24. Run failure and endurance testing

Cover:

- [ ] Sleep/wake, lock/unlock, and shutdown during recording.
- [ ] Microphone loss and destination closure.
- [ ] Permission revocation and portal restart.
- [ ] Low memory, disk-full, slow providers, and GPU load failure.
- [ ] Rapid starts/stops and repeated sessions over extended use.

**Acceptance:** no unintended recording after wake, stuck lifecycle state,
unbounded growth, or unexplained transcript loss.

### 25. Validate every advertised platform/application combination

- [ ] Linux: each claimed desktop/session type and representative native, browser, Electron, and terminal targets.
- [ ] Windows: real UIA/SendInput behavior, elevation refusal, permissions, and installer operation.
- [ ] macOS: packaged microphone and Accessibility permissions, denial/recovery, native delivery, and signing requirements.
- [ ] Every advertised architecture and GPU backend.

The macOS microphone usage string exists in the current source; the remaining
task is packaged-app validation, not assuming the old metadata gap still exists.

## Priority 5 — Package, audit, and release

### 26. Make CI and release builds enforce the quality bar

- [ ] Run formatting, Clippy, Rust tests, JavaScript tests, and production builds.
- [ ] Build the actual shipping GPU configurations.
- [ ] Keep GUI/native checks available as isolated integration jobs or documented manual gates.
- [ ] Require validation before publishing; do not rely on compilation alone.
- [ ] Verify release workflow, build dependencies, version consistency, and artifact checksums.

### 27. Validate installers and distribution compatibility

- [ ] Choose and test minimum supported OS versions.
- [ ] Build Linux packages against an appropriate baseline; do not ship development-host ABI requirements as broad compatibility.
- [ ] Test `.deb`, AppImage, Windows, and macOS packages on clean machines as applicable.
- [ ] Test upgrade, uninstall/reinstall, moved executables, and retained models/settings/history.
- [ ] Complete signing/notarization for normal public distribution, or explicitly label limited unsigned previews.

### 28. Complete the security, privacy, and licence audit

- [ ] Review Tauri commands/capabilities and CSP.
- [ ] Audit dependencies, engine/model licences, redistribution rights, and notices.
- [ ] Verify logs and crash diagnostics exclude audio, transcript text, and credentials.
- [ ] Audit model-download integrity and settings-file handling.
- [ ] Review runtime-written launcher identity files and installed-app identity.
- [ ] Ensure packaged artifacts contain no tokens, personal data, fixtures, or scratch output.

### 29. Clean up the product tree and user documentation

- [ ] Move useful native/GUI fixtures into clearly developer-only locations; do not discard valuable regression tests simply to make the tree smaller.
- [x] Remove the outdated planning and incident documents from the initial Git baseline (user decision).
- [ ] Remove test titles and inconsistent release identity.
- [ ] Provide one current install/setup/permissions/troubleshooting guide.
- [ ] Publish the tested support matrix, English-only scope, privacy behavior, and known limits.
- [ ] Document a simple update and rollback policy.

### 30. Run final acceptance on the exact release artifacts

- [ ] Install the actual candidate package on a clean profile and an upgraded profile.
- [ ] Repeat the end-to-end workflow and relevant failure checks.
- [ ] Confirm the installed build identity matches the tested artifact.
- [ ] Publish only after its platform-specific gates pass.

**No rebuilding after final acceptance without revalidating the resulting artifact.**

## Recommended starting order

Start with items **1–3**, then the data-loss fixes in **4–7**, followed by the
readiness split in **9** and native delivery work in **10–12**. Item **8** remains
a prerequisite for relaxing delivery safeguards or claiming safe keyboard delivery.

The first release can be narrower than universal cross-platform support. It cannot
be narrower than truthful readiness, recoverable text, safe destination handling,
and repeatable real dictation on every platform we claim to support.
