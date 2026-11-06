use rand::{distributions::Alphanumeric, Rng};
use sha2::{Digest, Sha256};

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn random_token(len: usize) -> String {
    rand::thread_rng().sample_iter(&Alphanumeric).take(len).map(char::from).collect()
}

/// Human-friendly pairing code without ambiguous characters (no 0/O/1/I).
pub fn pairing_code() -> String {
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    (0..6).map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char).collect()
}

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

pub fn valid_hhmm(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && s[..2].parse::<u8>().map(|h| h < 24).unwrap_or(false)
        && s[3..].parse::<u8>().map(|m| m < 60).unwrap_or(false)
}

pub fn valid_local_dt(s: &str) -> bool {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn time_validation() {
        assert!(valid_hhmm("09:30"));
        assert!(valid_hhmm("23:59"));
        assert!(!valid_hhmm("24:00"));
        assert!(!valid_hhmm("9:30"));
        assert!(valid_local_dt("2026-11-06T08:00"));
        assert!(!valid_local_dt("2026-13-06T08:00"));
    }
    #[test]
    fn codes() {
        let c = pairing_code();
        assert_eq!(c.len(), 6);
        assert!(!c.contains('0') && !c.contains('O'));
    }
}
