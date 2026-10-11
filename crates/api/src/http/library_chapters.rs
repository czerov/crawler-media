//! Video chapters: container chapter markers with scene frames beside the
//! file (`chapter_N.jpg`), served for the detail strip and the player.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::http::library::{library_for_row, preferred_row, require_visible_library};
use crate::http::{err, ok, ok_list};
use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;
use std::str::FromStr;

/// 自动生成：文件有内嵌章节且尚无章节帧时，逐章抽一帧（跳过已有）。
/// 供整库刷新与入库链路调用；失败静默（不影响主流程）。
pub(crate) fn auto_generate_chapters(state: &ApiState, path: &std::path::Path) {
    let enabled = {
        let store = state.store.lock();
        chapter_images_enabled(&store, path)
    };
    if !enabled {
        tracing::debug!(path = %path.display(), "媒体库已关闭章节场景图提取，跳过自动抓帧");
        return;
    }
    let kind = state
        .store
        .lock()
        .list_libraries()
        .unwrap_or_default()
        .into_iter()
        .find(|owner| owner.root_paths.iter().any(|root| path.starts_with(root)))
        .map(|owner| owner.kind);
    let lease = match kind
        .map(|kind| state.acquire_path_work(path, kind))
        .transpose()
    {
        Ok(lease) => lease.flatten(),
        Err(error) => {
            tracing::debug!(%error, "chapter generation rejected");
            return;
        }
    };
    let control = lease
        .as_ref()
        .map(|lease| lease.control().clone())
        .unwrap_or_default();
    let _ = generate_chapter_frames(path, &control);
}

/// 最长根目录前缀所属媒体库的「生成章节」开关。路径不落在任何库时默认开启，
/// 与入库自动抓帧的历史行为一致。
pub(crate) fn chapter_images_enabled(store: &crate::Store, path: &std::path::Path) -> bool {
    store
        .list_libraries()
        .ok()
        .and_then(|libraries| {
            libraries
                .into_iter()
                .filter(|lib| lib.root_paths.iter().any(|root| path.starts_with(root)))
                .max_by_key(|lib| {
                    lib.root_paths
                        .iter()
                        .filter(|root| path.starts_with(root))
                        .map(|root| root.components().count())
                        .max()
                        .unwrap_or(0)
                })
                .map(|lib| lib.extract_chapter_images)
        })
        .unwrap_or(true)
}

pub(crate) fn generate_chapter_frames_for_targets(
    path: &std::path::Path,
    targets: &[(i64, i64)],
) -> (usize, usize) {
    generate_chapter_frames_for_targets_with(
        &scene_frame_ffmpeg(),
        path,
        targets,
        &library::WorkControl::default(),
    )
}

fn generate_chapter_frames_for_targets_with(
    ffmpeg: &str,
    path: &std::path::Path,
    targets: &[(i64, i64)],
    control: &library::WorkControl,
) -> (usize, usize) {
    let mut generated = 0usize;
    for (index, &(start_ms, end_ms)) in targets.iter().enumerate() {
        if control.is_cancelled() {
            break;
        }
        let out = chapter_image_path(path, index);
        if out.is_file() {
            generated += 1;
            continue;
        }
        let midpoint = if end_ms > start_ms {
            start_ms + (end_ms - start_ms) / 2
        } else {
            start_ms
        };
        if extract_chapter_frame(path, midpoint, &out, ffmpeg, control).is_ok() {
            generated += 1;
        }
    }
    (generated, targets.len())
}

