//! TOTP（RFC 6238）最小实现：给本地账号加两步验证。
//!
//! - 密钥 20 字节，base32（无填充）存库；`otpauth://` URI 可直接粘进
//!   Google Authenticator / 1Password / Authy。
//! - 只依赖 `hmac` + `sha1`，不引 QR 库：UI 展示密钥 + URI，用户手输或粘贴。
//! - 校验窗口 ±1 个 step（±30s），容忍手机时钟偏差。
use hmac::{Hmac, Mac};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
pub const STEP_SECS: u64 = 30;
pub const DIGITS: u32 = 6;

/// 生成新密钥（20 字节随机 → base32）。
pub fn generate_secret() -> String {
    // 用两个 UUIDv4 拼出 32 字节随机（v4 有 122 bit 熵/个，够 160 bit 密钥）
    let mut raw = Vec::with_capacity(32);
    raw.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    raw.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    base32_encode(&raw[..20])
}

pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for b in data {
        buffer = (buffer << 8) | u32::from(*b);
        bits += 8;
        while bits >= 5 {
            let idx = ((buffer >> (bits - 5)) & 0x1f) as usize;
            out.push(ALPHABET[idx] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        let idx = ((buffer << (5 - bits)) & 0x1f) as usize;
        out.push(ALPHABET[idx] as char);
    }
    out
}

pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let clean: Vec<u8> = s
        .bytes()
        .filter(|b| !matches!(b, b' ' | b'-' | b'='))
        .map(|b| b.to_ascii_uppercase())
        .collect();
    let mut out = Vec::new();
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for c in clean {
        let v = ALPHABET.iter().position(|x| *x == c)? as u32;
        buffer = (buffer << 5) | v;
        bits += 5;
        if bits >= 8 {
            out.push(((buffer >> (bits - 8)) & 0xff) as u8);
            bits -= 8;
        }
    }
    Some(out)
}

/// 计算某个时间片的一次性验证码（补零到 6 位）。
pub fn code_at(secret: &str, unix_secs: u64) -> Option<String> {
    let key = base32_decode(secret)?;
    if key.is_empty() {
        return None;
    }
    let counter = unix_secs / STEP_SECS;
    let mut mac = HmacSha1::new_from_slice(&key).ok()?;
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[19] & 0x0f) as usize;
    let bin = ((u32::from(digest[offset]) & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);
    let code = bin % 10u32.pow(DIGITS);
    Some(format!("{code:0width$}", width = DIGITS as usize))
}

/// 校验用户输入的验证码（窗口 ±1 step）。
pub fn verify(secret: &str, code: &str) -> bool {
    let code = code.trim().replace(' ', "");
    if code.len() != DIGITS as usize || !code.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    [now.saturating_sub(STEP_SECS), now, now + STEP_SECS]
        .iter()
        .filter_map(|t| code_at(secret, *t))
        .any(|c| constant_time_eq(c.as_bytes(), code.as_bytes()))
}

/// `otpauth://` URI：手机端扫码/粘贴都能用。
pub fn otpauth_uri(issuer: &str, account: &str, secret: &str) -> String {
    // 只做最小转义（账号里可能有 @）
    let account = account.replace('@', "%40").replace(' ', "%20");
    let issuer_enc = issuer.replace(' ', "%20");
    format!(
        "otpauth://totp/{issuer_enc}:{account}?secret={secret}&issuer={issuer_enc}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECS}"
    )
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_roundtrip() {
        let raw = b"hello-totp-secret-20";
        let enc = base32_encode(raw);
        assert_eq!(base32_decode(&enc).unwrap(), raw.to_vec());
        // 带空格/小写/填充也应能解
        assert_eq!(
            base32_decode(&format!(" {} ", enc.to_lowercase())).unwrap(),
            raw.to_vec()
        );
    }

    /// RFC 6238 的 SHA1 测试向量（8 位码截前 6 位）：
    /// secret "12345678901234567890"（ASCII），T=59s → 94287082 → 取 287082。
    #[test]
    fn rfc6238_vector_matches() {
        let secret = base32_encode(b"12345678901234567890");
        assert_eq!(code_at(&secret, 59).unwrap(), "287082");
        assert_eq!(code_at(&secret, 1111111109).unwrap(), "081804");
    }

    #[test]
    fn verify_accepts_current_code_and_rejects_garbage() {
        let secret = generate_secret();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let code = code_at(&secret, now).unwrap();
        assert!(verify(&secret, &code));
        assert!(!verify(&secret, "000000"));
        assert!(!verify(&secret, "abc"));
    }

    #[test]
    fn uri_has_required_params() {
        let uri = otpauth_uri("FlinkOps", "a@b.com", "ABCDEF");
        assert!(uri.starts_with("otpauth://totp/FlinkOps:a%40b.com?secret=ABCDEF"));
        assert!(uri.contains("issuer=FlinkOps"));
        assert!(uri.contains("period=30"));
    }
}
