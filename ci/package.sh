#!/usr/bin/env sh
set -eu

# Assembles the release archive for one asset.
#
# usage: ci/package.sh <asset> <path to binary>

ASSET="${1:?usage: ci/package.sh <asset> <binary>}"
BIN="${2:?usage: ci/package.sh <asset> <binary>}"

cd "$(dirname "$0")/.."

if [ -z "${ZEEN_VERSION:-}" ]; then
    ZEEN_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' compiler/Cargo.toml | head -n 1)"
fi

if [ -z "$ZEEN_VERSION" ]; then
    echo "unable to determine the version" >&2
    exit 1
fi

if [ ! -f "$BIN" ]; then
    echo "binary not found: $BIN" >&2
    exit 1
fi

ROOT="zeen-$ZEEN_VERSION-$ASSET"
OUT="dist"

rm -rf "${OUT:?}/${ROOT:?}"
mkdir -p "$OUT/$ROOT/bin" "$OUT/$ROOT/share/zeen"

cp -R lib/std "$OUT/$ROOT/share/zeen/std"
cp LICENSE README.md "$OUT/$ROOT/"

BIN_DEST="$OUT/$ROOT/bin/zeen"
case "$BIN" in
    (*.exe)
        BIN_DEST="$OUT/$ROOT/bin/zeen.exe"
        ;;
esac
cp "$BIN" "$BIN_DEST"

# resolve shared library dependencies, a statically linked binary reports none
OS="$(uname -s)"
DEPS=""
case "$OS" in
    (Darwin)
        DEPS="$(otool -L "$BIN_DEST" | sed 1d | awk '{print $1}')"
        ;;
    (*)
        if command -v ldd >/dev/null 2>&1; then
            if ldd "$BIN_DEST" | grep -q "not found"; then
                ldd "$BIN_DEST" | grep "not found" >&2
                echo "the packaged binary has unresolved shared libraries" >&2
                exit 1
            fi
            DEPS="$(ldd "$BIN_DEST" | awk '/=>/ {print $3}')"
        fi
        ;;
esac

MODE="static"
case "$OS" in
    (Darwin)
        for dep in $DEPS; do
            case "$dep" in
                (*libLLVM* | *libclang-cpp*)
                    MODE="dynamic"
                    ;;
            esac
        done

        RPATHS="$(otool -l "$BIN_DEST" | awk '/cmd LC_RPATH/{getline; print $2}')"
        BUNDLED=0

        # /usr/lib and /System stay external, the rest ships inside the archive
        for dep in $DEPS; do
            case "$dep" in
                (/usr/lib/* | /System/*)
                    continue
                    ;;
            esac

            NAME="$(basename "$dep")"
            SRC=""

            case "$dep" in
                (/*)
                    if [ -f "$dep" ]; then
                        SRC="$dep"
                    fi
                    ;;
                (*)
                    for RPATH in $RPATHS; do
                        case "$RPATH" in
                            (@loader_path*)
                                RPATH="$(dirname "$BIN_DEST")${RPATH#@loader_path}"
                                ;;
                            (@executable_path*)
                                RPATH="$(dirname "$BIN_DEST")${RPATH#@executable_path}"
                                ;;
                        esac
                        if [ -f "$RPATH/$NAME" ]; then
                            SRC="$RPATH/$NAME"
                            break
                        fi
                    done
                    ;;
            esac

            if [ -z "$SRC" ]; then
                echo "the packaged binary has an unresolved shared library: $dep" >&2
                exit 1
            fi

            mkdir -p "$OUT/$ROOT/lib/zeen"
            cp -L "$SRC" "$OUT/$ROOT/lib/zeen/$NAME"
            echo "bundled $NAME"
            BUNDLED=1
        done

        # point the binary and the bundled dylibs at the copies next to them
        if [ "$BUNDLED" = "1" ]; then
            for target in "$BIN_DEST" "$OUT/$ROOT/lib/zeen"/*.dylib; do
                if [ ! -f "$target" ]; then
                    continue
                fi

                for dep in $(otool -L "$target" | sed 1d | awk '{print $1}'); do
                    BASE="$(basename "$dep")"
                    if [ ! -f "$OUT/$ROOT/lib/zeen/$BASE" ]; then
                        continue
                    fi

                    if [ "$target" = "$BIN_DEST" ]; then
                        REF="@loader_path/../lib/zeen/$BASE"
                    else
                        REF="@loader_path/$BASE"
                    fi
                    install_name_tool -change "$dep" "$REF" "$target"
                done

                if command -v codesign >/dev/null 2>&1; then
                    codesign --force --sign - "$target"
                fi
            done
        fi
        ;;
    (*)
        for dep in $DEPS; do
            case "$dep" in
                (*libLLVM* | *libclang-cpp*)
                    MODE="dynamic"
                    ;;
            esac
        done

        if [ "$MODE" = "dynamic" ]; then
            mkdir -p "$OUT/$ROOT/lib/zeen"

            for dep in $DEPS; do
                if [ ! -f "$dep" ]; then
                    continue
                fi

                NAME="$(basename "$dep")"
                case "$NAME" in
                    (linux-vdso* | ld-* | libc.so* | libc.musl* | libm.so* | libgcc_s*)
                        continue
                        ;;
                esac

                cp -L "$dep" "$OUT/$ROOT/lib/zeen/$NAME"
                echo "bundled $NAME"
            done
        fi
        ;;
esac

echo "link mode: $MODE"

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    {
        echo "### $ROOT"
        echo ""
        echo "- link mode: \`$MODE\`"
        echo "- binary: \`$(du -h "$BIN_DEST" | awk '{print $1}')\`"
    } >> "$GITHUB_STEP_SUMMARY"
fi

# exercises the rpath and the exe relative std lookup of the copy
"$BIN_DEST" --version

case "$OS" in
    (MINGW* | MSYS* | CYGWIN*)
        powershell -NoProfile -Command \
            "Compress-Archive -Path '$OUT/$ROOT' -DestinationPath '$OUT/$ROOT.zip' -Force"
        ARCHIVE="$OUT/$ROOT.zip"
        ;;
    (*)
        (cd "$OUT" && tar -czf "$ROOT.tar.gz" "$ROOT")
        ARCHIVE="$OUT/$ROOT.tar.gz"
        ;;
esac

echo "archive: $ARCHIVE ($(du -h "$ARCHIVE" | awk '{print $1}'))"
