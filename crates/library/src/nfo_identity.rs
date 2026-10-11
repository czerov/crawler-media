use domain::MediaKind;

pub(crate) const MAX_NFO_BYTES: usize = 1024 * 1024;

/// Work-level identity only. Episode aliases never identify the TV Media.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NfoIdentity {
    pub kind: MediaKind,
    pub title: Option<String>,
    pub year: Option<u16>,
    pub original_title: Option<String>,
    pub tmdb_id: Option<String>,
    pub douban_id: Option<String>,
    pub tvdb_id: Option<String>,
    pub bangumi_id: Option<String>,
    pub anilist_id: Option<String>,
}

pub fn parse_nfo_identity(body: &str) -> Option<NfoIdentity> {
    let doc = document(body)?;
    let root = doc.root_element();
    let kind = match root.tag_name().name() {
        "movie" => MediaKind::Movie,
        "tvshow" => MediaKind::Tv,
        _ => return None,
    };
    Some(NfoIdentity {
        kind,
        title: text(root, "title"),
        year: text(root, "year")
            .and_then(|year| year.parse().ok())
            .filter(|year| *year > 0),
        original_title: text(root, "originaltitle"),
        tmdb_id: alias(root, "tmdb", "tmdbid")?,
        douban_id: alias(root, "douban", "doubanid")?,
        tvdb_id: alias(root, "tvdb", "tvdbid")?,
        bangumi_id: alias(root, "bangumi", "bangumiid")?,
        anilist_id: alias(root, "anilist", "anilistid")?,
    })
}

pub(crate) fn document(body: &str) -> Option<roxmltree::Document<'_>> {
    if body.len() > MAX_NFO_BYTES {
        return None;
    }
    roxmltree::Document::parse_with_options(
        body,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 20_000,
            ..Default::default()
        },
    )
    .ok()
}

fn text(root: roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    root.children()
        .find(|node| node.has_tag_name(name))?
        .text()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

// Any malformed or contradictory recognized alias invalidates the work identity.
fn alias(root: roxmltree::Node<'_, '_>, provider: &str, tag: &str) -> Option<Option<String>> {
    let mut found = None;
    for node in root.children().filter(|node| {
        node.has_tag_name(tag)
            || (node.has_tag_name("uniqueid")
                && node
                    .attribute("type")
                    .is_some_and(|kind| kind.eq_ignore_ascii_case(provider)))
    }) {
        let raw = node.text()?.trim();
        if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let number = raw.parse::<u64>().ok().filter(|number| *number > 0)?;
        let value = number.to_string();
        if found.as_ref().is_some_and(|previous| previous != &value) {
            return None;
        }
        found = Some(value);
    }
    Some(found)
}
