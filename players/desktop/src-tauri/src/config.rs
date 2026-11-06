//! Settings and state files, kept in one folder per Windows user:
//! `%LOCALAPPDATA%\Qivomedia\Player` (or `~/.config/qivomedia-player` elsewhere).
//! `QIVO_PLAYER_HOME` overrides the folder (tests, multiple instances).

use std::{
    fs,
    io::Write,
    path::PathBuf,
    time::SystemTime,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The Qivomedia server, e.g. `https://signage.univ.ma`. `None` = show the setup screen.
    pub server_url: Option<String>,
    /// Local time for a daily player restart ("04:00"), which keeps long-running screens fresh. Empty = never.
    pub restart_at: String,
    /// Start the player when this Windows user signs in.
    pub autostart: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { server_url: None, restart_at: "04:00".into(), autostart: true }
    }
}

pub fn home() -> PathBuf {
    if let Ok(p) = std::env::var("QIVO_PLAYER_HOME") {
        return PathBuf::from(p);
    }
    #[cfg(windows)]
    let base = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("C:\\ProgramData")).join("Qivomedia").join("Player");
    #[cfg(not(windows))]
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config"))
        .join("qivomedia-player");
    base
}

fn file(name: &str) -> PathBuf {
    let h = home();
    let _ = fs::create_dir_all(&h);
    h.join(name)
}

/// Machine-wide defaults written by IT (kiosk script, Intune, GPO): used until this
/// Windows user has saved settings of their own.
/// `%ProgramData%\Qivomedia\Player\defaults.json` (`QIVO_PLAYER_DEFAULTS` overrides).
pub fn machine_defaults_path() -> PathBuf {
    if let Ok(p) = std::env::var("QIVO_PLAYER_DEFAULTS") {
        return PathBuf::from(p);
    }
    #[cfg(windows)]
    return PathBuf::from(std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".into())).join("Qivomedia").join("Player").join("defaults.json");
    #[cfg(not(windows))]
    PathBuf::from("/etc/qivomedia-player/defaults.json")
}

impl Config {
    pub fn load() -> Self {
        let read = |p: PathBuf| fs::read_to_string(p).ok().and_then(|s| serde_json::from_str::<Config>(&s).ok());
        read(file("config.json")).or_else(|| read(machine_defaults_path())).unwrap_or_default()
    }
    pub fn save(&self) -> std::io::Result<()> {
        // Write then rename, so a power cut never leaves a half-written file.
        let tmp = file("config.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(tmp, file("config.json"))
    }
}

/// `https://Signage.Univ.ma/player/?x` → `https://signage.univ.ma` (scheme, host and port only).
pub fn normalize_server(input: &str) -> Result<String, String> {
    let s = input.trim();
    let s = if s.contains("://") { s.to_owned() } else { format!("https://{s}") };
    let u = url::Url::parse(&s).map_err(|_| "That doesn't look like a web address".to_string())?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err("Use an http:// or https:// address".into());
    }
    Ok(u.origin().ascii_serialization())
}

/// Addresses to try for what the technician typed: with no scheme, HTTPS first, then HTTP
/// (campus servers on the local network are often plain HTTP).
pub fn server_candidates(input: &str) -> Result<Vec<String>, String> {
    if input.contains("://") {
        return Ok(vec![normalize_server(input)?]);
    }
    let https = normalize_server(input)?;
    Ok(vec![https.clone(), https.replacen("https://", "http://", 1)])
}

// ---- small state files shared by the supervisor and the player ----

pub fn touch_heartbeat() {
    let _ = fs::write(file("heartbeat"), chrono::Local::now().to_rfc3339());
}

/// Seconds since the player last said it was alive (`None` = never).
pub fn heartbeat_age() -> Option<u64> {
    let m = fs::metadata(file("heartbeat")).ok()?.modified().ok()?;
    SystemTime::now().duration_since(m).ok().map(|d| d.as_secs())
}

pub fn clear_heartbeat() {
    let _ = fs::remove_file(file("heartbeat"));
}

/// The server's player has loaded at least once, so its offline cache exists and it
/// can start without a network.
pub fn mark_loaded(origin: &str) {
    let _ = fs::write(file("loaded-from"), origin);
}
pub fn loaded_before(origin: &str) -> bool {
    fs::read_to_string(file("loaded-from")).map(|s| s.trim() == origin).unwrap_or(false)
}

/// Append to `player.log`; keeps one previous file of about 1 MB.
pub fn log(msg: &str) {
    let path = file("player.log");
    if fs::metadata(&path).map(|m| m.len() > 1_000_000).unwrap_or(false) {
        let _ = fs::rename(&path, file("player.log.1"));
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_addresses_are_normalized() {
        assert_eq!(normalize_server("signage.univ.ma").unwrap(), "https://signage.univ.ma");
        assert_eq!(normalize_server(" https://Signage.Univ.ma/player/?x=1 ").unwrap(), "https://signage.univ.ma");
        assert_eq!(normalize_server("http://10.0.0.5:8080/").unwrap(), "http://10.0.0.5:8080");
        assert!(normalize_server("ftp://x.ma").is_err());
        assert!(normalize_server("javascript:alert(1)").is_err());
        assert_eq!(server_candidates("10.0.0.5:8080").unwrap(), vec!["https://10.0.0.5:8080", "http://10.0.0.5:8080"]);
        assert_eq!(server_candidates("http://10.0.0.5").unwrap(), vec!["http://10.0.0.5"]);
    }
}
