#!/usr/bin/env sh
set -eu

repo="${OBSCURA_GATEWAY_REPO:-l3wi/obscura-gateway}"
install_dir="${INSTALL_DIR:-/usr/local/bin}"

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "${os}-${arch}" in
    Linux-x86_64) printf '%s\n' "x86_64-unknown-linux-gnu" ;;
    Darwin-x86_64) printf '%s\n' "x86_64-apple-darwin" ;;
    Darwin-arm64|Darwin-aarch64) printf '%s\n' "aarch64-apple-darwin" ;;
    MINGW*-x86_64|MSYS*-x86_64|CYGWIN*-x86_64) printf '%s\n' "x86_64-pc-windows-msvc" ;;
    *) printf '%s\n' "unsupported target: ${os}-${arch}" >&2; exit 1 ;;
  esac
}

latest_version() {
  curl -fsSL "https://api.github.com/repos/${repo}/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' \
    | head -n 1
}

target="${TARGET:-$(detect_target)}"
version="${VERSION:-$(latest_version)}"

if [ -z "${version}" ]; then
  printf '%s\n' "could not determine latest release; set VERSION=vX.Y.Z" >&2
  exit 1
fi

if [ ! -d "${install_dir}" ] || [ ! -w "${install_dir}" ]; then
  install_dir="${HOME}/.local/bin"
  mkdir -p "${install_dir}"
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT INT TERM

asset="obscura-cli-${version}-${target}.tar.gz"
url="https://github.com/${repo}/releases/download/${version}/${asset}"
bin_name="obscura-cli"

case "${target}" in
  *windows*) bin_name="obscura-cli.exe" ;;
esac

curl -fsSL "${url}" -o "${tmp_dir}/${asset}"
tar -xzf "${tmp_dir}/${asset}" -C "${tmp_dir}"
install -m 0755 "${tmp_dir}/obscura-cli-${version}-${target}/${bin_name}" "${install_dir}/${bin_name}"

printf 'installed %s to %s/%s\n' "${bin_name}" "${install_dir}" "${bin_name}"
