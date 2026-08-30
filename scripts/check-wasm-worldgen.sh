#!/usr/bin/env sh
set -eu

cargo check -p steel-worldgen --target wasm32-unknown-unknown
