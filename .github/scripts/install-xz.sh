#!/usr/bin/env bash
# Put the xz the .xz container and encoder tests are written against on PATH,
# on every runner these workflows use.
#
# tests/xz_container.rs drives the system `xz` to produce the vectors it
# decodes, so the tests depend on whichever xz the image happens to ship.
# ubuntu-24.04 shipped xz 5.4.5 - the release Debian reverted to after the
# CVE-2024-3094 backdoor in 5.6.0/5.6.1 - which rejects filters that arrived in
# 5.6 (--riscv among them), so those tests failed on the runner while passing
# everywhere the developer works. ubuntu-26.04 ships 5.8.3, but the image's
# version is still not ours to choose. macOS and Windows have no `xz` at all,
# which is why `tests/xz_encoder.rs` used to skip its external-decode tests on
# two of the four platforms the writer is meant to be proved on. Rather than
# skip, install a known xz ahead of whatever the image has.
#
# The version and its checksum are pinned, the way every action in these
# workflows is pinned by SHA: a build tool fetched as "latest" over the network
# is an unreviewed input, and this particular tarball has a history that makes
# that more than a theoretical objection. 5.8.3 is well past the compromised
# releases, and is the version the tests are known to pass against locally.
#
# Unix builds it from that tarball. The build is static-only. Installing a
# shared liblzma into /usr/local would shadow the system one for everything
# that links it - tar, dpkg, systemd - and a CI job has no business doing that.
# Only the `xz` binary is wanted.
#
# Windows takes the project's own release build instead: the runner's bash is
# Git for Windows', which has no `make`, so there is no autotools build to run
# there. The zip is pinned and digest-checked exactly as the tarball is, and it
# is built by the same people from the same release.
set -euo pipefail

VERSION=5.8.3
SHA256=3d3a1b973af218114f4f889bbaa2f4c037deaae0c8e815eec381c3d546b974a0
WINDOWS_SHA256=8d0048ee51177b11ef1613959c2a268c951f4e7f6fb3706e681e00e34bb6d5e3
BASE="https://github.com/tukaani-project/xz/releases/download/v${VERSION}"

case "$(uname -s)" in
  Linux | Darwin) os=unix ;;
  *) os=windows ;;
esac

# `sha256sum` is GNU's and `shasum` is what macOS ships; take whichever is
# there rather than assuming the Linux one everywhere.
check_sha256() {
  local file="$1" want="$2" got
  if command -v sha256sum > /dev/null; then
    got="$(sha256sum "$file" | cut -d' ' -f1)"
  else
    got="$(shasum -a 256 "$file" | cut -d' ' -f1)"
  fi
  if [ "$got" != "$want" ]; then
    echo "$file: SHA-256 $got, pinned $want" >&2
    exit 1
  fi
}

echo "xz before: $(xz --version 2> /dev/null | head -n1 || echo 'none on PATH')"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

if [ "$os" = unix ]; then
  name="xz-${VERSION}.tar.gz"
  curl --fail --silent --show-error --location --retry 3 --output "$name" "${BASE}/${name}"
  check_sha256 "$name" "$SHA256"
  tar xzf "$name"
  cd "xz-${VERSION}"

  if command -v nproc > /dev/null; then jobs="$(nproc)"; else jobs="$(sysctl -n hw.ncpu)"; fi
  ./configure --prefix=/usr/local --disable-shared --enable-static --disable-doc --quiet
  make -j"$jobs" --silent
  sudo make install --silent
else
  # The zip carries bin_x86-64/ - xz.exe and its friends - and bin_i686-sse2/.
  name="xz-${VERSION}-windows.zip"
  curl --fail --silent --show-error --location --retry 3 --output "$name" "${BASE}/${name}"
  check_sha256 "$name" "$WINDOWS_SHA256"
  # Git Bash PATH uses colons as separators; a native drive-letter path
  # would split into unrelated entries and leave the image's xz selected.
  dest="$(cygpath -u "${RUNNER_TEMP:-$PWD}")/xz-${VERSION}"
  rm -rf "$dest"
  mkdir -p "$dest"
  # Not every Git for Windows image has `unzip`; PowerShell is always there.
  if command -v unzip > /dev/null; then
    unzip -q "$name" -d "$dest"
  else
    powershell -NoProfile -Command \
      "Expand-Archive -LiteralPath '$(cygpath -w "$PWD/$name")' -DestinationPath '$(cygpath -w "$dest")' -Force"
  fi
  bin="$dest/bin_x86-64"
  test -f "$bin/xz.exe" || {
    echo "no xz.exe in $bin" >&2
    exit 1
  }
  # Ahead of anything the image has, for every later step of the job.
  if [ -n "${GITHUB_PATH:-}" ]; then
    cygpath -w "$bin" >> "$GITHUB_PATH"
  fi
  export PATH="$bin:$PATH"
fi

# /usr/local/bin precedes /usr/bin on the runners, so this is what the tests
# will spawn. Fail loudly here rather than three test failures later.
hash -r
echo "xz now on PATH: $(command -v xz) -> $(xz --version | head -n1)"
xz --version | head -n1 | grep -q " ${VERSION}$"
