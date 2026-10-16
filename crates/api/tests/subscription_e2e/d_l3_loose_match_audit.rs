//! D-L3: loose title matching (`names_match` / `torrent_matches_snapshot`)
//! is display-only. Audit that no destructive source file calls it.

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn loose_matchers_are_only_used_by_display_code() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    // Display-only call sites, plus the definitions themselves.
    let allowed = [
        "api/src/http/subscription_depth.rs",
        "downloader/src/lib.rs",
        "downloader/src/identity.rs",
    ];
    let mut files = Vec::new();
    for krate in ["api", "subscribe", "downloader", "library", "store"] {
        walk(&crates.join(krate).join("src"), &mut files);
    }
    let mut offenders = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&crates)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if allowed.contains(&rel.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        if text.contains("names_match(") || text.contains("torrent_matches_snapshot(") {
            offenders.push(rel);
        }
    }
    assert!(offenders.is_empty(), "loose matcher used in {offenders:?}");
}