fn extract_chapter_frame(
    path: &std::path::Path,
    midpoint: i64,
    out: &std::path::Path,
    ffmpeg: &str,
    control: &library::WorkControl,
) -> Result<(), String> {
    // The child never writes the visible sidecar. Retain staging through reaping
    // and publish only under the cancellation gate.
    let staged = out.with_file_name(format!(".chapter-{}.jpg", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut command = std::process::Command::new(ffmpeg);
        command.args(["-y", "-ss", &format!("{:.3}", midpoint as f64 / 1000.0)]);
        library::ProbeTarget::from_path(path).apply_ffmpeg_input(&mut command);
        command.args(["-frames:v", "1", "-vf", "scale=960:-2", "-q:v", "4"]);
        command.arg(&staged);
        let output = control
            .output(&mut command)
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }
        control
            .signal()
            .publish(|| std::fs::rename(&staged, out).map_err(|error| error.to_string()))
    })();
    if staged.exists() {
        if let Err(error) = std::fs::remove_file(&staged) {
            tracing::warn!(%error, path = %staged.display(), "failed to remove staged chapter frame");
        }
    }
    if let Err(error) = &result {
        tracing::debug!(%error, path = %path.display(), "chapter frame extraction stopped");
    }
    result
}

pub(crate) fn trigger_scene_frames_for_chapter_updates(
    updates: &[(String, Vec<library::ChapterMarker>)],
    store: &std::sync::Arc<parking_lot::Mutex<crate::Store>>,
    registry: &std::sync::Arc<crate::library_lifecycle::LibraryWorkRegistry>,
) {
    for (ledger_id, chapters) in updates {
        if chapters.is_empty() {
            continue;
        }
        let targets: Vec<(i64, i64)> = chapters.iter().map(|c| (c.start_ms, c.end_ms)).collect();
        if targets.is_empty() {
            continue;
        }
        let row_path = store
            .lock()
            .get_ledger(&ledger_id.replace('-', ""))
            .ok()
            .flatten()
            .map(|r| std::path::PathBuf::from(r.path));
        if let Some(path) = row_path {
            if !chapter_images_enabled(&store.lock(), &path) {
                tracing::debug!(path = %path.display(), "媒体库已关闭章节场景图提取，跳过声纹章节抓帧");
                continue;
            }
            let owner = {
                let store = store.lock();
                store
                    .get_ledger(&ledger_id.replace('-', ""))
                    .ok()
                    .flatten()
                    .and_then(|row| {
                        store
                            .get_media(row.media_id)
                            .ok()
                            .flatten()
                            .and_then(|media| {
                                store
                                    .library_for_path_strict(&path, media.kind)
                                    .ok()
                                    .flatten()
                            })
                    })
            };
            let Some(owner) = owner else {
                continue;
            };
            let lease = match registry.acquire(store, &owner.id) {
                Ok(lease) => lease,
                Err(error) => {
                    tracing::debug!(%error, "chapter task rejected");
                    continue;
                }
            };
            let ffmpeg = scene_frame_ffmpeg();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn_blocking(move || {
                    generate_chapter_frames_for_targets_with(
                        &ffmpeg,
                        &path,
                        &targets,
                        lease.control(),
                    );
                });
            } else {
                std::thread::spawn(move || {
                    generate_chapter_frames_for_targets_with(
                        &ffmpeg,
                        &path,
                        &targets,
                        lease.control(),
                    );
                });
            }
        }
    }
}

