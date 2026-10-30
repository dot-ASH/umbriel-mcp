use serde_json::Value;

const README: &str = include_str!("../README.md");
const SERVER_MANIFEST: &str = include_str!("../server.json");
const AUR_RECIPE: &str = include_str!("../packaging/aur-bin/PKGBUILD");

fn manifest() -> Value {
    serde_json::from_str(SERVER_MANIFEST).expect("server.json is JSON")
}

#[test]
fn the_readme_names_the_same_registry_server_as_the_manifest() {
    let declared = README
        .lines()
        .find_map(|line| line.trim().strip_prefix("mcp-name: "))
        .expect("README has an mcp-name line");

    assert_eq!(declared, manifest()["name"]);
}

#[test]
fn the_registry_namespace_matches_the_github_owner_exactly() {
    let name = manifest()["name"].as_str().unwrap().to_owned();
    let repository = manifest()["repository"]["url"].as_str().unwrap().to_owned();
    let owner = repository
        .trim_start_matches("https://github.com/")
        .split('/')
        .next()
        .unwrap()
        .to_owned();

    assert!(
        name.starts_with(&format!("io.github.{owner}/")),
        "{name} must use the GitHub owner {owner} with the same capitalisation"
    );
}

#[test]
fn every_published_version_matches_the_crate_version() {
    let version = env!("CARGO_PKG_VERSION");
    let manifest = manifest();

    assert_eq!(manifest["version"], version);
    assert_eq!(manifest["packages"][0]["version"], version);
    assert!(AUR_RECIPE.contains(&format!("pkgver={version}\n")));
    assert!(README.contains(&format!("tag=v{version}\n")));
}
