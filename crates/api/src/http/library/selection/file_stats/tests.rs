use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use super::classify_metadata;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn capture(action: impl FnOnce()) -> String {
    let output = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(output.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, action);
    let bytes = output.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

fn test_row(media_id: domain::MediaId, path: &std::path::Path) -> domain::LedgerRow {
    domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id,
        path: path.display().to_string(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
    }
}

#[test]
fn rename_grace_stats_ignore_not_found_without_warn() {
    let tmp = tempfile::tempdir().unwrap();
    let live = tmp.path().join("live.strm");
    let gone = tmp.path().join("gone.strm");
    std::fs::write(&live, b"1234567").unwrap();
    let media_id = domain::MediaId::new();
    let rows = vec![test_row(media_id, &live), test_row(media_id, &gone)];
    let metadata = std::fs::metadata(&live).unwrap();
    let expected_time = metadata.created().or_else(|_| metadata.modified()).unwrap();

    let logs = capture(|| {
        let files = crate::browse_file_read::LocalBrowseFileRead;
        assert_eq!(super::total_size(&rows, &files), Some(7));
        assert_eq!(super::added_at(&rows, &files), Some(expected_time));
        assert_eq!(super::total_size(&rows[1..], &files), None);
        assert_eq!(super::added_at(&rows[1..], &files), None);
    });
    assert!(!logs.contains("WARN"), "expected no warnings, got: {logs}");
    assert!(logs.contains("DEBUG"), "expected debug log for not found");
}

#[test]
fn stats_warn_on_permission_denied() {
    let dummy_path = std::path::Path::new("/dummy/permission/denied.strm");
    let logs = capture(|| {
        let res = classify_metadata(
            dummy_path,
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "fixture permission denied")),
        );
        assert!(res.is_none());
    });
    assert!(logs.contains("WARN"), "permission denied must log warning: {logs}");
    assert!(logs.contains("fixture permission denied"));
}

#[test]
fn stats_entry_warns_when_metadata_read_is_denied() {
    let path = std::path::Path::new("/library/private.strm");
    let media_id = domain::MediaId::new();
    let row = test_row(media_id, path);
    let logs = capture(|| {
        let denied = |candidate: &std::path::Path| {
            assert_eq!(candidate, path);
            super::classify_metadata(
                candidate,
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "fixture permission denied",
                )),
            )
        };
        assert_eq!(super::total_size_with(&[row.clone()], denied), None);
        assert_eq!(super::added_at_with(&[row], denied), None);
    });
    assert!(logs.contains("WARN"), "stats entry must warn: {logs}");
    assert!(logs.contains("/library/private.strm"), "{logs}");
    assert!(logs.contains("fixture permission denied"), "{logs}");
}