thread_local! {
    static SCENE_FRAME_FFMPEG: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

fn scene_frame_ffmpeg() -> String {
    SCENE_FRAME_FFMPEG
        .with(|ffmpeg| ffmpeg.get().map(str::to_string))
        .unwrap_or_else(|| "ffmpeg".into())
}

/// 仅测试使用：让当前线程触发的场景图抓帧走指定 ffmpeg，不碰进程环境。
#[cfg(test)]
pub(crate) fn set_scene_frame_ffmpeg_for_test(ffmpeg: &'static str) {
    SCENE_FRAME_FFMPEG.with(|slot| slot.set(Some(ffmpeg)));
}

fn generate_chapter_frames(
    path: &std::path::Path,
    control: &library::WorkControl,
) -> Option<(usize, usize)> {
    let targets = probe_chapter_targets(path, control)?;
    Some(generate_chapter_frames_for_targets_with(
        &scene_frame_ffmpeg(),
        path,
        &targets,
        control,
    ))
}

fn probe_chapter_targets(
    path: &std::path::Path,
    control: &library::WorkControl,
) -> Option<Vec<(i64, i64)>> {
    let mut command = std::process::Command::new("ffprobe");
    command.args(["-v", "error", "-show_chapters", "-of", "json"]);
    library::ProbeTarget::from_path(path).apply_input(&mut command);
    let output = control
        .output(&mut command)
        .map_err(|error| {
            tracing::debug!(%error, path = %path.display(), "chapter probe stopped");
        })
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    Some(
        value["chapters"]
            .as_array()?
            .iter()
            .filter_map(|chapter| {
                let start = chapter["start_time"].as_str()?.parse::<f64>().ok()?;
                let end = chapter["end_time"].as_str()?.parse::<f64>().ok()?;
                Some(((start * 1000.0) as i64, (end * 1000.0) as i64))
            })
            .collect(),
    )
}

fn chapter_image_path(video: &std::path::Path, index: usize) -> std::path::PathBuf {
    let stem = video
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "video".into());
    video.with_file_name(format!("{stem}-chapter_{index}.jpg"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn manual_tv_refresh_always_queues_voiceprint() {
        assert!(
            super::should_queue_voiceprint_refresh(domain::MediaKind::Tv),
            "manual TV refresh must queue a complete voiceprint pass regardless of prior markers"
        );
    }

    #[test]
    fn manual_movie_refresh_does_not_queue_tv_voiceprint() {
        assert!(!super::should_queue_voiceprint_refresh(
            domain::MediaKind::Movie
        ));
    }
}

fn should_queue_voiceprint_refresh(media_kind: domain::MediaKind) -> bool {
    media_kind == domain::MediaKind::Tv
}

#[derive(Clone, Copy, Default, Deserialize)]
pub(crate) struct ChapterQuery {
    season: Option<u32>,
    episode: Option<u32>,
}

fn selected_row(rows: &[domain::LedgerRow], query: ChapterQuery) -> Option<domain::LedgerRow> {
    match (query.season, query.episode) {
        (Some(season), Some(episode)) => rows
            .iter()
            .find(|row| row.season == Some(season) && row.episode == Some(episode))
            .cloned(),
        _ => preferred_row(rows).cloned(),
    }
}

/// GET /libraries/{id}/items/{item_id}/chapters — probe chapters, with
/// `image_url` for the ones that already have a generated scene frame.
pub(crate) async fn chapters(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<ChapterQuery>,
) -> Response {
    let media_id = match domain::MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let (row, media) = {
        let store = state.store.lock();
        let library = match require_visible_library(&store, &id, user_id) {
            Ok(library) => library,
            Err(response) => return response,
        };
        let Some(media) = store.get_media(media_id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
        };
        let rows: Vec<domain::LedgerRow> = store
            .list_ledger()
            .unwrap_or_default()
            .into_iter()
            .filter(|row| row.media_id == media_id)
            .filter(|row| {
                library_for_row(&store, row, &media).is_some_and(|owner| owner.id == library.id)
            })
            .collect();
        let Some(row) = selected_row(&rows, query) else {
            return err(
                StatusCode::NOT_FOUND,
                "library.item_missing",
                "条目没有在位文件",
            );
        };
        (row, media)
    };
    let theintrodb_client = {
        let store = state.store.lock();
        let cfg = store.get_scrape_config().ok();
        let eff = cfg.as_ref().map(|config| &config.effective);
        let enabled = eff.map(|config| config.theintrodb_enabled).unwrap_or(false);
        let key = eff.and_then(|config| config.theintrodb_api_key.clone());
        (enabled && key.is_some()).then(|| crate::theintrodb::TheIntroDbClient::new(key))
    };
    let resolved =
        crate::marker_resolver::resolve_item_chapters(&state, &row, &media, theintrodb_client)
            .await;

    if resolved.is_empty() {
        return ok_list(Vec::new()).into_response();
    }
    let items: Vec<Value> = resolved
        .iter()
        .enumerate()
        .map(|(index, chapter)| {
            let image_url = chapter_image_path(std::path::Path::new(&row.path), index)
                .is_file()
                .then(|| format!("/chapters/{}/{index}", row.id));
            let mut obj = json!({
                "start_ms": chapter.start_ms,
                "end_ms": chapter.end_ms,
                "title": chapter.title,
                "image_url": image_url,
            });
            if let Some(mt) = chapter.marker_type {
                let tag = match mt {
                    library::MarkerType::IntroStart => "IntroStart",
                    library::MarkerType::IntroEnd => "IntroEnd",
                    library::MarkerType::CreditsStart => "CreditsStart",
                };
                obj["marker_type"] = json!(tag);
            }
            obj
        })
        .collect();
    ok_list(items).into_response()
}

/// POST /libraries/{id}/items/{item_id}/chapters/refresh — force clear cached
/// chapters and persistent markers, then re-resolve from TheIntroDB / ffprobe.
pub(crate) async fn refresh_item_chapters(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<ChapterQuery>,
) -> Response {
    let media_id = match domain::MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let (row, media, rows) = {
        let store = state.store.lock();
        let library = match require_visible_library(&store, &id, user_id) {
            Ok(library) => library,
            Err(response) => return response,
        };
        let Some(media) = store.get_media(media_id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
        };
        let rows: Vec<domain::LedgerRow> = store
            .list_ledger()
            .unwrap_or_default()
            .into_iter()
            .filter(|row| row.media_id == media_id)
            .filter(|row| {
                library_for_row(&store, row, &media).is_some_and(|owner| owner.id == library.id)
            })
            .collect();
        let Some(row) = selected_row(&rows, query) else {
            return err(
                StatusCode::NOT_FOUND,
                "library.item_missing",
                "条目没有在位文件",
            );
        };
        (row, media, rows)
    };

    let theintrodb_client = {
        let store = state.store.lock();
        let cfg = store.get_scrape_config().ok();
        let eff = cfg.as_ref().map(|config| &config.effective);
        let enabled = eff.map(|config| config.theintrodb_enabled).unwrap_or(false);
        let key = eff.and_then(|config| config.theintrodb_api_key.clone());
        (enabled && key.is_some()).then(|| crate::theintrodb::TheIntroDbClient::new(key))
    };
    let refreshed = if media.kind == domain::MediaKind::Tv {
        crate::marker_resolver::resolve_item_chapters(&state, &row, &media, theintrodb_client).await
    } else {
        crate::marker_resolver::force_refresh_item_chapters(&state, &row, &media, theintrodb_client)
            .await
    };

    let season = row.season.unwrap_or(1);
    let fingerprint_rows: Vec<_> = rows
        .iter()
        .filter(|candidate| candidate.season.unwrap_or(1) == season)
        .cloned()
        .collect();
    let season_probe_was_active = fingerprint_rows
        .iter()
        .any(|candidate| state.probe.is_queued(&candidate.id.to_string()));
    let mut probe_queued = 0;
    if should_queue_voiceprint_refresh(media.kind) && !season_probe_was_active {
        let units = fingerprint_rows
            .iter()
            .map(|candidate_row| crate::probe_manager::ProbeUnit {
                row: candidate_row.clone(),
                kind: media.kind,
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: true,
                marker_refresh_id: None,
                job_id: None,
            })
            .collect();
        probe_queued = state.probe.enqueue_marker_refresh(units);
        tracing::info!(
            media = %media.title,
            season,
            rows = fingerprint_rows.len(),
            queued = probe_queued,
            "【片头片尾】用户强制刷新，已排队同季完整声纹探测"
        );
    } else if season_probe_was_active {
        tracing::info!(
            media = %media.title,
            season,
            rows = fingerprint_rows.len(),
            "【片头片尾】该剧该季已有探测任务，跳过重复刷新"
        );
    }

    let items: Vec<Value> = refreshed
        .iter()
        .enumerate()
        .map(|(index, chapter)| {
            let image_url = chapter_image_path(std::path::Path::new(&row.path), index)
                .is_file()
                .then(|| format!("/chapters/{}/{index}", row.id));
            let mut obj = json!({
                "start_ms": chapter.start_ms,
                "end_ms": chapter.end_ms,
                "title": chapter.title,
                "image_url": image_url,
            });
            if let Some(mt) = chapter.marker_type {
                let tag = match mt {
                    library::MarkerType::IntroStart => "IntroStart",
                    library::MarkerType::IntroEnd => "IntroEnd",
                    library::MarkerType::CreditsStart => "CreditsStart",
                };
                obj["marker_type"] = json!(tag);
            }
            obj
        })
        .collect();
    let marker_job = match state.probe.marker_refresh_job(media_id, season) {
        Ok(job) => job,
        Err(error) => {
            tracing::error!(%error, %media_id, season, "读取片头片尾刷新任务状态失败");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "library.probe_status_failed",
                "读取片头片尾任务状态失败，请稍后重试",
            )
            .into_response();
        }
    };
    let marker_job_active = marker_job.as_ref().is_some_and(|job| job.is_active());
    let active_after_enqueue = fingerprint_rows
        .iter()
        .any(|candidate| state.probe.is_queued(&candidate.id.to_string()));
    let already_running =
        probe_queued == 0 && (marker_job_active || season_probe_was_active || active_after_enqueue);
    let enqueue_failed =
        should_queue_voiceprint_refresh(media.kind) && probe_queued == 0 && !already_running;
    ok(json!({
        "chapters": items,
        "fingerprint_refresh_queued": probe_queued > 0 || marker_job_active,
        "fingerprint_refresh_already_running": already_running,
        "fingerprint_refresh_error": enqueue_failed.then_some("声纹刷新任务未能入队"),
        "fingerprint_refresh_job": marker_job.as_ref().map(probe_job_json),
    }))
    .into_response()
}

/// GET /libraries/{id}/items/{item_id}/probe-status?season=N — persistent
/// progress for the latest forced marker refresh in this season.
pub(crate) async fn probe_status(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<ChapterQuery>,
) -> Response {
    let media_id = match domain::MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let season = query.season.unwrap_or(1);
    {
        let store = state.store.lock();
        let library = match require_visible_library(&store, &id, user_id) {
            Ok(library) => library,
            Err(response) => return response,
        };
        let Some(media) = store.get_media(media_id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
        };
        let visible = store.list_ledger().unwrap_or_default().iter().any(|row| {
            row.media_id == media_id
                && row.season.unwrap_or(1) == season
                && library_for_row(&store, row, &media).is_some_and(|owner| owner.id == library.id)
        });
        if !visible {
            return err(
                StatusCode::NOT_FOUND,
                "library.item_missing",
                "该季没有在位文件",
            );
        }
    }
    let job = match state.probe.marker_refresh_job(media_id, season) {
        Ok(job) => job,
        Err(error) => {
            tracing::error!(%error, %media_id, season, "读取片头片尾任务状态失败");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "library.probe_status_failed",
                "读取片头片尾任务状态失败，请稍后重试",
            )
            .into_response();
        }
    };
    let active = job.as_ref().is_some_and(|job| job.is_active());
    ok(json!({
        "active": active,
        "job": job.as_ref().map(probe_job_json),
    }))
    .into_response()
}

