# Releasing

Publishing to crates.io, the MCP Registry, and the AUR is manual. The release workflow only
builds binaries, writes `SHA256SUMS`, and creates the GitHub Release; it holds no registry
credentials. Every command below works in bash, zsh, and fish.

## 1. Bump the version

Set the new version `X.Y.Z` in all of these:

- `Cargo.toml` (`version`)
- `server.json`, twice: the top-level `version` and the package `version`
- `packaging/aur-bin/PKGBUILD` and `packaging/PKGBUILD` (`pkgver`)
- `README.md`, the `tag=vX.Y.Z` line in the release-binary install steps

In `packaging/aur-bin/PKGBUILD`, set both `sha256sums_x86_64` and `sha256sums_aarch64` back to
`('SKIP')` and delete `packaging/aur-bin/.SRCINFO`. The old checksums belong to the old tarballs;
step 5 fills in the new ones.

`cargo test` fails if any of these versions disagree (`tests/release_metadata.rs`).

## 2. Check locally

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo publish --dry-run --allow-dirty
mcp-publisher validate server.json
```

Then, in an idle, unlocked Umbriel session with nothing else using the keyboard for a minute:

```sh
cargo test -- --ignored --test-threads=1
```

## 3. Commit, tag, and wait for the release

```sh
git add -A
git commit -m "release X.Y.Z"
git push
git tag vX.Y.Z
git push origin vX.Y.Z
```

The tag must equal `v` plus the `Cargo.toml` version, or the workflow fails. Wait until the
GitHub Release shows three files: both `umbriel-mcp-vX.Y.Z-<arch>-linux.tar.gz` tarballs and
`SHA256SUMS`.

## 4. Publish the crate and the registry entry

```sh
cargo publish
mcp-publisher login github
mcp-publisher publish
```

- The crate must be on crates.io before `mcp-publisher publish`; the registry verifies it.
- The registry login token expires within minutes. Run `login` immediately before `publish`;
  a `401 ... token is expired` error just means log in again.
- The registry name `io.github.dot-ASH/umbriel-mcp` is case-sensitive and must match the
  GitHub owner exactly. The registry checks it against the `mcp-name:` line of the README
  published on crates.io, so a wrong name can only be fixed by publishing a new crate version.
  `tests/release_metadata.rs` guards this.

## 5. Fill in the Arch checksums

```sh
scripts/fill-aur-checksums.sh vX.Y.Z
```

This reads the release's `SHA256SUMS`, writes both checksums into
`packaging/aur-bin/PKGBUILD`, and regenerates `.SRCINFO` (it needs `makepkg`, so run it on Arch).
It refuses if the release is not out yet or `pkgver` does not match the tag. Commit the result:

```sh
git add packaging/aur-bin
git commit -m "packaging: add X.Y.Z checksums"
git push
```

Arch users install from this recipe with `makepkg -si` until the AUR package exists.

## 6. Update the AUR package

Needs an AUR account with your SSH public key (`~/.ssh/id_ed25519.pub`) added under
My Account. Check access with `ssh aur@aur.archlinux.org help`. New AUR registrations were
paused in late October 2026; until that is lifted and the package is published, skip this
step.

```sh
cd /tmp
git clone ssh://aur@aur.archlinux.org/umbriel-mcp-bin.git
cd umbriel-mcp-bin
cp ~/Projects/umbriel-mcp/packaging/aur-bin/PKGBUILD ~/Projects/umbriel-mcp/packaging/aur-bin/.SRCINFO .
git add PKGBUILD .SRCINFO
git commit -m "Update to X.Y.Z"
git push
```

The first push creates the package; cloning a package that does not exist yet warns about an
empty repository, which is expected. After the first AUR release, switch the README's Arch
section to `yay -S umbriel-mcp-bin`.

`packaging/PKGBUILD` is a build-from-source recipe kept as a starting point for contributors;
it is not published.
