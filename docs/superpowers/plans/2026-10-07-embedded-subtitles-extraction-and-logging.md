# 提取内封字幕与丰富交付日志实施计划

> **历史状态（2026-10-13 整理）：** 内封字幕提取历史方案；当前实现及交付竞态应按最新 API review 核对，不能因旧复选框推定未实现，也不能据提取测试宣称真实播放器字幕验收通过。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现内封字幕（MKV/MP4 内封轨）通过 ffmpeg 按需提取与磁盘缓存交付，并将字幕交付相关日志补全为包含媒体剧名、物理路径、台账 ID 等详细上下文。

**Architecture:**
- 在 `crates/library` 中新增内封字幕提取模块，利用 `ffmpeg` 按流索引抽取指定字幕轨（支持 WebVTT 转换或原生复制），并以原子重命名方式写入磁盘缓存；
- 在 `crates/library::subtitles::delivery` 中提供包含视频源路径和缓存目录的交付函数 `deliver_subtitle_with_source`；
- 在 `crates/api` 与 `crates/media-server` 的字幕交付路由中接入该能力，并升级结构化 tracing 日志包含 `media`（剧名/电影名）、`path`（文件路径）、`ledger_id` / `item_id`、`index` 与 `error`。

**Tech Stack:** Rust (2024 edition), axum, ffmpeg/ffprobe, tracing, tempfile, SQLite (store)

## Global Constraints

- 遵循 Rust 代码规模软硬限制：单文件软限 200 行 / 硬限 800 行，生产函数软限 60 行 / 硬限 120 行。
- 遵循测试驱动（TDD）：测试必须在隔离环境下可独立运行，不依赖外部 qBittorrent 或真实外网 TMDB 服务。
- 遵循可观测性规则：所有错误路径必须输出结构化 tracing 日志，必须包含剧名等充足上下文信息。

---

### Task 1: 在 `crates/library` 实现内封字幕 ffmpeg 提取模块与单元测试

**Files:**
- Create: `crates/library/src/subtitles/extract.rs`
- Modify: `crates/library/src/subtitles/mod.rs`
- Modify: `crates/library/src/lib.rs`
- Test: `crates/library/tests/subtitles_delivery.rs`

**Interfaces:**
- Consumes: `crate::ProbeTarget`
- Produces: `library::subtitles::extract::extract_embedded_subtitle(media_path: &Path, stream_index: u32, codec: Option<&str>, wants_vtt: bool, cache_dir: &Path) -> Result<(PathBuf, &'static str), String>`

- [ ] **Step 1: 编写测试用例验证内封字幕提取与缓存命中逻辑**

在 `crates/library/tests/subtitles_delivery.rs` 中增加测试：
```rust
#[test]
fn deliver_subtitle_with_source_extracts_embedded_subtitle_with_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("cache");
    let media = tmp.path().join("movie.mkv");
    std::fs::write(&media, b"fake video bytes").unwrap();

    let tracks = Tracks {
        subtitles: vec![SubtitleTrack {
            stream_index: Some(2),
            codec: Some("subrip".into()),
            is_external: false,
            path: None,
            ..Default::default()
        }],
        ..Default::default()
    };

    // 预先在缓存目录中写入提取好的文件，验证命中缓存逻辑
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(cache_dir.join("sub_2.vtt"), b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nCached Sub").unwrap();

    let payload = library::deliver_subtitle_with_source(
        &tracks,
        2,
        true,
        Some(&media),
        Some(&cache_dir),
    )
    .unwrap();

    assert_eq!(payload.content_type, "text/vtt; charset=utf-8");
    assert_eq!(
        String::from_utf8(payload.bytes).unwrap(),
        "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nCached Sub"
    );
}
```

- [ ] **Step 2: 运行测试确保失败（缺少 deliver_subtitle_with_source）**

运行：`cargo test -p library --test subtitles_delivery`
预期：编译错误或失败

- [ ] **Step 3: 实现 `extract_embedded_subtitle` 及 `deliver_subtitle_with_source`**

1. 在 `crates/library/src/subtitles/extract.rs` 中利用 `ffmpeg` 提取：
   - 检查目标缓存文件是否存在且大小大于 0，存在则直接返回路径与 content_type。
   - 不存在时使用 `ProbeTarget::from_path(media_path).apply_ffmpeg_input(&mut cmd)`，配置 `-map 0:<stream_index>`，对于 vtt 输出 `-c:s webvtt -f webvtt`，对于 ass 输出 `-c:s copy -f ass`，对于 sup 输出 `-c:s copy -f sup`，默认 `-c:s subrip -f srt`。
   - 写入临时文件并原子 `rename` 到缓存文件。
