use std::fs;
use std::os::unix::fs::PermissionsExt;
use library::extract_frame_with_ffmpeg;

fn fake_ffmpeg(dir: &std::path::Path) -> std::path::PathBuf {
    let program = dir.join("ffmpeg");
    fs::write(
        &program,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/argv.txt\"\nfor last; do :; done\n: > \"$last\"\nexit 0\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&program).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&program, permissions).unwrap();
    program
}

#[test]
fn extract_frame_seeks_the_url_inside_a_strm_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg(tmp.path());
    let strm = tmp.path().join("episode.strm");
    fs::write(&strm, "https://cdn.example/ep.mkv\n").unwrap();
    let jpeg = tmp.path().join("episode-thumb.jpg");
    extract_frame_with_ffmpeg(&strm, 60_000, &jpeg, ffmpeg.to_str().unwrap()).unwrap();
    let argv = fs::read_to_string(tmp.path().join("argv.txt")).unwrap();
    let input_at = argv.find("-i\n").expect(&argv);
    let url_at = argv.find("https://cdn.example/ep.mkv").expect(&argv);
    let out_at = argv.find("episode-thumb.jpg").expect(&argv);
    assert!(input_at < url_at && url_at < out_at, "{argv}");
    assert!(argv.contains("-ss\n60"), "{argv}");
    assert!(jpeg.is_file());
}

#[test]
fn extract_frame_passes_a_local_file_after_input_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg(tmp.path());
    let video = tmp.path().join("episode.mkv");
    fs::write(&video, b"not a real video").unwrap();
    let jpeg = tmp.path().join("episode-thumb.jpg");
    extract_frame_with_ffmpeg(&video, 60_000, &jpeg, ffmpeg.to_str().unwrap()).unwrap();
    let argv = fs::read_to_string(tmp.path().join("argv.txt")).unwrap();
    let input_at = argv.find("-i\n").expect(&argv);
    let file_at = argv.find("episode.mkv").expect(&argv);
    let out_at = argv.find("episode-thumb.jpg").expect(&argv);
    assert!(input_at < file_at && file_at < out_at, "{argv}");
}
