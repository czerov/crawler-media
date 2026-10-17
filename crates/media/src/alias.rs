use domain::Media;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Tmdb,
    Douban,
    Tvdb,
    Bangumi,
    Anilist,
}

pub fn attach(mut media: Media, source: Source, id: &str) -> Media {
    match source {
        Source::Tmdb if media.tmdb_id.is_none() => media.tmdb_id = Some(id.into()),
        Source::Douban if media.douban_id.is_none() => media.douban_id = Some(id.into()),
        Source::Tvdb if media.tvdb_id.is_none() => media.tvdb_id = Some(id.into()),
        Source::Bangumi if media.bangumi_id.is_none() => media.bangumi_id = Some(id.into()),
        Source::Anilist if media.anilist_id.is_none() => media.anilist_id = Some(id.into()),
        _ => {}
    }
    media
}

pub fn merge(into: Media, from: Media) -> Media {
    let mut merged = into;
    if merged.year.is_none() {
        merged.year = from.year;
    }
    if merged.original_title.is_none() {
        merged.original_title = from.original_title;
    }
    for alias in from.aliases {
        if !merged.aliases.iter().any(|existing| existing == &alias) {
            merged.aliases.push(alias);
        }
    }
    if merged.tmdb_id.is_none() {
        merged.tmdb_id = from.tmdb_id;
    }
    if merged.douban_id.is_none() {
        merged.douban_id = from.douban_id;
    }
    if merged.tvdb_id.is_none() {
        merged.tvdb_id = from.tvdb_id;
    }
    if merged.bangumi_id.is_none() {
        merged.bangumi_id = from.bangumi_id;
    }
    if merged.anilist_id.is_none() {
        merged.anilist_id = from.anilist_id;
    }
    merged
}
