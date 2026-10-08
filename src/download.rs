use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::header::CONTENT_DISPOSITION;
use reqwest::Url;

use crate::doc::FileRef;

/// Longest file name we write, in bytes (Linux allows 255).
const MAX_NAME_BYTES: usize = 200;

/// HTTP client for Ed's file CDN. No token is sent: edusercontent.com URLs are
/// pre-signed, and redirects may only stay on that host.
pub fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(600))
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if is_ed_hosted(attempt.url()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .user_agent(concat!("edstem-tui/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// Only HTTPS files on Ed's own CDN are downloaded; anything else is just shown.
pub fn is_downloadable(url: &str) -> bool {
    Url::parse(url).map(|u| is_ed_hosted(&u)).unwrap_or(false)
}

fn is_ed_hosted(url: &Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| host == "edusercontent.com" || host.ends_with(".edusercontent.com"))
}

/// Downloads one file into `dest_dir` and returns the path it was saved to.
/// The body goes to a hidden `.part` file first so a failed download never
/// leaves a truncated file behind.
pub fn download(http: &Client, file: &FileRef, dest_dir: &Path) -> Result<PathBuf> {
    if !is_downloadable(&file.url) {
        bail!("not an Ed-hosted file: {}", file.url);
    }
    let mut resp = http.get(&file.url).send().context("request failed")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("HTTP {}", status.as_u16());
    }

    let header_name = resp
        .headers()
        .get(CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .and_then(filename_from_content_disposition)
        .unwrap_or_default();
    let name = [header_name, file.name.clone(), filename_from_url(&file.url)]
        .iter()
        .map(|candidate| sanitize(candidate))
        .find(|candidate| !candidate.is_empty())
        .unwrap_or_else(|| "download".to_string());

    fs::create_dir_all(dest_dir)
        .with_context(|| format!("cannot create {}", dest_dir.display()))?;
    let part = dest_dir.join(format!(".{name}.part"));
    let saved = (|| -> Result<PathBuf> {
        let mut out = File::create(&part)?;
        io::copy(&mut resp, &mut out)?;
        out.sync_all()?;
        let target = unique_path(&dest_dir.join(&name));
        fs::rename(&part, &target)?;
        Ok(target)
    })();
    if saved.is_err() {
        let _ = fs::remove_file(&part);
    }
    saved
}

/// Makes a single safe path component: no separators or control characters,
/// no leading dots, and short enough for the filesystem (extension kept).
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    if cleaned.len() <= MAX_NAME_BYTES {
        return cleaned.to_string();
    }

    let (stem, ext) = match cleaned.rfind('.') {
        Some(dot) if cleaned.len() - dot <= 10 => cleaned.split_at(dot),
        _ => (cleaned, ""),
    };
    let mut cut = MAX_NAME_BYTES - ext.len();
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{}", stem[..cut].trim_end(), ext)
}

/// Appends " (n)" before the extension when the target already exists.
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|candidate| !candidate.exists())
        .expect("unbounded range always yields a free name")
}

pub fn filename_from_content_disposition(value: &str) -> Option<String> {
    let parts: Vec<&str> = value.split(';').map(str::trim).collect();
    for part in &parts {
        let lower = part.to_ascii_lowercase();
        if lower.starts_with("filename*") {
            let raw = part.split_once('=')?.1.trim().trim_matches('"');
            let encoded = match raw.find("''") {
                Some(pos) => &raw[pos + 2..],
                None => raw,
            };
            return Some(percent_decode(encoded));
        }
    }
    for part in &parts {
        if part.to_ascii_lowercase().starts_with("filename=") {
            let raw = part.split_once('=')?.1.trim();
            let unquoted = raw
                .strip_prefix('"')
                .and_then(|r| r.strip_suffix('"'))
                .map(|r| r.replace("\\\"", "\"").replace("\\\\", "\\"))
                .unwrap_or_else(|| raw.to_string());
            return Some(unquoted);
        }
    }
    None
}

pub fn filename_from_url(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(|mut segments| segments.next_back().map(percent_decode))
        })
        .unwrap_or_default()
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_separators_and_leading_dots() {
        assert_eq!(sanitize("../a/b\\c.pdf"), "_a_b_c.pdf");
        assert_eq!(sanitize("  .hidden "), "hidden");
        assert_eq!(sanitize("tab\there"), "tab_here");
    }

    #[test]
    fn sanitize_truncates_long_names_but_keeps_extension() {
        let long = format!("{}.pdf", "講義".repeat(100));
        let cut = sanitize(&long);
        assert!(cut.len() <= MAX_NAME_BYTES);
        assert!(cut.ends_with(".pdf"));
    }

    #[test]
    fn content_disposition_variants() {
        assert_eq!(
            filename_from_content_disposition("attachment; filename=\"Week 1.pdf\"").as_deref(),
            Some("Week 1.pdf")
        );
        assert_eq!(
            filename_from_content_disposition(
                "attachment; filename=\"fallback.pdf\"; filename*=UTF-8''%E8%AC%9B%E7%BE%A9.pdf"
            )
            .as_deref(),
            Some("講義.pdf")
        );
        assert_eq!(
            filename_from_content_disposition("inline; filename=notes.txt").as_deref(),
            Some("notes.txt")
        );
        assert_eq!(filename_from_content_disposition("inline"), None);
    }

    #[test]
    fn url_filename_is_decoded() {
        assert_eq!(
            filename_from_url("https://static.us.edusercontent.com/files/abc/Lab%201.pdf"),
            "Lab 1.pdf"
        );
    }

    #[test]
    fn only_ed_cdn_is_downloadable() {
        assert!(is_downloadable("https://static.us.edusercontent.com/files/x"));
        assert!(is_downloadable("https://edusercontent.com/x"));
        assert!(!is_downloadable("http://static.us.edusercontent.com/files/x"));
        assert!(!is_downloadable("https://evil-edusercontent.com/x"));
        assert!(!is_downloadable("https://example.com/x.pdf"));
    }

    #[test]
    fn unique_path_appends_counter() {
        let dir = std::env::temp_dir().join(format!("edstem-tui-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let first = dir.join("a.pdf");
        assert_eq!(unique_path(&first), first);
        fs::write(&first, b"x").unwrap();
        assert_eq!(unique_path(&first), dir.join("a (1).pdf"));
        fs::write(dir.join("a (1).pdf"), b"x").unwrap();
        assert_eq!(unique_path(&first), dir.join("a (2).pdf"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
