# Arxell Speak

Local-first dictation for Arxell. Rust/Tauri host, transcribe.cpp engine, one
curated model at a time.

Status: in-app dictation, local history, model setup, settings, local API, and
host-owned cross-app streaming delivery are implemented. Linux delivery has been
checked against an isolated real GTK field; Windows/macOS adapters have been
cross-target type-checked, not hardware-validated. Universal all-platform
production stability is not yet established. See `docs/DESKTOP-DELIVERY.md` for
permissions, measured evidence, limits and the remaining release gates.

## What is here

- `src-tauri/` — Rust host. Flat modules: `session`, `cue`, `segmenter`,
  `capture`, `mic`, `engine`, `delivery`, `submit`, `target`, `settings`,
  `host_api`, `ui`. `platform/` is the only subdirectory.
- `ui/` — vanilla HTML/CSS/JS, no frontend framework.
- `dev-build.sh` — builds directly from this checkout; caches stay in `.local/`.
- `docs/build-plan.md` — toolchain, milestones, CI and GPU policy.
- `.github/workflows/` — CPU test/build for Linux/Windows/macOS and tagged
  release builds (Linux Vulkan, macOS Metal, Windows CPU).
- `docs/review/` — source review, interfaces, tests, runtime results and decisions.
- `docs/IMPLEMENTATION-PLAN.md` — remaining phases and acceptance gates.

## Build and run

The build machine needs Rust, Tauri Linux libraries, cmake, pkg-config and
Vulkan tools. See `build-plan.md` for the exact install commands.

```bash
./dev-build.sh check
./dev-build.sh build
./dev-build.sh run
./dev-build.sh test
```

The canonical working directory is `/home/user/Projects/arxell-stt`.
There is no network-share mirror step. Local shader tools, research copies and
archived prototypes live in ignored `.local/`; build output is `.local/target/`.
See `MIGRATION.md` for the consolidation record.

## Linux installation packages

Use the `.deb` installer on Debian/Ubuntu, or run the portable `.AppImage`;
end users do not need Rust, Node.js, or the source checkout. Open the `.deb` with
your package installer, or install it with `sudo apt install ./"Arxell Speak_0.1.1_amd64.deb"`,
then launch **Arxell Speak** from the application menu. For AppImage, enable its
executable permission and open it. Existing app settings/history are preserved;
the speech model is separate from the installer.

To **build** packages on Linux with the build prerequisites installed:

```bash
npm ci
./dev-build.sh package                 # .deb and AppImage
./dev-build.sh package deb             # .deb only
```

Artifacts are under `.local/target/release/bundle/{deb,appimage}/`.
The helper uses Tauri's production build and derives Debian library dependencies
from the actual executable using `dpkg-shlibdeps` (requires `dpkg-dev`). This
includes audio, Vulkan, and the build distribution's OpenSSL ABI. A package built
on a newer distribution is not automatically compatible with older ones; use an
appropriate older build baseline for broad release compatibility. If available,
GNU `cp` is used locally for bundling to avoid an Ubuntu uutils `--parents` bug;
system tool settings are not changed.

## Model

Default: `parakeet-unified-en-0.6b` Q4_K_M, 477 MB, Efficient streaming preset
(1.2 s lookahead). Responsive (0.72 s) and Accurate (2.08 s) are also available.

- Expected SHA-256:
  `a8bf3de2b393bd14ead5a858c3748d5e3b07a20fdeabdd3b498fba4f463fa929`
- Default local path:
  `models/parakeet-unified-en-0.6b-Q4_K_M.gguf` (the helper exports the absolute path)

On first run the app downloads the curated default automatically (477 MB,
resumable, SHA-256 verified before install) into its app-data directory;
progress is shown in the setup panel and can be cancelled or retried. A
user-chosen custom model path in Settings is never replaced by the automatic
download.

The app uses one model at a time. `Engine` probes the platform GPU, falls back to
CPU once at load, and reports the actual backend/device. Advanced settings expose
Auto / CPU only / GPU only.

Build features: Linux `--features vulkan`, macOS `--features metal`, Windows
`--features vulkan` (or the default CPU build). `dev-build.sh` selects the local
platform automatically.

## Dictation and local history

The default VAD-gated mode streams audio while you speak and finalizes each
utterance at a pause; it no longer waits for a complete phrase before decoding.
The model is preloaded at startup. Cold model loading and CPU-only operation can
still take longer; wait for the green Ready indicator before measuring latency.
Enable metadata diagnostics to log `start_to_text_ms` for live sessions.
Efficient uses Responsive buffering for the first utterance, then efficient
chunks for subsequent utterances; Accurate is left unchanged.

History is shown by default above the current transcript. Older cards show a
timestamp, one-line preview and resume/save/copy actions; click the preview to
expand it. History scrolls independently, keeping the current transcript and
recording controls visible, with at least four lines for the active text.

Settings → Transcription history retains 3, 5, **10 (default)**, 20, 100 or
unlimited older transcriptions. The current transcript is separate from that
limit. History and the last completed/edited draft persist in the app's local
WebView IndexedDB storage; audio is not saved. Hiding history does not disable
storage. Reducing the limit permanently removes older records. Use Clear history
to delete saved history without deleting the current transcript. Unlimited
history is loaded in pages of 50 to avoid rendering the entire archive at once.

## Typing into other applications

