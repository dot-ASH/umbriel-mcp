use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

pub fn expanded(path: &str, home: Option<&str>) -> Result<PathBuf> {
    let path = path.trim();
    let expanded = match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => Path::new(home).join(rest),
        (Some(_), None) => return Err(anyhow!("cannot expand ~ because HOME is not set")),
        (None, _) => PathBuf::from(path),
    };
    if !expanded.is_absolute() {
        return Err(anyhow!(
            "save_to must be an absolute path or start with ~/, not {path:?}"
        ));
    }
    let is_png = expanded
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"));
    if !is_png {
        return Err(anyhow!("save_to must name a .png file, not {path:?}"));
    }
    Ok(expanded)
}

fn refuse_overwriting_anything_but_a_png(path: &Path) -> Result<()> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(());
    };
    if !metadata.is_file() {
        return Err(anyhow!("{} exists and is not a file", path.display()));
    }
    let mut start = [0u8; 8];
    let is_png = std::fs::File::open(path)
        .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut start))
        .is_ok()
        && start == PNG_SIGNATURE;
    if is_png {
        Ok(())
    } else {
        Err(anyhow!(
            "{} already exists and is not a PNG image, so it was not overwritten",
            path.display()
        ))
    }
}

pub fn save_png(path: &str, png: &[u8]) -> Result<PathBuf> {
    let target = expanded(path, std::env::var("HOME").ok().as_deref())?;
    refuse_overwriting_anything_but_a_png(&target)?;
    if let Some(folder) = target.parent() {
        std::fs::create_dir_all(folder)
            .with_context(|| format!("cannot create {}", folder.display()))?;
    }
    let staged = target.with_extension(format!("png.umbriel-mcp-{}", std::process::id()));
    std::fs::write(&staged, png).with_context(|| format!("cannot write {}", staged.display()))?;
    std::fs::rename(&staged, &target)
        .with_context(|| format!("cannot write {}", target.display()))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::expanded;
    use std::path::PathBuf;

    #[test]
    fn a_home_relative_png_path_is_expanded() {
        assert_eq!(
            expanded("~/shots/a.PNG", Some("/home/u")).unwrap(),
            PathBuf::from("/home/u/shots/a.PNG")
        );
        assert_eq!(
            expanded("/tmp/a.png", None).unwrap(),
            PathBuf::from("/tmp/a.png")
        );
    }

    #[test]
    fn relative_paths_and_other_extensions_are_refused() {
        assert!(expanded("shots/a.png", Some("/home/u")).is_err());
        assert!(expanded("/home/u/.bashrc", Some("/home/u")).is_err());
        assert!(expanded("/home/u/a.png.txt", Some("/home/u")).is_err());
    }
}