fn probe_job_json(job: &crate::store::ProbeJob) -> Value {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default();
    json!({
        "id": job.id,
        "kind": job.kind,
        "season": job.season,
        "status": job.status,
        "total": job.total,
        "completed": job.completed,
        "succeeded": job.succeeded,
        "failed": job.failed,
        "error": job.error,
        "elapsed_ms": job.elapsed_ms(now_ms),
        "phase": Value::Null,
        "sampling_mode": Value::Null,
        "queue_wait_ms": Value::Null,
        "priority_wait_ms": Value::Null,
        "metrics": {
            "input_bytes": Value::Null,
            "measurement_complete": false,
        },
    })
}

/// POST /libraries/{id}/items/{item_id}/chapters/generate — extract one scene
/// frame per chapter beside the file.
pub(crate) async fn generate_chapters(
    State(state): State<ApiState>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<ChapterQuery>,
) -> Response {
    let lease = match state.acquire_library_work(&id) {
        Ok(lease) => lease,
        Err(error) => return err(StatusCode::CONFLICT, "library.frozen", &error),
    };
    let media_id = match domain::MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let (row, media) = {
        let store = state.store.lock();
        let Some(library) = store.get_library(&id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在");
        };
        let Some(media) = store.get_media(media_id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
        };
        let rows: Vec<domain::LedgerRow> = store
            .list_ledger()
            .unwrap_or_default()
            .into_iter()
            .filter(|row| row.media_id == media_id)
            .filter(|row| {
                library_for_row(&store, row, &media).is_some_and(|owner| owner.id == library.id)
            })
            .collect();
        let Some(row) = selected_row(&rows, query) else {
            return err(
                StatusCode::NOT_FOUND,
                "library.item_missing",
                "条目没有在位文件",
            );
        };
        (row, media)
    };
    let path = std::path::PathBuf::from(row.path.clone());
    let probe_lease = match state.acquire_library_work(&id) {
        Ok(lease) => lease,
        Err(error) => return err(StatusCode::CONFLICT, "library.frozen", &error),
    };
    let mut targets: Vec<(i64, i64)> = match tokio::task::spawn_blocking({
        let path = path.clone();
        move || probe_chapter_targets(&path, probe_lease.control())
    })
    .await
    .ok()
    .flatten()
    {
        Some(targets) => targets,
        None => Vec::new(),
    };

    if targets.is_empty() {
        let theintrodb_client = {
            let store = state.store.lock();
            let cfg = store.get_scrape_config().ok();
            let eff = cfg.as_ref().map(|config| &config.effective);
            let enabled = eff.map(|config| config.theintrodb_enabled).unwrap_or(false);
            let key = eff.and_then(|config| config.theintrodb_api_key.clone());
            (enabled && key.is_some()).then(|| crate::theintrodb::TheIntroDbClient::new(key))
        };
        let resolved =
            crate::marker_resolver::resolve_item_chapters(&state, &row, &media, theintrodb_client)
                .await;
        targets = resolved.iter().map(|c| (c.start_ms, c.end_ms)).collect();
    }

    if targets.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "chapters.none",
            "该文件没有内嵌章节且无可用片头片尾标记",
        );
    }

    let ffmpeg = scene_frame_ffmpeg();
    let (generated, total) = tokio::task::spawn_blocking(move || {
        generate_chapter_frames_for_targets_with(&ffmpeg, &path, &targets, lease.control())
    })
    .await
    .unwrap_or((0, 0));

    ok(json!({ "generated": generated, "total": total })).into_response()
}

