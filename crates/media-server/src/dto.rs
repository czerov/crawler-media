mod chapters;
mod media_streams;
mod people;

pub use chapters::render_chapters_json;
pub use media_streams::{media_streams_json, media_streams_with_item};
pub use people::{media_person_json, person_item_json};

use playback::Item;
use serde_json::{Value, json};

use crate::provider::{MediaItemSnapshot, ServerLibrary, ServerUser};

pub fn library_view_json(lib: &ServerLibrary, server_id: &str) -> Value {
    let collection_type = match lib.kind {
        domain::MediaKind::Movie => "movies",
        domain::MediaKind::Tv => "tvshows",
        domain::MediaKind::Video => "homevideos",
    };
    let mut val = json!({
        "Id": lib.id,
        "Name": lib.name,
        "CollectionType": collection_type,
        "Type": "CollectionFolder",
        "IsFolder": true,
        "ServerId": server_id,
    });
    // Jellyfin clients cache images by ImageTags. Use a content-derived tag so
    // replacing a generated or uploaded cover invalidates VidHub's cached image.
    val["ImageTags"] = json!({ "Primary": lib.cover_tag });
    val
}

pub fn user_profile_json(user: &ServerUser, server_id: &str) -> Value {
    json!({
        "Id": user.id.to_string(),
        "Name": user.login,
        "ServerId": server_id,
        "HasPassword": true,
        "Policy": {
            "IsAdministrator": user.is_admin,
            "EnableMediaPlayback": true,
            "EnableContentDownloading": true,
        }
    })
}

pub fn item_dto_json(snapshot: MediaItemSnapshot) -> Value {
    let item = Item::from_ledger(&snapshot.media.title, &snapshot.row);
    let series_id = snapshot.media.id.to_string().replace('-', "");
    let item_id = if snapshot.is_series {
        series_id.clone()
    } else {
        item.id.clone()
    };
    let item_type = if snapshot.is_series {
        "Series"
    } else if snapshot.media.kind == domain::MediaKind::Tv {
        "Episode"
    } else {
        "Movie"
    };
    let item_path = std::path::Path::new(&item.path);
    let series_path = item_path.parent().map(|directory| {
        let show_directory = if snapshot.row.season.is_some() {
            directory.parent().unwrap_or(directory)
        } else {
            directory
        };
        show_directory.display().to_string()
    });
    let user_data = snapshot.user_item_data().json_value();

    let mut value = json!({
        "Id": item_id,
        "Name": if snapshot.media.kind == domain::MediaKind::Tv && !snapshot.is_series {
            format!("S{:02}E{:02}", snapshot.row.season.unwrap_or(0), snapshot.row.episode.unwrap_or(0))
        } else { item.name },
        "Path": if snapshot.is_series {
            series_path.unwrap_or(item.path)
        } else { item.path },
        "Type": item_type,
        "IsFolder": snapshot.is_series,
        "MediaType": "Video",
        "ParentId": snapshot.parent_id,
        "ProductionYear": snapshot.metadata.production_year,
        "OriginalTitle": snapshot.metadata.original_title,
        "PremiereDate": snapshot.metadata.premiere_date,
        "EndDate": snapshot.metadata.end_date,
        "OfficialRating": snapshot.metadata.official_rating,
        "Status": snapshot.metadata.status,
        "OriginalLanguage": snapshot.metadata.original_language,
        "Taglines": snapshot.metadata.taglines,
        "VoteCount": snapshot.metadata.vote_count,
        "Studios": snapshot.metadata.studios.iter().map(|name| json!({ "Name": name })).collect::<Vec<_>>(),
        "ProductionLocations": snapshot.metadata.production_locations,
        "RunTimeTicks": snapshot.metadata.runtime_ticks,
        "Overview": snapshot.metadata.overview,
        "CommunityRating": snapshot.metadata.community_rating,
        "Genres": snapshot.metadata.genres,
        "ProviderIds": snapshot.metadata.provider_ids,
        "People": snapshot
            .metadata
            .people
            .iter()
            .map(media_person_json)
            .collect::<Vec<_>>(),
        "UserData": user_data,
    });

    if snapshot.media.kind == domain::MediaKind::Tv && !snapshot.is_series {
        let season_num = snapshot.row.season.unwrap_or(1);
        value["SeriesId"] = json!(series_id);
        value["SeriesName"] = json!(snapshot.media.title);
        value["SeasonId"] = json!(format!("{series_id}{season_num:02}"));
        value["IndexNumber"] = json!(snapshot.row.episode);
        value["ParentIndexNumber"] = json!(snapshot.row.season);
        value["SeasonName"] = json!(format!("第 {} 季", season_num));
        value["ParentBackdropImageTags"] = json!(
            snapshot
                .metadata
                .has_backdrop_image
                .then_some("fanart")
                .into_iter()
                .collect::<Vec<_>>()
        );
        value["SeriesPrimaryImageTag"] = json!(snapshot.metadata.primary_image_tag.as_deref().unwrap_or("poster"));
        value["PrimaryImageAspectRatio"] = json!(1.7777777777777777);
    } else if snapshot.is_series {
        let season_count = snapshot
            .metadata
            .season_count
            .or_else(|| snapshot.metadata.season_count)
            .unwrap_or(1);
        value["ChildCount"] = json!(season_count);
        value["RecursiveItemCount"] = json!(snapshot.metadata.child_count.unwrap_or(0));
        value["NumberOfSeasons"] = json!(snapshot.metadata.season_count.unwrap_or(season_count));
        value["NumberOfEpisodes"] = json!(snapshot.metadata.number_of_episodes.map(|n| n as usize).or(snapshot.metadata.child_count).unwrap_or(0));
    }

    if let Some(date_created) = snapshot.metadata.date_created.as_ref() {
        value["DateCreated"] = json!(date_created);
    }

    if !snapshot.is_series {
        if let Some(name) = snapshot.metadata.episode_name.as_ref() {
            value["Name"] = json!(name);
        }
    }
    if snapshot.metadata.has_primary_image {
        let fallback_tag = if (snapshot.media.kind == domain::MediaKind::Tv
            && !snapshot.is_series
            && snapshot.row.episode.is_some())
            || snapshot.metadata.primary_image_url.is_some()
        {
            "episode"
        } else {
            "poster"
        };
        value["ImageTags"] = json!({
            "Primary": snapshot.metadata.primary_image_tag.as_deref().unwrap_or(fallback_tag)
        });
    }
    if !snapshot.is_series {
        let media_streams = media_streams_with_item(&snapshot.metadata.tracks, Some(&item_id));
        if !media_streams.is_empty() {
            value["MediaStreams"] = json!(media_streams);
        }
    }
    if snapshot.metadata.has_backdrop_image {
        value["BackdropImageTags"] = json!(["fanart"]);
    }

    let chapters = render_chapters_json(&snapshot);
    if !chapters.is_empty() {
        value["Chapters"] = json!(chapters);
    }

    value
}
