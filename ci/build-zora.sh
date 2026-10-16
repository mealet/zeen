#!/usr/bin/env sh
set -eu

REPO="${ZORA_REPO:-https://github.com/mealet/zora}"
REF="${ZORA_REF:-}"
SRC="${ZORA_SRC:-}"

cd "$(dirname "$0")/.."

OUT="$(pwd)/dist/zora"
rm -rf "$OUT"

export CARGO_NET_GIT_FETCH_WITH_CLI="${CARGO_NET_GIT_FETCH_WITH_CLI:-true}"

if [ -n "$SRC" ]; then
    cargo install --path "$SRC" --locked --root "$OUT" --force zora
elif [ -n "$REF" ]; then
    cargo install --git "$REPO" --locked --tag "$REF" --root "$OUT" --force zora
else
    cargo install --git "$REPO" --locked --root "$OUT" --force zora
fi

ZORA_BIN="$OUT/bin/zora"
if [ -f "$ZORA_BIN.exe" ]; then
    ZORA_BIN="$ZORA_BIN.exe"
fi

COMPILER_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' compiler/Cargo.toml | head -n 1)"
ZORA_VERSION="$("$ZORA_BIN" --version | sed 's/^zora //')"

if [ "$COMPILER_VERSION" != "$ZORA_VERSION" ]; then
    echo "version drift: compiler $COMPILER_VERSION != zora $ZORA_VERSION" >&2
    exit 1
fi

"$ZORA_BIN" --version
