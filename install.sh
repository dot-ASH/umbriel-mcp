#!/bin/sh
set -eu

releases="${UMBRIEL_MCP_RELEASES:-https://github.com/dot-ASH/umbriel-mcp/releases}"
releases="${releases%/}"
install_dir="${UMBRIEL_MCP_INSTALL_DIR:-$HOME/.local/bin}"
target="$install_dir/umbriel-mcp"

release_key='-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAb6vD24XeZrULn45eFBX1DOtynS2RdegYIHF9FZw5MYo=
-----END PUBLIC KEY-----'

fail() {
  echo "umbriel-mcp install: $*" >&2
  exit 1
}

case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) fail "no build for $(uname -m); releases exist for x86_64 and aarch64 Linux" ;;
esac

for tool in curl tar sha256sum openssl; do
  command -v "$tool" >/dev/null 2>&1 || fail "$tool is needed; install it with your package manager"
done

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT INT TERM

curl -fsSL "$releases/latest/download/SHA256SUMS" -o "$work/SHA256SUMS" ||
  fail "could not download the release checksums from $releases"
curl -fsSL "$releases/latest/download/SHA256SUMS.sig" -o "$work/SHA256SUMS.sig" ||
  fail "the latest release is not signed (it has no SHA256SUMS.sig); nothing was installed"

if [ -n "${UMBRIEL_MCP_RELEASE_KEY:-}" ]; then
  cp "$UMBRIEL_MCP_RELEASE_KEY" "$work/release-key.pem"
else
  printf '%s\n' "$release_key" >"$work/release-key.pem"
fi
openssl pkeyutl -verify -pubin -inkey "$work/release-key.pem" -rawin \
  -in "$work/SHA256SUMS" -sigfile "$work/SHA256SUMS.sig" >/dev/null 2>&1 ||
  fail "the release signature does not match the release signing key, so the release may have been tampered with; nothing was installed"

asset="$(awk -v suffix="-$arch-linux.tar.gz" '
  substr($2, 1, 13) == "umbriel-mcp-v" && substr($2, length($2) - length(suffix) + 1) == suffix { print $2; exit }
' "$work/SHA256SUMS")"
[ -n "$asset" ] || fail "the latest release has no $arch build"
tag="${asset#umbriel-mcp-}"
tag="${tag%-"$arch"-linux.tar.gz}"

echo "Downloading umbriel-mcp $tag for $arch..."
curl -fsSL "$releases/download/$tag/$asset" -o "$work/$asset" || fail "could not download $asset"

awk -v file="$asset" '$2 == file' "$work/SHA256SUMS" >"$work/expected"
(cd "$work" && sha256sum -c expected >/dev/null 2>&1) ||
  fail "$asset does not match its published checksum; nothing was installed"

tar -xzf "$work/$asset" -C "$work"
mkdir -p "$install_dir"
cp "$work/umbriel-mcp-$tag-$arch-linux/umbriel-mcp" "$install_dir/.umbriel-mcp.new"
chmod 755 "$install_dir/.umbriel-mcp.new"
mv -f "$install_dir/.umbriel-mcp.new" "$target"

echo "Installed $("$target" --version) to $target"
echo "Update it later with: umbriel-mcp update"

case ":$PATH:" in
  *":$install_dir:"*) ;;
  *)
    echo
    echo "$install_dir is not on your PATH yet. Log out and back in (many distributions add it"
    echo "at login once it exists), or add it to your shell's PATH."
    ;;
esac

found="$(command -v umbriel-mcp 2>/dev/null || true)"
if [ -n "$found" ] && [ "$found" != "$target" ]; then
  echo
  echo "Another umbriel-mcp at $found comes first on your PATH and would be used instead."
  echo "Remove it (for example: sudo rm $found) so this installation is the one that runs."
fi

echo
"$target" doctor || true
