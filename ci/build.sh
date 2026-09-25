#!/usr/bin/env sh
set -eu

# Builds the release binary. llvm-sys probes static archives first and falls
# back to a shared libLLVM on its own, so a straight build usually works. When
# the static link fails even though archives were found (missing Polly or z3,
# for instance) the build is retried with the llvm-dynamic feature.
#
# The rpath points at ../lib/zeen next to the binary, where ci/package.sh puts
# bundled LLVM libraries. Windows gets no rpath, link.exe rejects it and MSVC
# only ever links LLVM statically.

cd "$(dirname "$0")/../compiler"

if [ -z "${LLVM_SYS_221_PREFIX:-}" ] && [ -n "${LLVM_PATH:-}" ]; then
    LLVM_SYS_221_PREFIX="$LLVM_PATH"
    export LLVM_SYS_221_PREFIX
fi

case "$(uname -s)" in
    Darwin)
        export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-rpath,@loader_path/../lib/zeen"
        ;;
    Linux)
        export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,--disable-new-dtags -C link-arg=-Wl,-rpath,\$ORIGIN/../lib/zeen"
        ;;
    FreeBSD | DragonFly)
        export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-rpath,\$ORIGIN/../lib/zeen"
        ;;
esac

if [ -n "${LLVM_SYS_221_PREFIX:-}" ]; then
    echo "LLVM_SYS_221_PREFIX=$LLVM_SYS_221_PREFIX"
fi

if ! cargo build --release -p zeen; then
    echo "static link failed, retrying with the llvm-dynamic feature"
    cargo build --release -p zeen --features llvm-dynamic
fi

ZEEN_BIN="target/release/zeen"
if [ -f "$ZEEN_BIN.exe" ]; then
    ZEEN_BIN="$ZEEN_BIN.exe"
fi

"$ZEEN_BIN" --version
