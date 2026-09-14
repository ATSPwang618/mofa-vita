#!/usr/bin/env bash
# Build mofa-vita-krkr.vpk from WSL2.
#
#   wsl -d <distro> -u root bash /mnt/<drive>/<path>/mofa-vita-krkr/scripts/build-vita-wsl.sh
#
# A non-interactive login shell does not read ~/.bashrc, so VitaSDK is exported
# here before delegating to the repository's own release pipeline.  Missing
# prerequisites are reported with the exact command that installs them; this
# script never installs anything by itself.
set -euo pipefail

: "${VITASDK:=/opt/vitasdk}"
export VITASDK
export PATH="$VITASDK/bin:$PATH"
export JOBS="${JOBS:-$(nproc)}"

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)

missing=()
for tool in cmake git make g++ pkg-config; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("host tool: $tool")
done
for pkg in libcurl4-openssl-dev zlib1g-dev libpng-dev libsqlite3-dev \
           libssl-dev libfreetype-dev nasm; do
    dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null |
        grep -q "install ok installed" || missing+=("host package: $pkg")
done
for pkg in boost libarchive vitaGL openal-soft opusfile; do
    vdpm list "$pkg" 2>/dev/null | grep -q "^$pkg " ||
        missing+=("vitasdk package: $pkg")
done
[[ -x $VITASDK/bin/arm-vita-eabi-gcc ]] ||
    missing+=("VitaSDK toolchain at $VITASDK")

if ((${#missing[@]})); then
    echo "Missing build prerequisites:" >&2
    printf '  %s\n' "${missing[@]}" >&2
    cat >&2 <<'EOF'

Install them with:
  apt-get install -y libcurl4-openssl-dev zlib1g-dev libpng-dev \
      libsqlite3-dev libssl-dev libfreetype-dev nasm
  vdpm install boost libarchive vitaGL openal-soft opusfile
EOF
    exit 1
fi

exec bash "$repo_root/scripts/build-vita.sh"
