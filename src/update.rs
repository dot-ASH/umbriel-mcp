use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

const RELEASES: &str = "https://github.com/dot-ASH/umbriel-mcp/releases";
const RELEASES_OVERRIDE: &str = "UMBRIEL_MCP_RELEASES";
const ASSET_PREFIX: &str = "umbriel-mcp-";
const ASSET_SUFFIX: &str = "-linux.tar.gz";
const BINARY: &str = "umbriel-mcp";
const DOWNLOAD_TIMEOUT_SECONDS: &str = "120";
const CHECK_TIMEOUT_SECONDS: &str = "10";

#[derive(Debug, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: (u64, u64, u64),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Installation {
    SelfManaged,
    ManagedElsewhere(&'static str),
}

pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

pub fn asset_name(tag: &str, arch: &str) -> String {
    format!("{ASSET_PREFIX}{tag}-{arch}{ASSET_SUFFIX}")
}

pub fn latest_release(sums: &str, arch: &str) -> Option<Release> {
    let arch_suffix = format!("-{arch}{ASSET_SUFFIX}");
    sums.lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(|file| {
            file.strip_prefix(ASSET_PREFIX)?
                .strip_suffix(arch_suffix.as_str())
        })
        .filter_map(|tag| {
            parse_version(tag).map(|version| Release {
                tag: tag.to_owned(),
                version,
            })
        })
        .max_by_key(|release| release.version)
}

pub fn digest_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let digest = fields.next()?;
        (fields.next()? == file && digest.len() == 64).then(|| digest.to_lowercase())
    })
}

pub fn classify(executable: &Path) -> Installation {
    let path = executable.to_string_lossy();
    if path.starts_with("/usr/bin/") || path.starts_with("/bin/") || path.starts_with("/usr/sbin/")
    {
        return Installation::ManagedElsewhere(
            "it was installed by your system package manager, so update it there (for the AUR \
             package: `yay -Syu umbriel-mcp-bin`; for a makepkg build: rebuild the recipe)",
        );
    }
    if path.contains("/.cargo/bin/") {
        return Installation::ManagedElsewhere(
            "it was installed with cargo, so update it with `cargo install umbriel-mcp --locked`",
        );
    }
    if path.starts_with("/nix/") {
        return Installation::ManagedElsewhere("it was installed with Nix, so update it there");
    }
    Installation::SelfManaged
}

fn releases_base() -> String {
    std::env::var(RELEASES_OVERRIDE)
        .ok()
        .filter(|base| !base.is_empty())
        .unwrap_or_else(|| RELEASES.to_owned())
        .trim_end_matches('/')
        .to_owned()
}

