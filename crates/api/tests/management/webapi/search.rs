use super::*;

#[tokio::test]
async fn discover_without_catalog_returns_empty_sections() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/search/titles?keyword=test",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["ok"], true);
    assert!(body["data"]["titles"].is_array());
}

#[tokio::test]
async fn empty_keyword_torrent_search_browses_enabled_sites() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/search/torrents",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["keyword"], "");
}

#[tokio::test]
async fn torrent_search_stream_emits_events() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/search/torrents/stream?keyword=test",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 64)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("event:start") || text.contains("start"));
}

/// 单源 fake catalog（U3 provider 定向测试）。
struct SingleSourceCatalog {
    name: &'static str,
    movie: Option<domain::Media>,
    show: Option<domain::Media>,
}

impl api::catalog::Catalog for SingleSourceCatalog {
    fn source_name(&self) -> &'static str {
        self.name
    }
    fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(self
            .movie
            .clone()
            .map(|media| media::CatalogHit {
                media,
                poster_path: None,
                backdrop_path: None,
                rating: None,
                overview: None,
            })
            .into_iter()
            .collect())
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(self
            .show
            .clone()
            .map(|media| media::CatalogHit {
                media,
                poster_path: None,
                backdrop_path: None,
                rating: None,
                overview: None,
            })
            .into_iter()
            .collect())
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(
        &self,
        _kind: domain::MediaKind,
        _tmdb_id: &str,
    ) -> Result<Option<domain::Media>, String> {
        Ok(self.movie.clone())
    }
}

