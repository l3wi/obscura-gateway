use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use url::Url;

use crate::models::StoredCookie;

#[derive(Debug, Clone, Copy)]
pub enum CookieFormat {
    Auto,
    Netscape,
    Json,
}

pub fn parse_cookies(input: &str, format: CookieFormat) -> Result<Vec<StoredCookie>> {
    match format {
        CookieFormat::Netscape => parse_netscape(input),
        CookieFormat::Json => parse_json(input),
        CookieFormat::Auto => parse_json(input).or_else(|_| parse_netscape(input)),
    }
}

pub fn parse_json(input: &str) -> Result<Vec<StoredCookie>> {
    #[derive(Debug, Deserialize)]
    struct JsonCookie {
        name: String,
        value: String,
        domain: String,
        #[serde(default = "default_path")]
        path: String,
        #[serde(default)]
        secure: bool,
        #[serde(default, alias = "httpOnly")]
        http_only: bool,
        #[serde(default)]
        expires: Option<i64>,
    }

    fn default_path() -> String {
        "/".to_string()
    }

    let value: serde_json::Value = serde_json::from_str(input)?;
    let array = if value.is_array() {
        value
    } else if let Some(inner) = value.get("cookies") {
        inner.clone()
    } else {
        bail!("expected a JSON array or {{\"cookies\": [...] }}");
    };
    let cookies: Vec<JsonCookie> = serde_json::from_value(array)?;
    Ok(cookies
        .into_iter()
        .map(|c| StoredCookie {
            name: c.name,
            value: c.value,
            domain: c.domain,
            path: c.path,
            secure: c.secure,
            http_only: c.http_only,
            expires: c.expires,
        })
        .collect())
}

pub fn parse_netscape(input: &str) -> Result<Vec<StoredCookie>> {
    let mut out = Vec::new();
    for raw_line in input.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 7 {
            bail!("invalid netscape cookie line: {line}");
        }
        out.push(StoredCookie {
            domain: parts[0].to_string(),
            secure: parts[3].eq_ignore_ascii_case("TRUE"),
            path: parts[2].to_string(),
            expires: parts[4].parse::<i64>().ok().filter(|v| *v > 0),
            name: parts[5].to_string(),
            value: parts[6].to_string(),
            http_only: parts[1].eq_ignore_ascii_case("TRUE"),
        });
    }
    Ok(out)
}

pub fn export_json(cookies: &[StoredCookie]) -> Result<String> {
    Ok(serde_json::to_string_pretty(cookies)?)
}

pub fn export_netscape(cookies: &[StoredCookie]) -> String {
    let mut lines = vec!["# Netscape HTTP Cookie File".to_string()];
    for cookie in cookies {
        lines.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            cookie.domain,
            if cookie.http_only { "TRUE" } else { "FALSE" },
            cookie.path,
            if cookie.secure { "TRUE" } else { "FALSE" },
            cookie.expires.unwrap_or(0),
            cookie.name,
            cookie.value
        ));
    }
    lines.join("\n")
}

pub fn cookie_urls(cookies: &[StoredCookie]) -> Vec<String> {
    let mut urls: Vec<String> = cookies
        .iter()
        .filter_map(|cookie| {
            let host = cookie.domain.trim_start_matches('.');
            if host.is_empty() {
                return None;
            }
            let scheme = if cookie.secure { "https" } else { "http" };
            Url::parse(&format!("{scheme}://{host}"))
                .ok()
                .map(|url| url.to_string().trim_end_matches('/').to_string())
        })
        .collect();
    urls.sort();
    urls.dedup();
    urls
}

pub fn detect_format_from_name(file_name: Option<&str>) -> CookieFormat {
    match file_name.and_then(|name| name.rsplit('.').next()) {
        Some("json") => CookieFormat::Json,
        Some("txt") => CookieFormat::Netscape,
        _ => CookieFormat::Auto,
    }
}

pub fn validate_non_empty(cookies: &[StoredCookie]) -> Result<()> {
    if cookies.is_empty() {
        Err(anyhow!("no cookies found"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_netscape() {
        let raw = ".example.com\tFALSE\t/\tTRUE\t0\tsid\tabc";
        let parsed = parse_netscape(raw).unwrap();
        assert_eq!(parsed[0].name, "sid");
        assert_eq!(
            cookie_urls(&parsed),
            vec!["https://example.com".to_string()]
        );
    }

    #[test]
    fn parses_json() {
        let raw =
            r#"[{"name":"sid","value":"abc","domain":".example.com","path":"/","secure":true}]"#;
        let parsed = parse_json(raw).unwrap();
        assert_eq!(parsed[0].value, "abc");
    }
}