2. 在 `crates/library/src/subtitles/delivery.rs` 中实现 `deliver_subtitle_with_source`。

- [ ] **Step 4: 运行 library 测试验证通过**

运行：`cargo test -p library --test subtitles_delivery`
预期：PASS

- [ ] **Step 5: 提交更改**

```bash
git add crates/library/src/subtitles/extract.rs crates/library/src/subtitles/delivery.rs crates/library/src/subtitles/mod.rs crates/library/src/lib.rs crates/library/tests/subtitles_delivery.rs
git commit -m "feat(library): add embedded subtitle extraction and cached delivery"
```

---

### Task 2: 在 `crates/api` 与 `crates/media-server` 接入内封字幕交付并增强日志

**Files:**
- Modify: `crates/api/src/http/playback.rs`
- Modify: `crates/media-server/src/routes/media/playback.rs`
- Test: `crates/api/tests/management/playback.rs`

**Interfaces:**
- Consumes: `library::deliver_subtitle_with_source`
- Produces: HTTP API `/playback/subtitles/{ledger_id}/{index}` 与 Jellyfin `/media-server/...` 支持内封字幕交付，并在失败时输出结构化日志：
  `tracing::error!(media = %media_title, path = %row.path, ledger_id = %ledger_id, index, %error, "交付字幕文件失败")`

- [ ] **Step 1: 在 `crates/api/tests/management/playback.rs` 增加内封字幕交付测试**

在 `playback_decide_and_subtitle_delivery_returns_tracks_and_content` 中加入对 `is_external: false, path: None` 字幕轨的测试验证：
```rust
    // 3. 验证内封字幕轨交付与缓存命中 (无 external 路径时走内封提取或缓存)
    let cache_dir = std::env::temp_dir().join("crawler-media-subtitles").join(row.id.to_string());
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(cache_dir.join("sub_3.vtt"), b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nEmbedded Subtitle Cached").unwrap();

    let store_arc = st.store();
    let store = store_arc.lock();
    store
        .put_file_meta(
            &row.id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack {
                    stream_index: Some(0),
                    codec: Some("h264".into()),
                    ..Default::default()
                }),
                audio: vec![],
                subtitles: vec![library::SubtitleTrack {
                    stream_index: Some(3),
                    codec: Some("subrip".into()),
                    profile: None,
                    language: Some("eng".into()),
                    title: Some("English Subtitle".into()),
                    bit_rate: None,
                    is_default: false,
                    forced: false,
                    is_external: false,
                    path: None,
                }],
            },
        )
        .unwrap();
    drop(store);

    let embedded_sub_url = format!("/api/v1/playback/subtitles/{}/3?format=vtt", row.id);
    let embedded_res = app
        .clone()
        .oneshot(request(
            "GET",
            &embedded_sub_url,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(embedded_res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(embedded_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body_bytes.to_vec()).unwrap();
    assert!(
        text.contains("Embedded Subtitle Cached"),
        "必须成功交付内封字幕 (G07)"
    );
```

- [ ] **Step 2: 改造 `get_subtitle_file` 与 `subtitle_stream`**

1. 在 `crates/api/src/http/playback.rs::get_subtitle_file`：
   - 获取 `row = resolve_visible_row(...)`。
   - 获取 `media_title = store.get_media(row.media_id).ok().flatten().map(|m| m.title).unwrap_or_default()`。
   - 调用 `library::deliver_subtitle_with_source(&tracks, index, wants_vtt, Some(source_path), Some(&cache_dir))`。
   - 在失败分支输出详细日志：
     ```rust
     tracing::error!(
         media = %media_title,
         path = %row.path,
         ledger_id = %ledger_id,
         index,
         %error,
         "交付字幕文件失败"
     );
     ```
2. 在 `crates/media-server/src/routes/media/playback.rs::subtitle_stream`：
   - 同样调用 `deliver_subtitle_with_source`。
   - 在失败分支输出详细日志：
     ```rust
     tracing::error!(
         media = %snapshot.media.title,
         path = %snapshot.row.path,
         item_id = %id,
         index,
         %error,
         "Jellyfin 交付字幕失败"
     );
     ```

- [ ] **Step 3: 运行 API 集成测试验证通过**

运行：`cargo test -p api --test management playback::playback_decide_and_subtitle_delivery_returns_tracks_and_content`
预期：PASS

- [ ] **Step 4: 运行 workspace 全量测试**

运行：`cargo test --workspace`
预期：全量测试通过

- [ ] **Step 5: 提交更改**

```bash
git add crates/api/src/http/playback.rs crates/media-server/src/routes/media/playback.rs crates/api/tests/management/playback.rs
git commit -m "feat(playback): deliver embedded subtitles with cache and enhance subtitle failure logging"
```
