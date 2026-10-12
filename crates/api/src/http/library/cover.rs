use cover_generator::{BackgroundOption, CoverOptions, CoverStyle, generate_cover};
use image::DynamicImage;
use std::collections::HashSet;
use std::io::Cursor;
use std::path::Path;

use crate::store::Library;

/// Old releases copied a single `fanart.jpg` into the generated-cover path.
/// Detect that exact byte-for-byte shape so upgrading preserves manual covers
/// while replacing stale automatic covers when poster art is available.
pub(crate) fn is_legacy_fanart_copy(rows: &[domain::LedgerRow], cover: &Path) -> bool {
    let Ok(cover_bytes) = std::fs::read(cover) else {
        return false;
    };
    rows.iter()
        .filter_map(super::backdrop_path)
        .any(|fanart| std::fs::read(fanart).is_ok_and(|bytes| bytes == cover_bytes))
}

/// 自动封面默认选项（标题取库名、多海报拼贴）。
pub(crate) fn default_cover_options(library: &Library) -> CoverOptions {
    CoverOptions {
        title_zh: library.name.clone(),
        title_en: None,
        width: 1920,
        height: 1080,
        style: CoverStyle::MultiPosterPile,
        background: BackgroundOption::default(),
    }
}

/// 锁外渲染：读取本地海报并合成 JPEG 字节，**不写盘**（发布由 cover service 负责）。
/// `None` means this Library has no readable local poster yet.
pub(crate) fn render_cover_jpeg(
    library: &Library,
    rows: &[domain::LedgerRow],
    options: &CoverOptions,
) -> Result<Option<Vec<u8>>, String> {
    let posters = collect_posters(rows);
    if posters.is_empty() {
        return Ok(None);
    }
    let jpeg = render_library_cover(&posters, options)?;
    tracing::info!(
        library_id = %library.id,
        poster_count = posters.len(),
        "rendered library cover bytes from distinct media posters"
    );
    Ok(Some(jpeg))
}

fn collect_posters(rows: &[domain::LedgerRow]) -> Vec<DynamicImage> {
    let mut rows: Vec<&domain::LedgerRow> = rows.iter().collect();
    rows.sort_by(|left, right| left.path.cmp(&right.path));
    let mut seen_media_ids = HashSet::new();
    let mut posters: Vec<DynamicImage> = Vec::new();
    for row in rows {
        if seen_media_ids.contains(&row.media_id) {
            continue;
        }
        let Some(path) = super::poster_path(&row) else {
            continue;
        };
        match std::fs::read(&path).and_then(|bytes| {
            image::load_from_memory(&bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        }) {
            Ok(poster) => {
                seen_media_ids.insert(row.media_id);
                posters.push(poster);
                if posters.len() == 6 {
                    break;
                }
            }
            Err(error) => {
                tracing::debug!(%error, poster_path = %path.display(), "skipping unreadable library poster");
            }
        }
    }
    posters
}

fn render_library_cover(
    posters: &[DynamicImage],
    options: &CoverOptions,
) -> Result<Vec<u8>, String> {
    let font = include_bytes!("../../../../cover-generator/assets/fonts/chaohei.ttf");
    let generated = generate_cover(posters, options, font, None)
        .map_err(|error| format!("cover.generate: {error}"))?;
    let mut jpeg = Vec::new();
    generated
        .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
        .map_err(|error| format!("cover.encode: {error}"))?;
    Ok(jpeg)
}
