use std::net::IpAddr;

use crate::IndexerError;

/// Normalizes a host for consistent grouping and comparison.
/// Strips brackets, trailing dots, canonicalizes IP addresses, and converts IDN to ASCII punycode.
pub fn normalize_host(raw: &str) -> Result<String, IndexerError> {
    let mut s = raw.trim();
    if s.is_empty() {
        return Err(IndexerError::BrowserSetup("empty host".into()));
    }
    if s.starts_with('[') && s.ends_with(']') && s.len() >= 2 {
        s = &s[1..s.len() - 1];
    }
    s = s.trim_end_matches('.');
    if s.is_empty() {
        return Err(IndexerError::BrowserSetup("empty host".into()));
    }

    if let Ok(ip) = s.parse::<IpAddr>() {
        return Ok(ip.to_string());
    }

    idna::domain_to_ascii(s)
        .map_err(|e| IndexerError::BrowserSetup(format!("invalid host '{raw}': {e}")))
}

/// Derives a grouping key for concurrency control based on registrable domain (eTLD+1).
/// Like [`group_key`] but fails when the host cannot be normalized, instead of
/// grouping by the raw string. Used on the request path.
pub fn try_group_key(host: &str) -> Result<String, crate::IndexerError> {
    let normalized = normalize_host(host)?;
    Ok(group_key_normalized(normalized))
}

/// Infallible variant: an unnormalizable host groups by its raw lowercase form.
/// Prefer [`try_group_key`] on any request path.
pub fn group_key(host: &str) -> String {
    match normalize_host(host) {
        Ok(n) => group_key_normalized(n),
        Err(_) => host.trim().to_ascii_lowercase(),
    }
}

fn group_key_normalized(normalized: String) -> String {

    if normalized.parse::<IpAddr>().is_ok() {
        return normalized;
    }

    if normalized == "localhost" || normalized.ends_with(".localhost") {
        return "localhost".into();
    }

    if !normalized.contains('.') {
        return normalized;
    }

    match psl::domain_str(&normalized) {
        Some(domain) => domain.to_string(),
        None => normalized,
    }
}
