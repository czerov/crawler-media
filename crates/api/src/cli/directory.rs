use super::parse::Command;

pub fn parse_directory(args: &[&str]) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Directory);
    }
    let mut add = false;
    let mut remove = false;
    let mut kind = None;
    let mut path = None;
    let mut id = None;
    let mut watch_inplace = None;
    let mut watch_intake = None;
    let mut transfer_mode = None;
    let mut scrape = None;
    let mut movie_naming = None;
    let mut tv_naming = None;
    let mut rest = args;
    while let Some((flag, tail)) = rest.split_first() {
        match *flag {
            "--add-root" => {
                add = true;
                rest = tail;
            }
            "--remove-root" => {
                remove = true;
                rest = tail;
            }
            "--kind" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--kind needs a value".to_string())?;
                kind = Some((*value).to_string());
                rest = next;
            }
            "--path" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--path needs a value".to_string())?;
                path = Some((*value).to_string());
                rest = next;
            }
            "--id" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--id needs a value".to_string())?;
                id = Some((*value).to_string());
                rest = next;
            }
            "--watch-inplace" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--watch-inplace needs a value".to_string())?;
                watch_inplace = Some((*value).to_string());
                rest = next;
            }
            "--watch-intake" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--watch-intake needs a value".to_string())?;
                watch_intake = Some((*value).to_string());
                rest = next;
            }
            "--transfer-mode" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--transfer-mode needs a value".to_string())?;
                transfer_mode = Some((*value).to_string());
                rest = next;
            }
            "--scrape" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--scrape needs on or off".to_string())?;
                scrape = Some(parse_on_off(value)?);
                rest = next;
            }
            "--movie-naming" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--movie-naming needs a value".to_string())?;
                if value.trim().is_empty() {
                    return Err("movie-naming must not be empty".into());
                }
                movie_naming = Some((*value).to_string());
                rest = next;
            }
            "--tv-naming" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--tv-naming needs a value".to_string())?;
                if value.trim().is_empty() {
                    return Err("tv-naming must not be empty".into());
                }
                tv_naming = Some((*value).to_string());
                rest = next;
            }
            other => return Err(format!("unknown directory flag {other}")),
        }
    }
    if add && remove {
        return Err("use --add-root or --remove-root, not both".into());
    }
    if remove {
        return Ok(Command::DirectoryRemoveRoot {
            id: id.ok_or_else(|| "remove-root needs --id".to_string())?,
        });
    }
    if let Some(path) = watch_inplace {
        if add
            || watch_intake.is_some()
            || transfer_mode.is_some()
            || scrape.is_some()
            || movie_naming.is_some()
            || tv_naming.is_some()
        {
            return Err("use one directory mutation at a time".into());
        }
        return Ok(Command::DirectoryWatchInplace { path });
    }
    if let Some(path) = watch_intake {
        if add
            || transfer_mode.is_some()
            || scrape.is_some()
            || movie_naming.is_some()
            || tv_naming.is_some()
        {
            return Err("use one directory mutation at a time".into());
        }
        return Ok(Command::DirectoryWatchIntake { path });
    }
    if let Some(mode) = transfer_mode {
        if add || scrape.is_some() || movie_naming.is_some() || tv_naming.is_some() {
            return Err("use one directory mutation at a time".into());
        }
        if !matches!(mode.as_str(), "hardlink" | "copy" | "move") {
            return Err("transfer-mode must be hardlink, copy, or move".into());
        }
        return Ok(Command::DirectoryTransferMode { mode });
    }
    if let Some(enabled) = scrape {
        if add || movie_naming.is_some() || tv_naming.is_some() {
            return Err("use one directory mutation at a time".into());
        }
        return Ok(Command::DirectoryScrape { enabled });
    }
    if let Some(pattern) = movie_naming {
        if add || tv_naming.is_some() {
            return Err("use one directory mutation at a time".into());
        }
        return Ok(Command::DirectoryMovieNaming { pattern });
    }
    if let Some(pattern) = tv_naming {
        if add {
            return Err("use one directory mutation at a time".into());
        }
        return Ok(Command::DirectoryTvNaming { pattern });
    }
    if !add {
        return Err(
            "usage: directory | directory --add-root --kind movie|tv --path <dir> | directory --remove-root --id <id> | directory --watch-inplace <path> | directory --watch-intake <path> | directory --transfer-mode hardlink|copy|move | directory --scrape on|off | directory --movie-naming|--tv-naming <pattern>"
                .into(),
        );
    }
    Ok(Command::DirectoryAddRoot {
        kind: kind.ok_or_else(|| "add-root needs --kind".to_string())?,
        path: path.ok_or_else(|| "add-root needs --path".to_string())?,
    })
}

/// Shares the workspace boolean vocabulary (`1`/`true`/`yes`/`on` and
/// `0`/`false`/`no`/`off`) with env flags and settings KV.
fn parse_on_off(value: &str) -> Result<bool, String> {
    crate::config::parse_bool(value).ok_or_else(|| "scrape must be on or off".into())
}
