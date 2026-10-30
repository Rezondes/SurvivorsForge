//! Who sent an anonymous count, without storing their IP: a keyed hash whose key changes every
//! day and lives only in this process. After the day nobody can tell two days' senders apart.

use chrono::NaiveDate;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::net::IpAddr;
use std::sync::Mutex;

// ponytail: per process; after a restart or with several API instances one visitor can count twice a day.
#[derive(Default)]
pub struct SenderKey {
    today: Mutex<Option<(NaiveDate, [u8; 32])>>,
}

impl SenderKey {
    /// The sender's hash for `day`; the key is drawn on the day's first call.
    pub fn hash(&self, day: NaiveDate, ip: IpAddr) -> Vec<u8> {
        let key = {
            let mut today = self.today.lock().unwrap_or_else(|e| e.into_inner());
            match *today {
                Some((d, key)) if d == day => key,
                _ => {
                    let key = rand::random::<[u8; 32]>();
                    *today = Some((day, key));
                    key
                }
            }
        };
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).expect("HMAC takes any key length");
        mac.update(ip.to_string().as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_day_same_hash_next_day_new() {
        let key = SenderKey::default();
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 10, 31).unwrap();
        assert_eq!(key.hash(day, ip), key.hash(day, ip));
        assert_ne!(key.hash(day, ip), key.hash(day, "203.0.113.8".parse().unwrap()));
        let first = key.hash(day, ip);
        assert_ne!(key.hash(day.succ_opt().unwrap(), ip), first, "a new key every day");
    }
}
