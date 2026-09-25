#!/usr/bin/env sh
set -eu

# Installs the LLVM 22 development packages from apt.llvm.org on a Debian or
# Ubuntu root and exports LLVM_SYS_221_PREFIX for llvm-sys.
#
# usage: ci/setup-llvm-apt.sh [version] [suite]

VERSION="${1:-22}"
SUITE="${2:-}"

if [ -z "$SUITE" ]; then
    # shellcheck disable=SC1091
    SUITE="$(. /etc/os-release && printf '%s' "${VERSION_CODENAME:-}")"
fi

if [ -z "$SUITE" ]; then
    echo "unable to detect the distro codename, pass it as the second argument" >&2
    exit 1
fi

export DEBIAN_FRONTEND=noninteractive

echo "adding https://apt.llvm.org/$SUITE/ for llvm-toolchain-$SUITE-$VERSION"

apt-get update
apt-get install -y --no-install-recommends ca-certificates curl gnupg

KEYRING=/usr/share/keyrings/apt-llvm.gpg
curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key -o /tmp/llvm-snapshot.key
gpg --dearmor < /tmp/llvm-snapshot.key > "$KEYRING"
rm -f /tmp/llvm-snapshot.key

echo "deb [signed-by=$KEYRING] https://apt.llvm.org/$SUITE/ llvm-toolchain-$SUITE-$VERSION main" \
    > /etc/apt/sources.list.d/llvm.list

apt-get update

# llvm-22-dev does not depend on libpolly-22-dev and the system libraries named
# by llvm-config --system-libs need their development symlinks.
apt-get install -y --no-install-recommends \
    "llvm-$VERSION-dev" \
    "libpolly-$VERSION-dev" \
    libz3-dev \
    zlib1g-dev \
    libzstd-dev \
    libxml2-dev \
    libffi-dev \
    libedit-dev \
    libncurses-dev

LLVM_PREFIX="/usr/lib/llvm-$VERSION"
if [ ! -x "$LLVM_PREFIX/bin/llvm-config" ]; then
    echo "llvm-config not found under $LLVM_PREFIX" >&2
    exit 1
fi

echo "llvm-config $("$LLVM_PREFIX/bin/llvm-config" --version) at $LLVM_PREFIX"

if [ -n "${GITHUB_ENV:-}" ]; then
    echo "LLVM_SYS_221_PREFIX=$LLVM_PREFIX" >> "$GITHUB_ENV"
fi
