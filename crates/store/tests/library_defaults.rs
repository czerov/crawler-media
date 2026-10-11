use domain::MediaKind;
use store::Store;

#[test]
fn deleting_default_library_promotes_next_and_survives_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();

    let movie_root_2 = tmp.path().join("movies_2");
    std::fs::create_dir_all(&movie_root_2).unwrap();

    // 1. 创建第二电影库
    let extra_movie = store
        .create_library(
            MediaKind::Movie,
            "Extra Movie Lib",
            &[movie_root_2.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let default_movie = store.default_library(MediaKind::Movie).unwrap().unwrap();
    assert_ne!(default_movie.id, extra_movie.id);

    // 2. 删除原默认电影库
    let operation = store.begin_library_removal(&default_movie.id).unwrap();
    store.set_removal_state(&operation.operation_id, "cleaning", None).unwrap();
    store.clean_library_removal(&operation.operation_id).unwrap();

    // 3. 验证 extra_movie 已被自动提升为默认电影库
    let current_default = store.default_library(MediaKind::Movie).unwrap().unwrap();
    assert_eq!(
        current_default.id, extra_movie.id,
        "删除默认库后应自动将剩余同类型库提升为默认库"
    );

    // 4. 重启 Store：绝不可因缺少默认库而启动崩溃
    drop(store);
    let reopened = Store::open(&data_dir);
    assert!(reopened.is_ok(), "重启必须成功打开数据库");
    let reopened_store = reopened.unwrap();
    let reopened_default = reopened_store
        .default_library(MediaKind::Movie)
        .unwrap()
        .unwrap();
    assert_eq!(reopened_default.id, extra_movie.id);
}

#[test]
fn first_video_library_becomes_default_and_survives_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();

    let video_root = tmp.path().join("videos");
    std::fs::create_dir_all(&video_root).unwrap();

    // 首次创建 Video 媒体库
    let video_lib = store
        .create_library(
            MediaKind::Video,
            "Personal Videos",
            &[video_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let default_video = store.default_library(MediaKind::Video).unwrap();
    assert!(
        default_video.is_some(),
        "首个 Video 库必须自动成为该类型的默认库"
    );
    assert_eq!(default_video.unwrap().id, video_lib.id);
    assert!(
        !video_lib.detect_intros,
        "新建库默认关闭片头片尾识别"
    );
    assert!(!video_lib.enable_fingerprint, "新建库默认关闭声纹");

    // 重启 Store 必须成功
    drop(store);
    let reopened = Store::open(&data_dir);
    assert!(reopened.is_ok(), "含 Video 库的数据库重启必须成功");
}
