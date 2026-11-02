#!/bin/sh
set -eu

tag="${1:?usage: fill-aur-checksums.sh vX.Y.Z}"
recipe_dir="${RECIPE_DIR:-$(cd "$(dirname "$0")/../packaging/aur-bin" && pwd)}"
recipe="$recipe_dir/PKGBUILD"

recipe_version="$(sed -n 's/^pkgver=//p' "$recipe")"
[ "v$recipe_version" = "$tag" ] || {
  echo "PKGBUILD has pkgver=$recipe_version, which does not match $tag" >&2
  exit 1
}

sums="$(curl -fsSL "https://github.com/dot-ASH/umbriel-mcp/releases/download/$tag/SHA256SUMS")" || {
  echo "no SHA256SUMS for $tag yet; wait for the release workflow to finish" >&2
  exit 1
}

for arch in x86_64 aarch64; do
  digest="$(printf '%s\n' "$sums" | awk -v file="umbriel-mcp-$tag-$arch-linux.tar.gz" '$2 == file { print $1 }')"
  [ -n "$digest" ] || { echo "SHA256SUMS for $tag has no $arch tarball" >&2; exit 1; }
  sed -i "s/^sha256sums_$arch=.*/sha256sums_$arch=('$digest')/" "$recipe"
  echo "$arch: $digest"
done

(cd "$recipe_dir" && makepkg --printsrcinfo > .SRCINFO)
echo "updated $recipe and .SRCINFO"
