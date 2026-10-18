use domain::Site;

use crate::IndexerError;
use crate::profile::{Framework, Profile};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchMethod {
    Get,
    PostJson,
    PostForm,
}
#[derive(Clone)]
pub struct FetchRequest {
    pub key: String,
    pub url: String,
    pub method: FetchMethod,
    pub body: Option<String>,
    pub cookie: Option<String>,
    pub api_key: Option<String>,
    pub proxy: Option<String>,
    pub cdp_url: Option<String>,
}

pub trait Fetcher: Send + Sync {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError>;
}

pub fn effective_base_url(site: &Site, profile: &Profile) -> String {
    if let Some(base) = &profile.base_url {
        base.trim_end_matches('/').to_string()
    } else {
        site.url.trim_end_matches('/').to_string()
    }
}

pub fn search_request(site: &Site, profile: &Profile, keyword: &str) -> FetchRequest {
    search_request_page(site, profile, keyword, 1)
}

pub fn search_request_page(
    site: &Site,
    profile: &Profile,
    keyword: &str,
    page: u32,
) -> FetchRequest {
    search_request_page_categories(site, profile, keyword, page, &[])
}

pub fn search_request_page_categories(
    site: &Site,
    profile: &Profile,
    keyword: &str,
    page: u32,
    categories: &[String],
) -> FetchRequest {
    let base = effective_base_url(site, profile);
    let category_ids = profile
        .categories
        .as_ref()
        .map(|mapping| mapping.ids(categories))
        .unwrap_or_default();
    let path = if profile.search.path.starts_with('/') {
        profile.search.path.clone()
    } else {
        format!("/{}", profile.search.path)
    };
    let (method, url, body) = match profile.framework {
        Framework::Nexusphp => {
            let mut url = format!(
                "{base}{path}?{}={}",
                profile.search.query_param,
                form_encode(keyword)
            );
            if page > 1 {
                let param = profile.search.page_param.as_deref().unwrap_or("page");
                let page_start = profile.search.page_start.unwrap_or(0);
                let page_number = page_start.saturating_add(page - 1);
                url.push_str(&format!("&{}={page_number}", form_encode(param)));
            }
            let param = profile.search.category_param.as_deref().unwrap_or("cat");
            for id in &category_ids {
                let value = id
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| id.to_string());
                url.push_str(&format!("&{}={}", form_encode(param), form_encode(&value)));
            }
            (FetchMethod::Get, url, None)
        }
        Framework::Api => {
            let mut search_json = serde_json::json!({
                "mode": "normal",
                "keyword": keyword,
                "pageSize": profile.search.page_size.unwrap_or(100),
            });
            let page_param = profile.search.page_param.as_deref().unwrap_or("pageNumber");
            let page_start = profile.search.page_start.unwrap_or(1);
            search_json[page_param] = serde_json::json!(page_start.saturating_add(page - 1));
            if !category_ids.is_empty() {
                let param = profile
                    .search
                    .category_param
                    .as_deref()
                    .unwrap_or("categories");
                search_json[param] = serde_json::Value::Array(category_ids);
            }
            (
                FetchMethod::PostJson,
                format!("{base}{path}"),
                Some(search_json.to_string()),
            )
        }
    };
    FetchRequest {
        key: format!("search:{}", site.id),
        url,
        method,
        body,
        cookie: site.cookie.clone(),
        api_key: site.api_key.clone(),
        proxy: site.proxy.clone(),
        cdp_url: site.cdp_url.clone(),
    }
}

pub fn rss_request(site: &Site, profile: &Profile) -> FetchRequest {
    FetchRequest {
        key: format!("rss:{}", site.id),
        url: site.rss_url.clone().unwrap_or_else(|| site.url.clone()),
        method: FetchMethod::Get,
        body: None,
        cookie: site.cookie.clone(),
        api_key: site.api_key.clone(),
        proxy: site.proxy.clone(),
        cdp_url: site.cdp_url.clone(),
    }
}

pub fn download_token_request(site: &Site, profile: &Profile, id: &str) -> Option<FetchRequest> {
    let dl_cfg = profile.download.as_ref()?;
    let base = effective_base_url(site, profile);
    let path = if dl_cfg.path.starts_with('/') {
        dl_cfg.path.clone()
    } else {
        format!("/{}", dl_cfg.path)
    };
    let method_str = dl_cfg.method.as_deref().unwrap_or("post_form");
    let param = dl_cfg.id_param.as_deref().unwrap_or("id");

    let (method, body) = match method_str.to_lowercase().as_str() {
        "post_json" => (
            FetchMethod::PostJson,
            Some(serde_json::json!({ param: id }).to_string()),
        ),
        _ => (FetchMethod::PostForm, Some(format!("{param}={id}"))),
    };

    Some(FetchRequest {
        key: format!("download:{}:{id}", site.id),
        url: format!("{base}{path}"),
        method,
        body,
        cookie: site.cookie.clone(),
        api_key: site.api_key.clone(),
        proxy: site.proxy.clone(),
        cdp_url: None,
    })
}

pub fn mteam_download_request(site: &Site, id: &str) -> FetchRequest {
    FetchRequest {
        key: format!("download:{}:{id}", site.id),
        url: format!("{}/api/torrent/genDlToken", site.url.trim_end_matches('/')),
        method: FetchMethod::PostForm,
        body: Some(format!("id={id}")),
        cookie: None,
        api_key: site.api_key.clone(),
        proxy: site.proxy.clone(),
        cdp_url: None,
    }
}

fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push('+'),
            _ => {
                use std::fmt::Write;
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}
