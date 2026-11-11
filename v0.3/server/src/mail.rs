//! メール送信（SMTP）。設定は管理画面から変更し data/mail.json に保存する
//!
//! 自宅回線などでは 25 番ポートからの直接送信がブロックされる（OP25B）ため、
//! プロバイダや Gmail・Microsoft 365 などの SMTP サーバ（587 / 465）を経由して送る。

use lettre::message::header::ContentType;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MailSettings {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    /// starttls（587）/ tls（465）/ none（暗号化なし。LAN 内の中継サーバ向け）
    pub security: String,
    pub username: String,
    pub password: String,
    pub from_address: String,
    pub from_name: String,
}

impl Default for MailSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 587,
            security: "starttls".into(),
            username: String::new(),
            password: String::new(),
            from_address: String::new(),
            from_name: "Macross".into(),
        }
    }
}

/// 管理画面に返す設定（パスワードは返さない）
#[derive(Serialize)]
pub struct MailView {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub security: String,
    pub username: String,
    pub has_password: bool,
    pub from_address: String,
    pub from_name: String,
}

/// 管理画面からの更新。password が None（または空）なら保存済みのものを使う
#[derive(Deserialize)]
pub struct MailUpdate {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub security: String,
    pub username: String,
    pub password: Option<String>,
    pub from_address: String,
    pub from_name: String,
}

pub struct Mailer {
    path: PathBuf,
    settings: Mutex<MailSettings>,
}

impl Mailer {
    pub fn open(path: PathBuf) -> Self {
        let settings = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self { path, settings: Mutex::new(settings) }
    }

    pub fn enabled(&self) -> bool {
        self.settings.lock().unwrap().enabled
    }

    pub fn view(&self) -> MailView {
        let s = self.settings.lock().unwrap();
        MailView {
            enabled: s.enabled,
            host: s.host.clone(),
            port: s.port,
            security: s.security.clone(),
            username: s.username.clone(),
            has_password: !s.password.is_empty(),
            from_address: s.from_address.clone(),
            from_name: s.from_name.clone(),
        }
    }

    pub fn update(&self, u: MailUpdate) -> anyhow::Result<()> {
        anyhow::ensure!(["starttls", "tls", "none"].contains(&u.security.as_str()), "暗号化方式が不正です");
        if u.enabled {
            anyhow::ensure!(!u.host.trim().is_empty(), "SMTP サーバを入力してください");
            anyhow::ensure!(u.port > 0, "ポート番号を入力してください");
            u.from_address.trim().parse::<lettre::Address>().map_err(|_| anyhow::anyhow!("送信元アドレスが不正です"))?;
        }
        let mut s = self.settings.lock().unwrap();
        let password = match u.password {
            Some(p) if !p.is_empty() => p,
            _ => s.password.clone(),
        };
        let new = MailSettings {
            enabled: u.enabled,
            host: u.host.trim().to_string(),
            port: u.port,
            security: u.security,
            username: u.username.trim().to_string(),
            password,
            from_address: u.from_address.trim().to_string(),
            from_name: u.from_name.trim().to_string(),
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&new)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &self.path)?;
        *s = new;
        Ok(())
    }

    pub async fn send(&self, to: &str, subject: &str, body: String) -> anyhow::Result<()> {
        let s = self.settings.lock().unwrap().clone();
        anyhow::ensure!(s.enabled, "メール送信が無効です（管理画面のメール設定で有効にしてください）");
        let from_name = if s.from_name.is_empty() { None } else { Some(s.from_name.clone()) };
        let from = Mailbox::new(from_name, s.from_address.parse().map_err(|_| anyhow::anyhow!("送信元アドレスが不正です"))?);
        let to: Mailbox = to.trim().parse().map_err(|_| anyhow::anyhow!("宛先アドレスが不正です: {to}"))?;
        let msg = Message::builder()
            .from(from)
            .to(to)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body)?;

        let builder = match s.security.as_str() {
            "tls" => AsyncSmtpTransport::<Tokio1Executor>::relay(&s.host)?,
            "starttls" => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&s.host)?,
            _ => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&s.host),
        };
        let mut builder = builder.port(s.port).timeout(Some(Duration::from_secs(20)));
        if !s.username.is_empty() {
            builder = builder.credentials(Credentials::new(s.username.clone(), s.password.clone()));
        }
        builder.build().send(msg).await.map_err(|e| anyhow::anyhow!("送信に失敗しました: {e}"))?;
        Ok(())
    }
}

pub fn login_url(domain: &str) -> String {
    if domain.is_empty() { "（URL は管理者に確認してください）".into() } else { format!("https://{domain}/") }
}

pub fn initial_password_mail(name: &str, id: &str, password: &str, url: &str, reset: bool) -> (String, String) {
    let subject = if reset { "【Macross】パスワードをリセットしました" } else { "【Macross】アカウントを発行しました" };
    let lead = if reset {
        "Macross（無線）のパスワードを管理者がリセットしました。\n以前のパスワードは使えません。"
    } else {
        "Macross（無線）のアカウントを発行しました。"
    };
    let body = format!(
        "{name} 様\n\n{lead}\n\nURL: {url}\nID: {id}\n初期パスワード: {password}\n\n\
         ログイン後、パスワードの変更を求められます。\n\
         このメールに心当たりがない場合は破棄し、管理者に連絡してください。\n\n-- \nMacross\n"
    );
    (subject.to_string(), body)
}