fn media_of(name: &'static str, kind: domain::MediaKind, tmdb: &str) -> domain::Media {
    domain::Media {
        id: domain::MediaId::new(),
        kind,
        title: name.into(),
        year: Some(2020),
        original_title: None,
        tmdb_id: Some(tmdb.into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn fanout_app(tmp: &tempfile::TempDir) -> axum::Router {
    let tmdb = Arc::new(SingleSourceCatalog {
        name: "tmdb",
        movie: Some(media_of("Inception", domain::MediaKind::Movie, "27205")),
        show: None,
    });
    let douban = Arc::new(SingleSourceCatalog {
        name: "douban",
        movie: Some(media_of("盗梦空间", domain::MediaKind::Movie, "27205")),
        show: None,
    });
    let fanout = api::catalog::FanoutCatalog::new(vec![tmdb, douban]);
    router(
        state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(fanout),
    )
}

#[tokio::test]
async fn title_search_provider_filters_to_single_source() {
    let tmp = tempfile::tempdir().unwrap();
    let app = fanout_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/search/titles?keyword=inception&provider=tmdb",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let providers = body["data"]["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 1, "定向搜索只列一个 provider");
    assert_eq!(providers[0]["provider"], "tmdb");
    assert_eq!(providers[0]["ok"], true);
    assert_eq!(providers[0]["count"], 1);
    let titles = body["data"]["titles"].as_array().unwrap();
    assert_eq!(titles.len(), 1);
    assert_eq!(titles[0]["provider"], "tmdb");
    assert_eq!(titles[0]["title"], "Inception");
}

#[tokio::test]
async fn title_search_unknown_provider_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let app = fanout_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/search/titles?keyword=inception&provider=nope",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "search.provider");

    let history = json_body(
        send(
            &app,
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    assert!(
        history["data"]["items"].as_array().unwrap().is_empty(),
        "a rejected provider must not create a history entry"
    );
}

#[tokio::test]
async fn title_search_default_fanout_reports_each_source() {
    let tmp = tempfile::tempdir().unwrap();
    let app = fanout_app(&tmp);
    let response = send(
        &app,
        "GET",
        "/api/v1/search/titles?keyword=inception",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    let body = json_body(response).await;
    let providers = body["data"]["providers"].as_array().unwrap();
    let names: Vec<&str> = providers
        .iter()
        .filter_map(|p| p["provider"].as_str())
        .collect();
    assert!(
        names.contains(&"tmdb") && names.contains(&"douban"),
        "fanout 应逐源报告: {names:?}"
    );
}

/// 受闸门的多源 fake catalog：占住搜索预算，直到测试放行。
struct GatedCatalog {
    name: &'static str,
    started: Arc<std::sync::Mutex<Vec<&'static str>>>,
    gate: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl GatedCatalog {
    fn search(&self) -> Result<Vec<media::CatalogHit>, String> {
        self.started.lock().unwrap().push(self.name);
        let (lock, cv) = &*self.gate;
        let mut open = lock.lock().unwrap();
        while !*open {
            open = cv.wait(open).unwrap();
        }
        Ok(Vec::new())
    }
}

impl api::catalog::Catalog for GatedCatalog {
    fn source_name(&self) -> &'static str {
        self.name
    }
    fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.search()
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(
        &self,
        _kind: domain::MediaKind,
        _tmdb_id: &str,
    ) -> Result<Option<domain::Media>, String> {
        Ok(None)
    }
}

/// 断言失败/提前返回时也放行闸门，避免 runtime 关闭时卡在 blocking 线程上。
struct OpenGate(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);

impl Drop for OpenGate {
    fn drop(&mut self) {
        *self.0 .0.lock().unwrap() = true;
        self.0 .1.notify_all();
    }
}

/// 单请求内元数据源超过并发预算时，多出来的源必须排队补位，而不是被本请求
/// 自己的预算判成过载（P1：标题搜索 fanout 自饥饿，默认 5 源恒定挂一个）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn title_search_beyond_the_window_still_searches_every_source() {
    let tmp = tempfile::tempdir().unwrap();
    let started = Arc::new(std::sync::Mutex::new(Vec::new()));
    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let _open = OpenGate(gate.clone());
    let sources: Vec<Arc<dyn api::catalog::Catalog>> = ["tmdb", "douban", "tvdb", "bangumi", "anilist"]
        .into_iter()
        .map(|name| {
            Arc::new(GatedCatalog {
                name,
                started: started.clone(),
                gate: gate.clone(),
            }) as Arc<dyn api::catalog::Catalog>
        })
        .collect();
    let fanout = api::catalog::FanoutCatalog::new(sources);
    let app = router(
        state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(fanout),
    );

    let token = "management-secret".to_string();
    let handle = tokio::spawn(async move {
        send(
            &app,
            "GET",
            "/api/v1/search/titles?keyword=inception",
            Some(&token),
            Value::Null,
        )
        .await
    });
    // 4 个 permit：窗口内只应开始 4 个源。
    for _ in 0..500 {
        if started.lock().unwrap().len() >= 4 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        started.lock().unwrap().len(),
        4,
        "窗口内只应开始 4 个源: {:?}",
        started.lock().unwrap()
    );
    // 放行：已完成 4 个之后，剩下 1 个必须补位；旧实现此时已把它判成过载。
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();

    let response = tokio::time::timeout(std::time::Duration::from_secs(10), handle)
        .await
        .expect("窗口补位不得让整请求挂起")
        .unwrap();
    let body = json_body(response).await;
    let providers = body["data"]["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 5, "5 个源都要报告: {body}");
    let failed: Vec<&Value> = providers.iter().filter(|p| p["ok"] != true).collect();
    assert!(
        failed.is_empty(),
        "同一请求内的数据源不得互相饿死: {failed:?}"
    );
    assert_eq!(
        started.lock().unwrap().len(),
        5,
        "5 个源都必须真的搜过: {:?}",
        started.lock().unwrap()
    );
}

#[tokio::test]
async fn search_history_snapshot_round_trip_and_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    assert_transient_search_not_saved(&app).await;
    let id = save_empty_search(&app).await;
    assert_empty_snapshot(&app, &id).await;
    assert_history_deletion(&app, &id).await;
}

/// 快照删不掉时必须如实回 500，并且**保留**历史条目：绝不能返回 200 却留下
/// 「条目在、快照没了」的死链（P3：历史删除不能静默成功）。
#[tokio::test]
async fn history_delete_reports_failure_and_keeps_entry_when_snapshot_delete_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let id = save_empty_search(&app).await;

    // 触发器拦截快照删除，模拟 store 写失败。
    let conn = rusqlite::Connection::open(tmp.path().join("data").join("app.db")).unwrap();
    conn.execute(
        "CREATE TRIGGER reject_snapshot_delete BEFORE DELETE ON search_snapshots
         BEGIN SELECT RAISE(FAIL, 'nope'); END;",
        [],
    )
    .unwrap();

    let deleted = send(
        &app,
        "DELETE",
        &format!("/api/v1/search/history/{id}"),
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(
        deleted.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "快照删除失败必须回 500，不能假装成功"
    );

    // 历史条目必须原样保留（先删快照、失败即中止，不动设置）。
    let history = json_body(
        send(
            &app,
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    let ids: Vec<&str> = history["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert!(
        ids.contains(&id.as_str()),
        "快照删除失败时历史条目必须保留: {history}"
    );

    // 清空路径同样必须回 500。
    let cleared = send(
        &app,
        "DELETE",
        "/api/v1/search/history",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(
        cleared.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "清空历史失败必须回 500"
    );
}

/// 历史删除是**单事务**：settings 写失败时，已删的快照必须一起回滚。
///
/// 旧顺序（先删快照、成功后再写设置）在这里会留下「条目在、快照 404」的死链，
/// 也就是 P3-1 复现的「点开历史返回 404」。断言写成「条目与快照同生同死」，
/// 而不是只断言条目还在——后者在死链状态下也成立。
#[tokio::test]
async fn history_delete_rolls_back_snapshot_when_settings_write_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let id = save_empty_search(&app).await;
    // 基线：删除前快照是可回放的。
    assert_empty_snapshot(&app, &id).await;

    // 只拦 settings 写入，让快照删除「成功」而设置写失败。
    let conn = rusqlite::Connection::open(tmp.path().join("data").join("app.db")).unwrap();
    conn.execute(
        "CREATE TRIGGER reject_settings_write BEFORE INSERT ON settings
         BEGIN SELECT RAISE(FAIL, 'nope'); END;",
        [],
    )
    .unwrap();

    let deleted = send(
        &app,
        "DELETE",
        &format!("/api/v1/search/history/{id}"),
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(
        deleted.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "设置写失败必须回 500"
    );

    let history = json_body(
        send(
            &app,
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    let entry_present = history["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["id"].as_str() == Some(id.as_str()));
    let snapshot = send(
        &app,
        "GET",
        &format!("/api/v1/search/history/{id}?vertical=torrents"),
        Some("management-secret"),
        Value::Null,
    )
    .await;
    let snapshot_present = snapshot.status() == StatusCode::OK;
    assert_eq!(
        entry_present, snapshot_present,
        "条目与快照必须同生同死：不允许「条目在、快照 404」的死链"
    );
    assert!(entry_present, "设置写失败时条目与快照都必须原样保留");

    // 清空路径同理：删除设置键失败时，已经删掉的快照必须一起回滚。
    conn.execute(
        "CREATE TRIGGER reject_settings_delete BEFORE DELETE ON settings
         BEGIN SELECT RAISE(FAIL, 'nope'); END;",
        [],
    )
    .unwrap();
    let cleared = send(
        &app,
        "DELETE",
        "/api/v1/search/history",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(
        cleared.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "清空时设置删除失败必须回 500"
    );
    let replay = send(
        &app,
        "GET",
        &format!("/api/v1/search/history/{id}?vertical=torrents"),
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(
        replay.status(),
        StatusCode::OK,
        "清空失败后快照必须仍可回放（不能只删掉一半）"
    );
}

#[tokio::test]
async fn title_search_snapshot_captures_results() {
    let tmp = tempfile::tempdir().unwrap();
    let app = fanout_app(&tmp);
    let searched = send(
        &app,
        "GET",
        "/api/v1/search/titles?keyword=inception",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(searched.status(), StatusCode::OK);

    let history = json_body(
        send(
            &app,
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    let items = history["data"]["items"].as_array().unwrap();
    let id = items[0]["id"].as_str().unwrap().to_string();
    let snapshot = json_body(
        send(
            &app,
            "GET",
            &format!("/api/v1/search/history/{id}?vertical=titles"),
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    assert_eq!(snapshot["data"]["vertical"], "titles");
    let titles = snapshot["data"]["items"].as_array().unwrap();
    assert_eq!(titles.len(), 2, "fanout 双源各 1 条");
    let names: Vec<&str> = titles.iter().filter_map(|t| t["title"].as_str()).collect();
    assert!(names.contains(&"Inception") && names.contains(&"盗梦空间"));
}

async fn assert_transient_search_not_saved(app: &axum::Router) {
    // 未明确请求保存时，空搜索结果不进入历史。
    let searched = send(
        &app,
        "GET",
        "/api/v1/search/torrents?keyword=transient",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(searched.status(), StatusCode::OK);

    let empty_history = json_body(
        send(
            &app,
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    assert!(
        empty_history["data"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

async fn save_empty_search(app: &axum::Router) -> String {
    // 无启用站点 → 空结果，但显式开启历史后仍记录历史与快照。
    let searched = send(
        &app,
        "GET",
        "/api/v1/search/torrents?keyword=matrix&save_history=true",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(searched.status(), StatusCode::OK);

    let history = json_body(
        send(
            &app,
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    let items = history["data"]["items"].as_array().unwrap();
    assert!(!items.is_empty(), "应有历史记录");
    let id = items[0]["id"].as_str().unwrap().to_string();
    assert_eq!(items[0]["query"], "matrix");

    id
}

async fn assert_empty_snapshot(app: &axum::Router, id: &str) {
    // 快照回放。
    let snapshot = json_body(
        send(
            &app,
            "GET",
            &format!("/api/v1/search/history/{id}?vertical=torrents"),
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    assert_eq!(snapshot["data"]["vertical"], "torrents");
    assert_eq!(snapshot["data"]["keyword"], "matrix");
    assert_eq!(snapshot["data"]["total"], 0);
}

async fn assert_history_deletion(app: &axum::Router, id: &str) {
    // 删除单条：快照 404 + 历史消失。
    let deleted = send(
        &app,
        "DELETE",
        &format!("/api/v1/search/history/{id}"),
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::OK);
    let gone = send(
        &app,
        "GET",
        &format!("/api/v1/search/history/{id}?vertical=torrents"),
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);

    // 清空。
    let cleared = send(
        &app,
        "DELETE",
        "/api/v1/search/history",
        Some("management-secret"),
        Value::Null,
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::OK);
    let history = json_body(
        send(
            &app,
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        )
        .await,
    )
    .await;
    assert_eq!(
        history["data"]["items"].as_array().unwrap().len(),
        0,
        "清空后历史应为空"
    );
}
