//! 从环境变量加载应用配置。

use std::net::IpAddr;
use std::path::PathBuf;

/// 管理面板与中继引擎的运行时配置。
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub panel_port: u16,
    pub data_dir: PathBuf,
    pub auth_username: String,
    pub auth_password: String,
    pub jwt_secret: String,
    pub jwt_expire_hours: i64,
    pub default_start_port: u16,
    pub listen_host: IpAddr,
}

impl AppConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let data_dir = std::env::var("DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./data"));

        let data_dir = if data_dir.is_absolute() {
            data_dir
        } else {
            std::env::current_dir()?.join(data_dir)
        };

        std::fs::create_dir_all(&data_dir)?;
        let data_dir = std::fs::canonicalize(&data_dir).unwrap_or(data_dir);

        let listen_host: IpAddr = std::env::var("LISTEN_HOST")
            .unwrap_or_else(|_| "0.0.0.0".into())
            .parse()?;

        let (auth_username, auth_password) = resolve_auth_credentials();
        let jwt_secret = resolve_jwt_secret();

        Ok(Self {
            panel_port: env_u16("PANEL_PORT", 888),
            data_dir,
            auth_username,
            auth_password,
            jwt_secret,
            jwt_expire_hours: env_i64("JWT_EXPIRE_HOURS", 5),
            default_start_port: env_u16("DEFAULT_START_PORT", 1000),
            listen_host,
        })
    }
}

/// JWT 密钥仅驻留内存；未设置时每次启动随机生成。
fn resolve_jwt_secret() -> String {
    std::env::var("JWT_SECRET")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

/// 用户名与密码需同时通过环境变量指定；否则随机生成并打印到终端。
fn resolve_auth_credentials() -> (String, String) {
    let user_set = env_opt("AUTH_USERNAME");
    let pass_set = env_opt("AUTH_PASSWORD");

    if let (Some(username), Some(password)) = (user_set, pass_set) {
        return (username, password);
    }

    let username = random_alphanumeric(8);
    let password = random_alphanumeric(16);

    eprintln!();
    eprintln!("========================================");
    eprintln!("  管理面板初始账号（请妥善保存）");
    eprintln!("  用户名: {username}");
    eprintln!("  密码:   {password}");
    eprintln!("========================================");
    eprintln!();

    (username, password)
}

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn random_alphanumeric(len: usize) -> String {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(len);
    while out.len() < len {
        for byte in uuid::Uuid::new_v4().into_bytes() {
            if out.len() >= len {
                break;
            }
            out.push(CHARSET[byte as usize % CHARSET.len()] as char);
        }
    }
    out
}

fn env_u16(key: &str, default: u16) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_alphanumeric_format() {
        let s = random_alphanumeric(32);
        assert_eq!(s.len(), 32);
        assert!(
            s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
    }
}
