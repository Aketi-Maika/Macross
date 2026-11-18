//! メール送信（SMTP）。設定は管理画面から変更し data/mail.json に保存する
//!
//! 自宅回線などでは 25 番ポートからの直接送信がブロックされる（OP25B）ため、
//! プロバイダや Gmail・Microsoft 365 などの SMTP サーバ（587 / 465）を経由して送る。
//! 送信元アドレスを空にすると Macross.yaml の mail.default_from を使う。
//!
//! 既定の送信サーバ（さくらのメールボックス）を Macross.yaml の mail.smtp_* に書いておくと、
//! 管理画面でメール設定を保存していなくても、その設定で送信する（管理画面で保存した設定が優先）。

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
    /// 送信元を空にしたときに使うアドレス
    pub default_from: String,
    /// いま送信に使われる設定が Macross.yaml の既定（管理画面で未設定）
    pub builtin: bool,
    /// 既定の送信サーバ（Macross.yaml）があるか・その内容（パスワードは返さない）
    pub builtin_available: bool,
    pub builtin_host: String,
    pub builtin_username: String,
    /// 実際に送信できる状態か（管理画面の設定が有効、または既定の送信サーバがある）
    pub active: bool,
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
    default_from: String,
    default_name: String,
    settings: Mutex<MailSettings>,
    /// Macross.yaml の既定の送信サーバ（パスワードまで書かれているときだけ）
    builtin: Option<MailSettings>,
}

impl Mailer {
    pub fn open(path: PathBuf, cfg: &crate::config::MailCfg) -> Self {
        let default_from = cfg.default_from.as_str();
        let default_name = cfg.default_from_name.as_str();
        let builtin = (!cfg.smtp_host.trim().is_empty() && !cfg.smtp_password.is_empty()).then(|| MailSettings {
            enabled: true,
            host: cfg.smtp_host.trim().to_string(),
            port: if cfg.smtp_port == 0 { 587 } else { cfg.smtp_port },
            security: if cfg.smtp_security.trim().is_empty() { "starttls".into() } else { cfg.smtp_security.trim().to_string() },
            username: if cfg.smtp_username.trim().is_empty() { default_from.trim().to_string() } else { cfg.smtp_username.trim().to_string() },
            password: cfg.smtp_password.clone(),
            from_address: default_from.trim().to_string(),
            from_name: default_name.trim().to_string(),
        });
        if let Some(b) = &builtin {
            tracing::info!("既定のメール送信サーバ: {}:{} ({})", b.host, b.port, b.username);
        }
        let mut settings: MailSettings = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        if settings.from_address.trim().is_empty() {
            settings.from_address = default_from.trim().to_string();
        }
        if settings.from_name.trim().is_empty() {
            settings.from_name = default_name.trim().to_string();
        }
        Self {
            path,
            default_from: default_from.trim().to_string(),
            default_name: default_name.trim().to_string(),
            settings: Mutex::new(settings),
            builtin,
        }
    }

    /// 送信に使う設定（管理画面で有効にした設定 → Macross.yaml の既定 の順）。どちらも無ければ None
    fn effective(&self) -> Option<MailSettings> {
        let s = self.settings.lock().unwrap().clone();
        if s.enabled && !s.host.trim().is_empty() {
            return Some(s);
        }
        self.builtin.clone()
    }

    pub fn enabled(&self) -> bool {
        self.effective().is_some()
    }

    pub fn view(&self) -> MailView {
        let active = self.effective();
        let s = self.settings.lock().unwrap();
        let using_builtin = !(s.enabled && !s.host.trim().is_empty()) && self.builtin.is_some();
        MailView {
            builtin: using_builtin,
            builtin_available: self.builtin.is_some(),
            builtin_host: self.builtin.as_ref().map(|b| format!("{}:{}", b.host, b.port)).unwrap_or_default(),
            builtin_username: self.builtin.as_ref().map(|b| b.username.clone()).unwrap_or_default(),
            active: active.is_some(),
            enabled: s.enabled,
            host: s.host.clone(),
            port: s.port,
            security: s.security.clone(),
            username: s.username.clone(),
            has_password: !s.password.is_empty(),
            from_address: s.from_address.clone(),
            from_name: s.from_name.clone(),
            default_from: self.default_from.clone(),
        }
    }

    pub fn update(&self, mut u: MailUpdate) -> anyhow::Result<()> {
        if u.from_address.trim().is_empty() {
            u.from_address = self.default_from.clone();
        }
        if u.from_name.trim().is_empty() {
            u.from_name = self.default_name.clone();
        }
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
        let s = self.effective().ok_or_else(|| {
            anyhow::anyhow!("メール送信が無効です（Macross.yaml の mail.smtp_password を設定するか、管理画面のメール設定で有効にしてください）")
        })?;
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