fn curl(url: &str, timeout_seconds: &str, destination: Option<&Path>) -> Result<Vec<u8>> {
    let mut command = Command::new("curl");
    command.args(["-fsSL", "--max-time", timeout_seconds]);
    if let Some(destination) = destination {
        command.arg("-o").arg(destination);
    }
    let output = command.arg(url).output().map_err(|cause| {
        anyhow!("updating needs curl ({cause}); install it with your package manager")
    })?;
    if !output.status.success() {
        return Err(anyhow!(
            "could not download {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn latest(timeout_seconds: &str) -> Result<(Release, String)> {
    let url = format!("{}/latest/download/SHA256SUMS", releases_base());
    let sums = String::from_utf8_lossy(&curl(&url, timeout_seconds, None)?).into_owned();
    let release = latest_release(&sums, std::env::consts::ARCH).ok_or_else(|| {
        anyhow!(
            "the latest release has no build for {} Linux",
            std::env::consts::ARCH
        )
    })?;
    Ok((release, sums))
}

fn current_version() -> (u64, u64, u64) {
    parse_version(env!("CARGO_PKG_VERSION")).expect("the crate version is x.y.z")
}

fn dotted((major, minor, patch): (u64, u64, u64)) -> String {
    format!("{major}.{minor}.{patch}")
}

pub fn notice() -> String {
    match latest(CHECK_TIMEOUT_SECONDS) {
        Ok((release, _)) if release.version > current_version() => format!(
            "Update available: {} (you have {}). Run `umbriel-mcp update`.",
            dotted(release.version),
            env!("CARGO_PKG_VERSION")
        ),
        Ok(_) => format!(
            "umbriel-mcp {} is the latest version.",
            env!("CARGO_PKG_VERSION")
        ),
        Err(error) => format!("Could not check for updates: {error:#}"),
    }
}

struct WorkDirectory(PathBuf);

impl WorkDirectory {
    fn create() -> Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path =
            std::env::temp_dir().join(format!("umbriel-mcp-update-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&path).with_context(|| format!("cannot create {}", path.display()))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        Ok(Self(path))
    }
}

impl Drop for WorkDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sha256_hex(file: &Path) -> Result<String> {
    let bytes = std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn staging_path(executable: &Path) -> PathBuf {
    executable.with_file_name(format!(".{BINARY}.update-{}", std::process::id()))
}

fn refuse_unwritable(executable: &Path) -> Result<()> {
    let probe = staging_path(executable);
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Err(anyhow!(
            "cannot replace {} because its folder is not writable by you; run \
             `sudo umbriel-mcp update`, or reinstall into ~/.local/bin with the install script",
            executable.display()
        )),
        Err(error) => Err(anyhow!(
            "cannot write next to {}: {error}",
            executable.display()
        )),
    }
}

fn download_verified(release: &Release, sums: &str, work: &Path) -> Result<PathBuf> {
    let asset = asset_name(&release.tag, std::env::consts::ARCH);
    let expected = digest_for(sums, &asset)
        .ok_or_else(|| anyhow!("SHA256SUMS has no checksum for {asset}"))?;
    let tarball = work.join(&asset);
    let url = format!("{}/download/{}/{asset}", releases_base(), release.tag);
    curl(&url, DOWNLOAD_TIMEOUT_SECONDS, Some(&tarball))?;
    let actual = sha256_hex(&tarball)?;
    if actual != expected {
        return Err(anyhow!(
            "the download of {asset} does not match its published checksum (expected {expected}, \
             got {actual}); nothing was changed"
        ));
    }
    let extracted = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(work)
        .output()
        .map_err(|cause| anyhow!("updating needs tar ({cause})"))?;
    if !extracted.status.success() {
        return Err(anyhow!(
            "could not unpack {asset}: {}",
            String::from_utf8_lossy(&extracted.stderr).trim()
        ));
    }
    let binary = work.join(asset.trim_end_matches(".tar.gz")).join(BINARY);
    if !binary.is_file() {
        return Err(anyhow!("{asset} does not contain {BINARY}"));
    }
    Ok(binary)
}

fn confirm_it_runs(binary: &Path, release: &Release) -> Result<()> {
    std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o755))?;
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|cause| {
            anyhow!("the downloaded {BINARY} does not run ({cause}); nothing was changed")
        })?;
    let reported = String::from_utf8_lossy(&output.stdout);
    let expected = format!("{BINARY} {}", dotted(release.version));
    if !output.status.success() || reported.trim() != expected {
        return Err(anyhow!(
            "the downloaded {BINARY} reports {:?} instead of {expected:?}; nothing was changed",
            reported.trim()
        ));
    }
    Ok(())
}

fn replace(executable: &Path, binary: &Path) -> Result<()> {
    let staged = staging_path(executable);
    let result = (|| -> Result<()> {
        std::fs::copy(binary, &staged)
            .with_context(|| format!("cannot write {}", staged.display()))?;
        let permissions = std::fs::metadata(executable)
            .map(|metadata| metadata.permissions())
            .unwrap_or_else(|_| std::fs::Permissions::from_mode(0o755));
        std::fs::set_permissions(&staged, permissions)?;
        std::fs::rename(&staged, executable)
            .with_context(|| format!("cannot replace {}", executable.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

fn update(check_only: bool) -> Result<String> {
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .context("cannot find the running umbriel-mcp executable")?;
    let (release, sums) = latest(DOWNLOAD_TIMEOUT_SECONDS)?;
    let current = env!("CARGO_PKG_VERSION");
    if release.version <= current_version() {
        return Ok(format!("umbriel-mcp {current} is up to date."));
    }
    let newer = dotted(release.version);
    let installation = classify(&executable);
    if check_only {
        let how = match installation {
            Installation::SelfManaged => "run `umbriel-mcp update`".to_owned(),
            Installation::ManagedElsewhere(advice) => advice.to_owned(),
        };
        return Ok(format!(
            "Update available: {newer} (you have {current}); {how}."
        ));
    }
    if let Installation::ManagedElsewhere(advice) = installation {
        return Err(anyhow!(
            "umbriel-mcp {newer} is available, but {} was not updated: {advice}.",
            executable.display()
        ));
    }
    refuse_unwritable(&executable)?;
    let work = WorkDirectory::create()?;
    let binary = download_verified(&release, &sums, &work.0)?;
    confirm_it_runs(&binary, &release)?;
    replace(&executable, &binary)?;
    Ok(format!(
        "Updated {} from {current} to {newer}. Restart your harness, or start a new session, so \
         it uses the new version.",
        executable.display()
    ))
}

pub fn run(check_only: bool) -> ExitCode {
    match update(check_only) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Installation, Release, asset_name, classify, digest_for, latest_release, parse_version,
    };
    use std::path::Path;

    const SUMS: &str = "\
1111111111111111111111111111111111111111111111111111111111111111  umbriel-mcp-v0.3.0-aarch64-linux.tar.gz
2222222222222222222222222222222222222222222222222222222222222222  umbriel-mcp-v0.3.0-x86_64-linux.tar.gz
";

    #[test]
    fn versions_parse_with_or_without_a_leading_v() {
        assert_eq!(parse_version("0.2.10"), Some((0, 2, 10)));
        assert_eq!(parse_version("v1.0.0"), Some((1, 0, 0)));
        assert_eq!(parse_version("1.0"), None);
        assert_eq!(parse_version("x.y.z"), None);
        assert!(
            parse_version("0.2.10") > parse_version("0.2.9"),
            "numeric, not text, ordering"
        );
    }

    #[test]
    fn the_latest_release_is_read_from_its_checksum_list() {
        assert_eq!(
            latest_release(SUMS, "x86_64"),
            Some(Release {
                tag: "v0.3.0".into(),
                version: (0, 3, 0)
            })
        );
        assert_eq!(latest_release(SUMS, "riscv64"), None);
        assert_eq!(latest_release("garbage\n", "x86_64"), None);
    }

    #[test]
    fn asset_names_and_digests_match_the_release_layout() {
        assert_eq!(
            asset_name("v0.3.0", "x86_64"),
            "umbriel-mcp-v0.3.0-x86_64-linux.tar.gz"
        );
        assert_eq!(
            digest_for(SUMS, "umbriel-mcp-v0.3.0-x86_64-linux.tar.gz").as_deref(),
            Some("2222222222222222222222222222222222222222222222222222222222222222")
        );
        assert_eq!(
            digest_for(SUMS, "umbriel-mcp-v0.3.0-x86_64-linux.tar"),
            None
        );
    }

    #[test]
    fn binaries_owned_by_a_package_manager_are_not_self_updated() {
        assert_eq!(
            classify(Path::new("/home/u/.local/bin/umbriel-mcp")),
            Installation::SelfManaged
        );
        assert_eq!(
            classify(Path::new("/usr/local/bin/umbriel-mcp")),
            Installation::SelfManaged
        );
        assert_eq!(
            classify(Path::new("/opt/tools/umbriel-mcp")),
            Installation::SelfManaged
        );
        for managed in [
            "/usr/bin/umbriel-mcp",
            "/bin/umbriel-mcp",
            "/home/u/.cargo/bin/umbriel-mcp",
            "/nix/store/abc-umbriel-mcp/bin/umbriel-mcp",
        ] {
            assert!(
                matches!(
                    classify(Path::new(managed)),
                    Installation::ManagedElsewhere(_)
                ),
                "{managed}"
            );
        }
        let Installation::ManagedElsewhere(advice) =
            classify(Path::new("/home/u/.cargo/bin/umbriel-mcp"))
        else {
            unreachable!()
        };
        assert!(advice.contains("cargo install umbriel-mcp"), "{advice}");
    }
}
