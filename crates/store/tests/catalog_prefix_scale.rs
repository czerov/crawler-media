use store::Store;

#[test]
fn catalog_details_bounded_vm_steps_at_scale() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let target_prefix = "/movie/42";
    store
        .put_catalog_cache("tmdb", "/movie/42", "{\"exact\":true}", 1000, None)
        .unwrap();
    store
        .put_catalog_cache(
            "tmdb",
            "/movie/42?language=zh-CN",
            "{\"detail\":\"zh\"}",
            1000,
            None,
        )
        .unwrap();

    // 1k baseline of unrelated rows
    for index in 0..1_000 {
        let key = format!("/other/{index}?query=1");
        store
            .put_catalog_cache("tmdb", &key, "{}", 500, None)
            .unwrap();
    }

    let (details_1k, vm_steps_1k) = store
        .catalog_details_for_prefixes_with_vm_steps("tmdb", &[target_prefix.to_string()])
        .unwrap();
    assert_eq!(details_1k.len(), 2);
    assert!(vm_steps_1k > 0, "vm_steps_1k must be > 0");

    // Grow unrelated rows to 50,000 (49,000 additional rows)
    for index in 1_000..50_000 {
        let key = format!("/other/{index}?query=1");
        store
            .put_catalog_cache("tmdb", &key, "{}", 500, None)
            .unwrap();
    }

    let (details_50k, vm_steps_50k) = store
        .catalog_details_for_prefixes_with_vm_steps("tmdb", &[target_prefix.to_string()])
        .unwrap();

    assert_eq!(details_50k, details_1k);
    assert!(
        vm_steps_50k <= vm_steps_1k * 2,
        "vm_steps_50k ({vm_steps_50k}) must be <= vm_steps_1k * 2 ({})",
        vm_steps_1k * 2
    );
}

fn put_catalog(store: &Store, key: &str, body: &str, fetched_at: i64) {
    store
        .put_catalog_cache("tmdb", key, body, fetched_at, None)
        .unwrap();
}

#[test]
fn catalog_details_edge_cases_and_semantics() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    // 1. Empty prefix list returns empty results with 0 vm steps
    let (empty_details, empty_steps) = store
        .catalog_details_for_prefixes_with_vm_steps("tmdb", &[])
        .unwrap();
    assert!(empty_details.is_empty());
    assert_eq!(empty_steps, 0);

    // 2. Exact key with no query suffix
    put_catalog(&store, "/movie/exact_only", "{\"exact\":1}", 100);
    let exact_res = store
        .catalog_details_for_prefixes("tmdb", &["/movie/exact_only".to_string()])
        .unwrap();
    assert_eq!(exact_res.len(), 1);
    assert_eq!(exact_res[0].cache_key, "/movie/exact_only");

    // 3. Two languages sharing one fetched_at: rowid ASC tie-break (first inserted wins)
    put_catalog(&store, "/movie/tie?lang=zh", "{\"lang\":\"zh\"}", 200);
    put_catalog(&store, "/movie/tie?lang=en", "{\"lang\":\"en\"}", 200);
    let tied_res = store
        .catalog_details_for_prefixes("tmdb", &["/movie/tie".to_string()])
        .unwrap();
    assert_eq!(tied_res.len(), 2);
    assert_eq!(tied_res[0].cache_key, "/movie/tie?lang=zh");

    // 4. Prefix looking like other numbers: "42" must not match "420"
    put_catalog(&store, "/movie/420?query=1", "{}", 300);
    let p42_res = store
        .catalog_details_for_prefixes("tmdb", &["/movie/42".to_string()])
        .unwrap();
    assert!(p42_res.iter().all(|r| !r.cache_key.starts_with("/movie/420")));

    // 5. Sub-paths like /images staying out of detail results
    put_catalog(&store, "/movie/42/images", "{}", 400);
    let images_res = store
        .catalog_details_for_prefixes("tmdb", &["/movie/42".to_string()])
        .unwrap();
    assert!(images_res.iter().all(|r| r.cache_key != "/movie/42/images"));

    // 6. '%' and '_' in a prefix treated as literal characters (not wildcards)
    put_catalog(&store, "/movie/100%", "{\"literal_pct\":1}", 500);
    put_catalog(&store, "/movie/100%?language=zh-CN", "{\"literal_pct_zh\":1}", 500);
    put_catalog(&store, "/movie/1000", "{\"other\":1}", 500);
    put_catalog(&store, "/movie/1000?language=en-US", "{\"other_en\":1}", 500);
    let pct_res = store
        .catalog_details_for_prefixes("tmdb", &["/movie/100%".to_string()])
        .unwrap();
    let pct_keys: Vec<_> = pct_res.iter().map(|r| r.cache_key.as_str()).collect();
    assert_eq!(pct_keys, vec!["/movie/100%", "/movie/100%?language=zh-CN"]);
    assert!(!pct_keys.contains(&"/movie/1000"));
    assert!(!pct_keys.contains(&"/movie/1000?language=en-US"));

    put_catalog(&store, "/movie/a_c", "{\"literal_under\":1}", 500);
    put_catalog(&store, "/movie/a_c?language=zh-CN", "{\"literal_under_zh\":1}", 500);
    put_catalog(&store, "/movie/abc", "{\"wildcard_match\":1}", 500);
    put_catalog(&store, "/movie/abc?language=en-US", "{\"wildcard_match_en\":1}", 500);
    let under_res = store
        .catalog_details_for_prefixes("tmdb", &["/movie/a_c".to_string()])
        .unwrap();
    let under_keys: Vec<_> = under_res.iter().map(|r| r.cache_key.as_str()).collect();
    assert_eq!(under_keys, vec!["/movie/a_c", "/movie/a_c?language=zh-CN"]);
    assert!(!under_keys.contains(&"/movie/abc"));
    assert!(!under_keys.contains(&"/movie/abc?language=en-US"));
}
