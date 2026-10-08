use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Us,
    Au,
}

impl Region {
    pub const ALL: [Region; 2] = [Region::Us, Region::Au];

    pub fn parse(raw: &str) -> Option<Region> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "us" => Some(Region::Us),
            "au" => Some(Region::Au),
            _ => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Region::Us => "us",
            Region::Au => "au",
        }
    }

    pub fn api_base(self) -> &'static str {
        match self {
            Region::Us => "https://us.edstem.org/api/",
            Region::Au => "https://edstem.org/api/",
        }
    }

    /// Base of the web UI, used for "open in browser" links.
    pub fn web_base(self) -> String {
        format!("https://edstem.org/{}", self.code())
    }

    pub fn token_url(self) -> String {
        format!("https://edstem.org/{}/settings/api-tokens", self.code())
    }
}

#[derive(Debug, Default)]
pub struct Options {
    pub region: Option<Region>,
    pub download_dir: Option<PathBuf>,
    pub show_help: bool,
    pub show_version: bool,
}

pub fn parse_args(args: &[String]) -> Result<Options> {
    let mut options = Options::default();
    let mut idx = 0;
    while idx < args.len() {
        let arg = args[idx].as_str();
        match arg {
            "-h" | "--help" => options.show_help = true,
            "--version" => options.show_version = true,
            _ if arg == "--region" || arg.starts_with("--region=") => {
                let (raw, next) = option_value(args, idx, "--region")?;
                options.region = match Region::parse(&raw) {
                    Some(region) => Some(region),
                    None => bail!("invalid region {raw:?}: expected us or au"),
                };
                idx = next;
            }
            _ if arg == "--download-dir" || arg.starts_with("--download-dir=") => {
                let (raw, next) = option_value(args, idx, "--download-dir")?;
                if raw.trim().is_empty() {
                    bail!("invalid download directory {raw:?}");
                }
                options.download_dir = Some(expand_tilde(raw.trim()));
                idx = next;
            }
            _ => bail!("unknown argument {arg:?}"),
        }
        idx += 1;
    }
    Ok(options)
}

fn option_value(args: &[String], idx: usize, flag: &str) -> Result<(String, usize)> {
    let arg = &args[idx];
    if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
        return Ok((value.to_string(), idx));
    }
    match args.get(idx + 1) {
        Some(value) => Ok((value.clone(), idx + 1)),
        None => bail!("missing value for {flag}"),
    }
}

pub fn usage() -> String {
    format!(
        "Usage:
  edstem-tui [options]

Browse Ed Lessons and download their files in the terminal.

Options:
  -h, --help              Show this help text
      --version           Show version information
      --region <us|au>    Ed region (default: auto-detect, remembered in {region})
      --download-dir <dir>
                          Where downloads are saved (default {downloads})

Environment:
  EDSTEM_TOKEN            Ed API token (otherwise read from {token})
  EDSTEM_BASE_URL         Override the API base URL (e.g. https://us.edstem.org/api/)
",
        region = tilde(&config_dir().join("region")),
        downloads = tilde(&default_download_dir()),
        token = tilde(&config_dir().join("token")),
    )
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home().join(".config"))
        .join("edstem-tui")
}

/// Token lookup order: EDSTEM_TOKEN, our token file, then edstem-cli's token file.
pub fn load_token() -> Option<String> {
    if let Ok(token) = env::var("EDSTEM_TOKEN") {
        if !token.trim().is_empty() {
            return Some(token.trim().to_string());
        }
    }
    let shared = config_dir()
        .parent()
        .map(|dir| dir.join("edstem-cli").join("token"));
    [Some(config_dir().join("token")), shared]
        .into_iter()
        .flatten()
        .filter_map(|path| fs::read_to_string(path).ok())
        .map(|token| token.trim().to_string())
        .find(|token| !token.is_empty())
}

pub fn saved_region() -> Option<Region> {
    fs::read_to_string(config_dir().join("region"))
        .ok()
        .and_then(|raw| Region::parse(&raw))
}

pub fn save_region(region: Region) {
    let dir = config_dir();
    if fs::create_dir_all(&dir).is_ok() {
        let _ = fs::write(dir.join("region"), format!("{}\n", region.code()));
    }
}

pub fn default_download_dir() -> PathBuf {
    home().join("ssd").join("tui").join("edstem-tui").join("downloads")
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn expand_tilde(raw: &str) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if raw == "~" => home(),
        None => PathBuf::from(raw),
    }
}

/// Shortens paths under $HOME to ~/... for display.
pub fn tilde(path: &std::path::Path) -> String {
    match path.strip_prefix(home()) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_flags_in_both_forms() {
        let options = parse_args(&args(&["--region", "au", "--download-dir=/tmp/x"])).unwrap();
        assert_eq!(options.region, Some(Region::Au));
        assert_eq!(options.download_dir, Some(PathBuf::from("/tmp/x")));
    }

    #[test]
    fn rejects_unknown_region_and_arguments() {
        assert!(parse_args(&args(&["--region", "eu"])).is_err());
        assert!(parse_args(&args(&["--nope"])).is_err());
        assert!(parse_args(&args(&["--region"])).is_err());
    }
}
