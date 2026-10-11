use std::io::Read;
use std::path::{Path, PathBuf};

use crate::nfo_identity::{MAX_NFO_BYTES, document};
use crate::{LibraryError, NfoIdentity, parse_nfo_identity};
use domain::{MediaKind, Release};

/// Search only the movie's own sidecars or TV ancestors inside the Library root.
pub fn read_watch_identity(
    path: &Path,
    root: &Path,
    kind: MediaKind,
) -> Result<Option<NfoIdentity>, LibraryError> {
    let Some((path, root)) = bounded_path(path, root)? else {
        return Ok(None);
    };
    let Some(parent) = path.parent() else {
        return Ok(None);
    };
    if kind == MediaKind::Movie {
        for candidate in [path.with_extension("nfo"), parent.join("movie.nfo")] {
            if let Some(identity) = read_identity(&candidate, &root, kind)? {
                return Ok(Some(identity));
            }
        }
    } else if kind == MediaKind::Tv {
        let mut current = Some(parent);
        while let Some(dir) = current {
            if let Some(identity) = read_identity(&dir.join("tvshow.nfo"), &root, kind)? {
                return Ok(Some(identity));
            }
            if dir == root {
                break;
            }
            current = dir.parent();
        }
    }
    Ok(None)
}

fn bounded_path(path: &Path, root: &Path) -> Result<Option<(PathBuf, PathBuf)>, LibraryError> {
    let root = root.canonicalize()?;
    let path = path.canonicalize()?;
    Ok(path.starts_with(&root).then_some((path, root)))
}

fn read_identity(
    path: &Path,
    root: &Path,
    kind: MediaKind,
) -> Result<Option<NfoIdentity>, LibraryError> {
    let Some(body) = read_bounded(path, root)? else {
        return Ok(None);
    };
    let identity = parse_nfo_identity(&body).filter(|identity| identity.kind == kind);
    if identity.is_none() {
        tracing::warn!(path = %path.display(), "Ignoring invalid or mismatched NFO work identity");
    }
    Ok(identity)
}

fn read_bounded(path: &Path, root: &Path) -> Result<Option<String>, LibraryError> {
    let resolved = match path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !resolved.starts_with(root) {
        return Ok(None);
    }
    let file = std::fs::File::open(resolved)?;
    if !file.metadata()?.is_file() {
        return Ok(None);
    }
    let mut body = String::new();
    file.take((MAX_NFO_BYTES + 1) as u64)
        .read_to_string(&mut body)?;
    if body.len() > MAX_NFO_BYTES {
        tracing::warn!(path = %path.display(), "Ignoring oversized NFO");
        return Ok(None);
    }
    Ok(Some(body))
}

/// Episode metadata fills missing coordinates only; contradictory filename fields win.
pub(crate) fn enrich_episode(
    parsed: &mut Release,
    path: &Path,
    root: &Path,
) -> Result<(), LibraryError> {
    let Some((path, root)) = bounded_path(path, root)? else {
        return Ok(());
    };
    let Some(body) = read_bounded(&path.with_extension("nfo"), &root)? else {
        return Ok(());
    };
    let Some(doc) = document(&body) else {
        return Ok(());
    };
    let node = doc.root_element();
    if !node.has_tag_name("episodedetails") {
        return Ok(());
    }
    let number = |name| {
        node.children()
            .find(|child| child.has_tag_name(name))
            .and_then(|child| child.text())
            .and_then(|text| text.trim().parse::<u32>().ok())
    };
    let season = number("season");
    let episode = number("episode");
    if parsed.season.zip(season).is_some_and(|(a, b)| a != b)
        || parsed.episode.zip(episode).is_some_and(|(a, b)| a != b)
    {
        tracing::warn!(path = %path.display(), "Episode NFO conflicts with filename coordinates; keeping filename");
        return Ok(());
    }
    parsed.season = parsed.season.or(season);
    parsed.episode = parsed.episode.or(episode);
    Ok(())
}
