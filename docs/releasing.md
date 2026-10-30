# Releasing

Publishing to crates.io, the MCP Registry, and the AUR is manual. The release workflow only
builds binaries, writes `SHA256SUMS`, and creates the GitHub Release; it holds no registry
credentials.

1. Set the same version in `Cargo.toml` and in `server.json` (the top-level `version` and the
   package `version`), update `packaging/aur-bin/PKGBUILD` `pkgver`, and commit.
2. Check everything locally: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`,
   `cargo test`, then `cargo test -- --ignored --test-threads=1` in an idle Umbriel session.
3. Tag and push: `git tag vX.Y.Z && git push origin vX.Y.Z`. The tag must equal `vCargo.toml-version`
   or the workflow fails. Wait for the GitHub Release to appear.
4. Publish the crate: `cargo publish`.
5. Publish the registry entry (the crate must already be on crates.io):
   `mcp-publisher login github && mcp-publisher publish`.
6. Update the AUR package (needs an AUR account with your SSH key, and `pacman-contrib` for
   `updpkgsums`). In a scratch directory:

   ```sh
   git clone ssh://aur@aur.archlinux.org/umbriel-mcp-bin.git
   cd umbriel-mcp-bin
   cp ~/Projects/umbriel-mcp/packaging/aur-bin/PKGBUILD .
   updpkgsums
   makepkg -f
   makepkg --printsrcinfo > .SRCINFO
   git add PKGBUILD .SRCINFO
   git commit -m "Update to X.Y.Z"
   git push
   ```

   Commit the updated checksums back to `packaging/aur-bin/PKGBUILD` too.

The registry name `io.github.dot-ASH/umbriel-mcp` is case-sensitive and must match the GitHub
owner exactly. The registry checks it against the `mcp-name:` line of the README published on
crates.io, so a wrong name can only be fixed by publishing a new crate version.
`tests/release_metadata.rs` guards both.

`packaging/PKGBUILD` is a build-from-source recipe kept as a starting point for contributors; it
is not published.
