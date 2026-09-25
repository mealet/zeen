#!/usr/bin/env sh
set -eu

# Installs zeen from a GitHub release.
#
# usage: install.sh [--version vX.Y.Z] [--prefix DIR] [--system] [--musl|--gnu]
#                   [--help]
#
# ZEEN_REPO and ZEEN_API_URL override the release source, which is only
# useful for testing.

REPO="${ZEEN_REPO:-mealet/zeen}"
API_URL="${ZEEN_API_URL:-https://api.github.com}"

PREFIX=""
VERSION_REF=""
LIBC=""

usage() {
    cat <<'EOF'
usage: install.sh [options]

  --version vX.Y.Z   install a specific release instead of the latest one
  --prefix DIR       install into DIR (default: ~/.local)
  --system           install into /usr/local
  --musl             pick the musl asset (linux only, detected by default)
  --gnu              pick the gnu asset (linux only, detected by default)
  --help             show this message

Environment: ZEEN_VERSION and ZEEN_PREFIX work like --version and --prefix,
GITHUB_TOKEN is used for the API request when set.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        (--version)
            VERSION_REF="${2:?--version needs a value}"
            shift 2
            ;;
        (--prefix)
            PREFIX="${2:?--prefix needs a value}"
            shift 2
            ;;
        (--system)
            PREFIX="/usr/local"
            shift
            ;;
        (--musl)
            LIBC="musl"
            shift
            ;;
        (--gnu)
            LIBC="gnu"
            shift
            ;;
        (--help | -h)
            usage
            exit 0
            ;;
        (*)
            echo "unknown option: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

PREFIX="${PREFIX:-${ZEEN_PREFIX:-$HOME/.local}}"
VERSION_REF="${VERSION_REF:-${ZEEN_VERSION:-}}"

die() {
    echo "install.sh: $1" >&2
    exit 1
}

case "$(uname -s)" in
    (Linux)
        OS="linux"
        ;;
    (Darwin)
        OS="darwin"
        ;;
    (MINGW* | MSYS* | CYGWIN*)
        OS="windows"
        ;;
    (*)
        die "unsupported operating system: $(uname -s)"
        ;;
esac

case "$(uname -m)" in
    (x86_64 | amd64)
        ARCH="x86_64"
        ;;
    (aarch64 | arm64)
        ARCH="aarch64"
        ;;
    (i686 | i386)
        ARCH="i686"
        ;;
    (*)
        die "unsupported architecture: $(uname -m)"
        ;;
esac

if [ "$OS" = "linux" ] && [ -z "$LIBC" ]; then
    LIBC="gnu"
    for loader in "/lib/ld-musl-$ARCH.so.1" "/usr/lib/ld-musl-$ARCH.so.1"; do
        if [ -e "$loader" ]; then
            LIBC="musl"
        fi
    done
    if ldd --version 2>&1 | grep -qi musl; then
        LIBC="musl"
    fi
fi

case "$OS" in
    (linux)
        if [ "$LIBC" = "musl" ] && [ "$ARCH" != "x86_64" ]; then
            die "no musl build for $ARCH, only x86_64 is published"
        fi
        ASSET="linux-$ARCH-$LIBC"
        ;;
    (darwin)
        ASSET="darwin-$ARCH"
        ;;
    (windows)
        ASSET="windows-$ARCH"
        ;;
esac

if [ -z "$VERSION_REF" ]; then
    RELEASE_URL="$API_URL/repos/$REPO/releases/latest"
else
    case "$VERSION_REF" in
        (v*) ;;
        (*) VERSION_REF="v$VERSION_REF" ;;
    esac
    RELEASE_URL="$API_URL/repos/$REPO/releases/tags/$VERSION_REF"
fi

if [ -n "${GITHUB_TOKEN:-}" ]; then
    RESPONSE="$(curl -fsSL \
        -H "Authorization: Bearer $GITHUB_TOKEN" \
        -H "Accept: application/vnd.github+json" \
        "$RELEASE_URL")" ||
        die "unable to read the release from $RELEASE_URL, is there a published release?"
else
    RESPONSE="$(curl -fsSL \
        -H "Accept: application/vnd.github+json" \
        "$RELEASE_URL")" ||
        die "unable to read the release from $RELEASE_URL, is there a published release?"
fi

VERSION_REF="$(printf '%s' "$RESPONSE" | tr -d '\n' | grep -o '"tag_name": *"[^"]*"' | head -n 1 | sed 's/.*"tag_name": *"//; s/"$//')"
[ -n "$VERSION_REF" ] || die "could not read the release tag from the API response"

URL="$(printf '%s' "$RESPONSE" | tr -d '\n' | grep -o '"browser_download_url": *"[^"]*"' |
    sed 's/.*"browser_download_url": *"//; s/"$//' |
    grep -e "-$ASSET\.zip$" -e "-$ASSET\.tar\.gz$" | head -n 1)"
[ -n "$URL" ] || die "release $VERSION_REF carries no build for $ASSET"

FILE="${URL##*/}"

echo "release: $VERSION_REF"
echo "asset:   $FILE"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "downloading $URL"
curl -fL --retry 3 -o "$TMP/$FILE" "$URL" || die "download failed for $FILE"

mkdir -p "$TMP/unpack"
case "$FILE" in
    (*.zip)
        command -v unzip >/dev/null 2>&1 || die "unzip is required to extract $FILE"
        unzip -q "$TMP/$FILE" -d "$TMP/unpack"
        ;;
    (*)
        tar -xzf "$TMP/$FILE" -C "$TMP/unpack"
        ;;
esac

ROOT="$(find "$TMP/unpack" -mindepth 1 -maxdepth 1 -type d | head -n 1)"
[ -n "$ROOT" ] || die "the archive did not contain an install directory"
[ -e "$ROOT/bin/zeen" ] || [ -e "$ROOT/bin/zeen.exe" ] || die "the archive does not contain bin/zeen"

echo "installing into $PREFIX"

mkdir -p "$PREFIX/bin" "$PREFIX/share/zeen"
cp "$ROOT"/bin/zeen* "$PREFIX/bin/"

rm -rf "$PREFIX/share/zeen/std"
cp -R "$ROOT/share/zeen/std" "$PREFIX/share/zeen/std"

if [ -d "$ROOT/lib/zeen" ]; then
    mkdir -p "$PREFIX/lib/zeen"
    cp -R "$ROOT/lib/zeen/." "$PREFIX/lib/zeen/"
fi

"$PREFIX/bin/zeen" --version

case ":$PATH:" in
    (*":$PREFIX/bin:"*)
        echo "done, $PREFIX/bin is on PATH"
        ;;
    (*)
        echo "done, add $PREFIX/bin to PATH to run zeen"
        echo "  export PATH=\"$PREFIX/bin:\$PATH\""
        ;;
esac
