//! macOS 本地凭据保险库。
//!
//! 为什么不用钥匙串：IslandHop 是自签名（adhoc / TeamIdentifier 未设置），
//! macOS 不允许为未受信任签名的 App 记住钥匙串授权——授权框「始终允许」是灰的，
//! 每次更新 / 重启都会重新弹「IslandHop 想访问 Daozai.Mail / Daozai.Calendar…」。
//! 改成本机 0600 文件 + AES-256-GCM，安全等级等价（同用户进程本就能读钥匙串），
//! 且彻底不再触发系统授权 UI。Windows 仍走凭据管理器（DPAPI），不受影响。

use std::path::{Path, PathBuf};

const MAGIC: &[u8; 7] = b"ISLSEC1";

fn vault_dir() -> PathBuf {
    crate::model::config_dir().join("credentials")
}

fn key_path() -> PathBuf {
    vault_dir().join("session.key")
}

/// 读取（或首次生成）32 字节主密钥，0600 权限落盘。
/// 与 gemini_quota 的 mac_session_key 共用同一文件，一次生成全端复用。
fn session_key_at(dir: &Path) -> Result<[u8; 32], String> {
    use std::os::unix::fs::PermissionsExt;
    use ring::rand::SecureRandom;
    let path = dir.join("session.key");
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() != 32 {
            return Err("本机会话密钥文件损坏，请删除后重试".into());
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }
    let mut key = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| "无法生成会话密钥".to_string())?;
    std::fs::create_dir_all(dir).map_err(|_| "无法创建密钥目录".to_string())?;
    std::fs::write(&path, key).map_err(|_| "无法保存会话密钥".to_string())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| "无法设置会话密钥权限".to_string())?;
    Ok(key)
}

fn session_key() -> Result<[u8; 32], String> {
    session_key_at(&vault_dir())
}

/// 文件名把 service + slot 编成十六进制，避免邮箱/特殊字符落到路径里。
fn entry_path_at(dir: &Path, service: &str, slot: &str) -> PathBuf {
    let mut raw = Vec::new();
    raw.extend_from_slice(service.as_bytes());
    raw.push(0);
    raw.extend_from_slice(slot.as_bytes());
    let mut hex = String::with_capacity(raw.len() * 2);
    for byte in raw {
        hex.push_str(&format!("{byte:02x}"));
    }
    dir.join(format!("secret-{hex}.bin"))
}

fn entry_path(service: &str, slot: &str) -> PathBuf {
    entry_path_at(&vault_dir(), service, slot)
}

fn encrypt(key: &[u8; 32], plain: &[u8]) -> Result<Vec<u8>, String> {
    use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
    use ring::rand::SecureRandom;
    let unbound =
        UnboundKey::new(&AES_256_GCM, key).map_err(|_| "本机凭据密钥无效".to_string())?;
    let mut nonce = [0u8; 12];
    ring::rand::SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| "无法加密凭据".to_string())?;
    let mut data = plain.to_vec();
    LessSafeKey::new(unbound)
        .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut data)
        .map_err(|_| "无法加密凭据".to_string())?;
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&data);
    Ok(out)
}

fn decrypt(key: &[u8; 32], raw: &[u8]) -> Result<Vec<u8>, String> {
    use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
    if raw.len() < MAGIC.len() + 12 + 16 || !raw.starts_with(MAGIC) {
        return Err("本机凭据格式无效，请重新输入".into());
    }
    let unbound =
        UnboundKey::new(&AES_256_GCM, key).map_err(|_| "本机凭据密钥无效".to_string())?;
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&raw[MAGIC.len()..MAGIC.len() + 12]);
    let mut data = raw[MAGIC.len() + 12..].to_vec();
    let plain = LessSafeKey::new(unbound)
        .open_in_place(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut data)
        .map_err(|_| "本机凭据密钥已更新（不再使用钥匙串），请重新输入一次".to_string())?;
    Ok(plain.to_vec())
}

fn write_0600(path: &Path, body: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| "无法创建凭据目录".to_string())?;
    }
    std::fs::write(path, body).map_err(|_| "无法保存凭据".to_string())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| "无法设置凭据权限".to_string())
}

pub fn save(service: &str, slot: &str, value: &str) -> Result<(), String> {
    save_at(&vault_dir(), service, slot, value)
}

fn save_at(dir: &Path, service: &str, slot: &str, value: &str) -> Result<(), String> {
    let key = session_key_at(dir)?;
    let sealed = encrypt(&key, value.as_bytes())?;
    write_0600(&entry_path_at(dir, service, slot), &sealed)
}

pub fn load(service: &str, slot: &str) -> Result<String, String> {
    load_at(&vault_dir(), service, slot)
}

fn load_at(dir: &Path, service: &str, slot: &str) -> Result<String, String> {
    let raw = std::fs::read(entry_path_at(dir, service, slot)).map_err(|_| {
        "本机凭据存储已从钥匙串切换到加密文件，请重新输入一次授权码/密码".to_string()
    })?;
    let key = session_key_at(dir)?;
    let plain = decrypt(&key, &raw)?;
    String::from_utf8(plain).map_err(|_| "本机凭据内容无效，请重新输入".into())
}

pub fn delete(service: &str, slot: &str) {
    let _ = std::fs::remove_file(entry_path(service, slot));
}

#[cfg(test)]
fn delete_at(dir: &Path, service: &str, slot: &str) {
    let _ = std::fs::remove_file(entry_path_at(dir, service, slot));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_secret_encrypt_decrypt_roundtrip() {
        let key = [7u8; 32];
        let sealed = encrypt(&key, b"app-password-2026").unwrap();
        assert!(!sealed.windows(11).any(|w| w == b"app-password"));
        assert_eq!(decrypt(&key, &sealed).unwrap(), b"app-password-2026");
    }

    #[test]
    fn local_secret_tampered_ciphertext_is_rejected() {
        let key = [9u8; 32];
        let mut sealed = encrypt(&key, b"secret").unwrap();
        sealed[MAGIC.len() + 12] ^= 1;
        assert!(decrypt(&key, &sealed).is_err());
    }

    #[test]
    fn local_secret_wrong_key_is_rejected() {
        let sealed = encrypt(&[1u8; 32], b"secret").unwrap();
        assert!(decrypt(&[2u8; 32], &sealed).is_err());
    }

    #[test]
    fn local_secret_file_roundtrip_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("islsec-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let service = "Daozai.Mail";
        let slot = "user@example.com";
        save_at(&dir, service, slot, "app-password-2026").unwrap();
        assert_eq!(load_at(&dir, service, slot).unwrap(), "app-password-2026");

        // 主密钥文件与凭据文件都必须是 0600。
        let key_mode = std::fs::metadata(dir.join("session.key")).unwrap().permissions().mode() & 0o777;
        assert_eq!(key_mode, 0o600, "主密钥文件权限应为 0600");
        let entry = entry_path_at(&dir, service, slot);
        let entry_mode = std::fs::metadata(&entry).unwrap().permissions().mode() & 0o777;
        assert_eq!(entry_mode, 0o600, "凭据文件权限应为 0600");

        // 密文文件里不能出现明文。
        let raw = std::fs::read(&entry).unwrap();
        assert!(!raw.windows(11).any(|w| w == b"app-password"));

        // 幂等：再次写入同键覆盖、读回最新值。
        save_at(&dir, service, slot, "new-password").unwrap();
        assert_eq!(load_at(&dir, service, slot).unwrap(), "new-password");

        delete_at(&dir, service, slot);
        assert!(load_at(&dir, service, slot).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
