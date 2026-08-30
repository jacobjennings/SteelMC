#!/usr/bin/env sh
set -eu

export PATH="$HOME/.cargo/bin:$PATH"

output_dir=${1:-target/wasm-pkg}

cargo build -p steel-worldgen-wasm --target wasm32-unknown-unknown --release
wasm-bindgen \
    --target web \
    --out-dir "$output_dir" \
    target/wasm32-unknown-unknown/release/steel_worldgen_wasm.wasm

wasm_file="$output_dir/steel_worldgen_wasm_bg.wasm"
printf 'wasm-bindgen version: %s\n' "$(wasm-bindgen --version)"
printf 'WASM byte size: %s (%s)\n' "$(wc -c < "$wasm_file")" "$wasm_file"
