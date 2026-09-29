#!/usr/bin/env sh
set -eu

# Builds the release LSP binary. Unlike the compiler it needs no LLVM,
# so this runs on every platform job as-is.
#
# The archive version comes from the compiler workspace; the LSP crate
# version must match it or the release mixes versions.

cd "$(dirname "$0")/../lsp"

COMPILER_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' ../compiler/Cargo.toml | head -n 1)"
LSP_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"

if [ "$COMPILER_VERSION" != "$LSP_VERSION" ]; then
    echo "version drift: compiler $COMPILER_VERSION != lsp $LSP_VERSION" >&2
    exit 1
fi

cargo build --release
