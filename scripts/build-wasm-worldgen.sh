#!/usr/bin/env sh
set -eu

export PATH="$HOME/.cargo/bin:$PATH"

usage() {
    cat <<'EOF'
Usage: scripts/build-wasm-worldgen.sh [OUTPUT_DIRECTORY]

Environment:
  WASM_SIMD=0|1     Disable or enable simd128 (default: 1).
  WASM_OPT=LEVEL    Run wasm-opt after wasm-bindgen (for example, -Oz or -O3).

SIMD compatibility: WebAssembly SIMD is supported by Chrome 91+, Firefox 89+,
and Safari 16.4+. Set WASM_SIMD=0 for older browsers.
EOF
}

case ${1:-} in
    -h|--help)
        usage
        exit 0
        ;;
esac

output_dir=${1:-target/wasm-pkg}
wasm_simd=${WASM_SIMD:-1}

if [ "$wasm_simd" = 1 ]; then
    export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=+simd128"
elif [ "$wasm_simd" != 0 ]; then
    printf 'WASM_SIMD must be 0 or 1, got: %s\n' "$wasm_simd" >&2
    exit 2
fi

cargo build -p steel-worldgen-wasm --target wasm32-unknown-unknown --release
wasm-bindgen \
    --target web \
    --out-dir "$output_dir" \
    target/wasm32-unknown-unknown/release/steel_worldgen_wasm.wasm

wasm_file="$output_dir/steel_worldgen_wasm_bg.wasm"
if [ -n "${WASM_OPT:-}" ]; then
    if ! command -v wasm-opt >/dev/null 2>&1; then
        printf 'WASM_OPT=%s requested, but wasm-opt is not available on PATH\n' "$WASM_OPT" >&2
        exit 1
    fi
    optimized_file="$wasm_file.optimized"
    wasm-opt "$WASM_OPT" "$wasm_file" -o "$optimized_file"
    mv "$optimized_file" "$wasm_file"
fi

printf 'wasm-bindgen version: %s\n' "$(wasm-bindgen --version)"
printf 'simd128 enabled: %s\n' "$wasm_simd"
printf 'wasm-opt level: %s\n' "${WASM_OPT:-none}"
printf 'WASM byte size: %s (%s)\n' "$(wc -c < "$wasm_file")" "$wasm_file"
