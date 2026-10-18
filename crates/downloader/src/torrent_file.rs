//! Minimal bencode walk to compute a .torrent's v1 info hash (D-H2).

use sha1::{Digest, Sha1};

/// SHA-1 of the raw `info` dictionary, lowercase hex; `None` if `bytes` is
/// not a bencoded dictionary with an `info` entry.
pub fn torrent_info_hash(bytes: &[u8]) -> Option<String> {
    if bytes.first() != Some(&b'd') {
        return None;
    }
    let mut i = 1;
    while *bytes.get(i)? != b'e' {
        let (key, after_key) = read_string(bytes, i)?;
        let end = skip(bytes, after_key, 0)?;
        if key == b"info" {
            let digest = Sha1::digest(&bytes[after_key..end]);
            return Some(digest.iter().map(|b| format!("{b:02x}")).collect());
        }
        i = end;
    }
    None
}

fn read_string(bytes: &[u8], i: usize) -> Option<(&[u8], usize)> {
    let colon = i + bytes.get(i..)?.iter().position(|b| *b == b':')?;
    let len: usize = std::str::from_utf8(&bytes[i..colon]).ok()?.parse().ok()?;
    let start = colon + 1;
    let end = start.checked_add(len)?;
    Some((bytes.get(start..end)?, end))
}

/// Review M-5: deepest list/dict nesting accepted. Real torrents nest a few
/// levels; site responses are untrusted, so recursion is bounded.
const MAX_DEPTH: usize = 64;

/// End index (exclusive) of the bencoded value starting at `i`.
fn skip(bytes: &[u8], i: usize, depth: usize) -> Option<usize> {
    match *bytes.get(i)? {
        b'i' => Some(i + bytes.get(i..)?.iter().position(|b| *b == b'e')? + 1),
        b'l' | b'd' if depth < MAX_DEPTH => {
            let mut j = i + 1;
            while *bytes.get(j)? != b'e' {
                j = skip(bytes, j, depth + 1)?;
            }
            Some(j + 1)
        }
        b'0'..=b'9' => read_string(bytes, i).map(|(_, end)| end),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::torrent_info_hash;

    #[test]
    fn hashes_exactly_the_raw_info_dictionary() {
        let info = b"d6:lengthi12e4:name8:demo.mkv12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";
        let mut torrent = b"d8:announce16:https://t.ex/ann4:info".to_vec();
        torrent.extend_from_slice(info);
        torrent.extend_from_slice(b"e");
        use sha1::Digest;
        let expected: String = sha1::Sha1::digest(info).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(torrent_info_hash(&torrent).as_deref(), Some(expected.as_str()));
        // Known vector: SHA-1 of "d4:name1:ae".
        assert_eq!(
            torrent_info_hash(b"d4:infod4:name1:aee").as_deref(),
            Some(sha1_hex(b"d4:name1:ae").as_str())
        );
    }

    fn sha1_hex(data: &[u8]) -> String {
        use sha1::Digest;
        sha1::Sha1::digest(data).iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Review M-5: hostile nesting must not overflow the worker stack.
    #[test]
    fn deeply_nested_info_is_rejected_without_overflow() {
        let depth = 100_000;
        let mut torrent = b"d4:info".to_vec();
        torrent.extend(std::iter::repeat_n(b'l', depth));
        torrent.extend(std::iter::repeat_n(b'e', depth));
        torrent.push(b'e');
        assert_eq!(torrent_info_hash(&torrent), None);
        // Ordinary nesting still works.
        assert!(torrent_info_hash(b"d4:infod5:filesld4:pathl1:aeeeee").is_some());
    }

    #[test]
    fn rejects_non_torrents() {
        assert_eq!(torrent_info_hash(b"<html>"), None);
        assert_eq!(torrent_info_hash(b"d8:announce3:abce"), None);
        assert_eq!(torrent_info_hash(b"d4:infod4:name"), None);
    }
}
