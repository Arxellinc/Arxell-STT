#!/usr/bin/env bash
# Build directly from this checkout. Local tools and caches stay in .local/.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET="${ARXELL_TARGET_DIR:-$ROOT/.local/target}"
# GPU support is mandatory in the build graph (see src-tauri/Cargo.toml);
# there is deliberately no feature toggle to disable it.

GLSLANG_ROOT="${ARXELL_GLSLANG_ROOT:-$ROOT/.local/tooling/glslc/root}"
SPIRV_PREFIX="${ARXELL_SPIRV_PREFIX:-$ROOT/.local/tooling/spirv-prefix}"

if [ -d "$GLSLANG_ROOT/usr/bin" ]; then
  export PATH="$GLSLANG_ROOT/usr/bin:$PATH"
  export LD_LIBRARY_PATH="$GLSLANG_ROOT/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi
if [ -d "$SPIRV_PREFIX" ]; then
  export CMAKE_PREFIX_PATH="$SPIRV_PREFIX${CMAKE_PREFIX_PATH:+:$CMAKE_PREFIX_PATH}"
  export CPLUS_INCLUDE_PATH="$SPIRV_PREFIX/include${CPLUS_INCLUDE_PATH:+:$CPLUS_INCLUDE_PATH}"
  export C_INCLUDE_PATH="$SPIRV_PREFIX/include${C_INCLUDE_PATH:+:$C_INCLUDE_PATH}"
fi

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="$TARGET"
export ARXELL_MODEL="${ARXELL_MODEL:-$ROOT/models/parakeet-unified-en-0.6b-Q4_K_M.gguf}"
cd "$ROOT/src-tauri"

case "${1:-check}" in
  check) cargo check ;;
  build) cargo build ;;
  release) cargo build --release ;;
  package)
    [ "$(uname -s)" = Linux ] || { echo "package currently supports Linux only" >&2; exit 2; }
    BUNDLES="${2:-deb,appimage}"
    # Use the actual production build path, including Tauri's embedded frontend.
    npx --no-install tauri build --no-bundle --ci
    python3 "$ROOT/scripts/deb-depends.py" "$TARGET/release/arxell-speak" "$TARGET/linux-package.json"
    # Ubuntu's uutils cp currently breaks the GTK bundler's --parents copies.
    # Prefer installed GNU cp for this build only; do not change system tools.
    if command -v gnucp >/dev/null 2>&1; then
      mkdir -p "$TARGET/package-tools"
      ln -sf "$(command -v gnucp)" "$TARGET/package-tools/cp"
      export PATH="$TARGET/package-tools:$PATH"
    fi
    npx --no-install tauri bundle --config "$TARGET/linux-package.json" --bundles "$BUNDLES" --ci
    ;;
  run) cargo run ;;
  test) cargo test ;;
  fmt) cargo fmt --all ;;
  lint) cargo clippy --all-targets -- -D warnings ;;
  example)
    shift
    cargo run --example "$@"
    ;;
  *)
    echo "usage: $0 [check|build|release|package [deb,appimage]|run|test|fmt|lint|example <name> -- <args>]" >&2
    exit 2
    ;;
esac