/// GET /chapters/{ledger_id}/{index} — a generated chapter scene frame.
pub(crate) async fn chapter_image(
    State(state): State<ApiState>,
    Path((ledger_id, index)): Path<(String, String)>,
) -> Response {
    let compact = ledger_id.replace('-', "");
    let row = {
        let store = state.store.lock();
        match store.get_ledger(&compact) {
            Ok(Some(row)) => row,
            _ => return err(StatusCode::NOT_FOUND, "chapters.missing", "章节帧不存在"),
        }
    };
    let index: usize = match index.parse() {
        Ok(index) => index,
        Err(_) => return err(StatusCode::BAD_REQUEST, "chapters.invalid", "章节下标无效"),
    };
    let frame = chapter_image_path(std::path::Path::new(&row.path), index);
    let frame = frame.is_file().then_some(frame);
    let Some(frame) = frame else {
        return err(StatusCode::NOT_FOUND, "chapters.missing", "章节帧不存在");
    };
    match tokio::task::spawn_blocking(move || std::fs::read(frame)).await {
        Ok(Ok(bytes)) => {
            ([(axum::http::header::CONTENT_TYPE, "image/jpeg")], bytes).into_response()
        }
        _ => err(StatusCode::NOT_FOUND, "chapters.missing", "章节帧不存在"),
    }
}