Under **Settings → Shortcuts & delivery**, click **Record shortcut** beside
**Dictation shortcut**, press the desired combination, then click **Stop recording**
to save it automatically. Enter also confirms; Escape, leaving Settings, or
switching windows cancels and keeps the previous binding. On Wayland, approve
the system shortcut dialog; use **Authorize / retry** if needed. Clear the field
to disable the shortcut. Dictation normally stops after three seconds of silence;
pressing the shortcut again ends it early. System-reserved combinations may not
be available.

**Stream dictation into other applications** and **Allow typing into terminals**
are together under Settings → Shortcuts & delivery. Both default to on in the
current unreleased source; existing explicit off settings remain off until changed. Use the global shortcut
with the destination focused, or click the microphone and then click an external
text field. The app need not be foreground or restored to deliver text.

Stable words are streamed by a host worker; tentative text stays in Arxell.
Delivery binds one target, stops on failed focus/cursor/text checks, never retries
ambiguous inserts, and never automatically submits. Press Enter yourself.

Wayland uses desktop portals for global shortcuts and keyboard access (including
Electron/browser editors without direct editable-text support);
approve the relevant system dialogs. macOS requires Accessibility permission.
Windows password/elevated targets are refused. Terminals cannot expose secure
prompt or input-mode guarantees: use only the intended input area. No clipboard
is used. See **[Desktop delivery and release gates](docs/DESKTOP-DELIVERY.md)**.

After rebuilding, **quit all old instances** before opening the new executable;
a running old process does not pick up a replaced binary. This build is identified
in the Settings footer. Released v0.1.1 uses `shortcut-recorder-1`; the current
test candidate 0.1.2-rc.3 uses `layout-setup-3`, with a 900px settings cap and
slim outstanding-access banner. Its delivery changes are not yet verified
microphone-to-Opencode. See [current setup/layout evidence](docs/LAYOUT-SETUP-3.md)
and [the original delivery test](docs/DELIVERY-TEST-1.md). Portal consent now waits
without Arxell's old two-minute approval deadline. No replacement installer has
been released from this working tree.

Production direction: [simplification review](docs/SIMPLIFICATION-REVIEW.md),
[stricter-than-necessary constraints](docs/STRICTER-THAN-NECESSARY.md), and
[production cleanup plan](docs/PRODUCTION-CLEANUP.md).

## Tests and smoke examples

```bash
./dev-build.sh test
./dev-build.sh example smoke -- <model.gguf> <16k-mono.wav>
./dev-build.sh example stream_smoke -- <model.gguf> <16k-mono.wav> 320
./dev-build.sh example cue_smoke -- <model.gguf> <cue-fixture.wav> 320
./dev-build.sh example cue_fixture -- <model.gguf> <cue-fixture.wav> 320
./dev-build.sh example mic_smoke          # opens the default input device
./dev-build.sh example replay -- ../tests/fixtures/jfk.wav gpu realtime startup
node --test tests/shortcut.test.mjs       # recorder key conversion
/usr/bin/python3 tests/ui-shortcuts.py    # recorder interaction/recovery, mock backend
/usr/bin/python3 tests/ui-history.py      # GTK/WebKit integration, mock backend
/usr/bin/python3 tests/usability-probes.py # observational UX probes, no real input/files
/usr/bin/python3 tests/desktop-field.py   # isolated real GTK field, no global keys
/usr/bin/python3 tests/desktop-electron.py # real AT-SPI; renderer input seam, NOT native keys
```

`smoke` checks batch transcription. `stream_smoke` checks committed-text
immutability and final/committed divergence. `cue_smoke` prints committed
updates. `cue_fixture` runs the VAD-to-text segmenter plus cue resolver; it is
the S1 cue-isolation check. Synthetic fixtures live in
`tests/fixtures/`.

## Usability review

**[Real-world usability review](docs/USABILITY-REVIEW.md)** is the prioritized,
one-issue-at-a-time backlog from the end-to-end workflow review. It distinguishes
reproduced problems, source-confirmed gaps, risks needing hardware validation,
and optional product decisions. The shortcut recorder is implemented; the
remaining findings are not silently treated as fixed.

## Current limits

- Microphone capture via cpal and Earshot VAD are implemented. Recorded-audio
  checks do not establish end-to-end human microphone latency.
- The live loop auto-stops on a matching cue after the confirmation pause. This
  is unit-tested with synthetic frames, not yet with human speech.
- Cross-app streaming does not automatically submit or erase already delivered
  text. Unverifiable fields retain the transcript in Arxell.
- Native Windows/macOS adapters are implemented and type-checked; real app,
  permission and microphone testing on those platforms remains required.
- Linux Wayland terminal typing and global shortcuts require compositor consent;
  permission-dependent end-to-end tests remain a release gate.
- Focus/input-mode races and uneven accessibility support prevent a universal
  "works in every app" guarantee. See the desktop delivery document.
- S3 technical-corpus accuracy has not been measured.

## Documentation

All docs live in `docs/`.

- `docs/IMPLEMENTATION-PLAN.md` — remaining work in five phases and acceptance gates.
- `docs/API.md` — local transcription API protocol and limits.
- `docs/build-plan.md` — build and milestones.
- `docs/VOICE-TEST.md` — first live voice-test checklist.
- `docs/review/recommendation.md` — foundation, model and next tests.
- `docs/review/architecture.md` — boundaries, state machine, cue contract.
- `docs/review/runtime-results.md` — what has actually run.
- `docs/review/decision-log.md` — settled decisions and open gates.
