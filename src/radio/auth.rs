//! Mutual proof of `ble.key` from `badge.toml`, so a host only drives its own badge and a badge
//! only answers its own host. Both sides prove they know the key with an HMAC over fresh random
//! nonces from each side, so a recorded handshake can't be replayed.
//!
//! ```text
//! host:  hello <host nonce>
//! badge: challenge <badge nonce> <HMAC(key, "badge" | host nonce | badge nonce)>
//! host:  auth <HMAC(key, "host" | badge nonce | host nonce)>
//! badge: auth ok
//! ```
//!
//! Nonces are 16 bytes and MACs 32, all as lowercase hex. This authenticates but doesn't
//! encrypt: someone listening nearby can still read the mirror stream.
use core::fmt::Write;

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub struct Auth {
    key: &'static [u8],
    badge_nonce: [u8; 16],
    host_nonce: Option<[u8; 16]>,
}

pub enum Step {
    /// Send this line to the host.
    Reply(heapless::String<128>),
    /// The host proved it knows the key.
    Accepted,
    Rejected,
    /// Not part of the handshake (a stray line from an earlier host, say): ignore it.
    Ignored,
}

impl Auth {
    /// `seed` is random per boot; `connection` makes each badge nonce different.
    pub fn new(key: &'static str, seed: &[u8; 32], connection: u32) -> Self {
        let digest = Sha256::new().chain_update(seed).chain_update(connection.to_le_bytes()).finalize();
        Self { key: key.as_bytes(), badge_nonce: digest[..16].try_into().unwrap(), host_nonce: None }
    }

    pub fn step(&mut self, line: &str) -> Step {
        match line.split_once(' ') {
            Some(("hello", hex)) => {
                let Some(host_nonce) = decode::<16>(hex) else { return Step::Rejected };
                self.host_nonce = Some(host_nonce);
                let proof = self.mac(b"badge", &host_nonce, &self.badge_nonce).finalize().into_bytes();
                let mut reply = heapless::String::new();
                let _ = write!(reply, "challenge {} {}\r\n", Hex(&self.badge_nonce), Hex(&proof));
                Step::Reply(reply)
            }
            Some(("auth", hex)) => {
                let (Some(host_nonce), Some(proof)) = (self.host_nonce.take(), decode::<32>(hex)) else {
                    return Step::Rejected;
                };
                // `verify_slice` compares in constant time.
                match self.mac(b"host", &self.badge_nonce, &host_nonce).verify_slice(&proof) {
                    Ok(()) => Step::Accepted,
                    Err(_) => Step::Rejected,
                }
            }
            _ => Step::Ignored,
        }
    }

    fn mac(&self, role: &[u8], first: &[u8; 16], second: &[u8; 16]) -> HmacSha256 {
        let mut mac = HmacSha256::new_from_slice(self.key).unwrap();
        mac.update(role);
        mac.update(first);
        mac.update(second);
        mac
    }
}

struct Hex<'a>(&'a [u8]);

impl core::fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

fn decode<const N: usize>(hex: &str) -> Option<[u8; N]> {
    let hex = hex.trim().as_bytes();
    if hex.len() != N * 2 {
        return None;
    }
    let mut out = [0; N];
    for (byte, pair) in out.iter_mut().zip(hex.chunks(2)) {
        *byte = u8::from_str_radix(core::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}
