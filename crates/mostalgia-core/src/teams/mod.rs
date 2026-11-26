//! Microsoft Teams through Microsoft Graph: sign in with a Microsoft
//! account, read the calendar to know which Teams meeting a recording
//! belongs to, and send results back (the summary to the meeting chat,
//! action items to Microsoft To Do). See `docs/teams-integration.md`.
//!
//! Everything goes straight to Microsoft with the user's own delegated
//! token. The refresh token lives in the system keyring, the settings in
//! the config, and which event a meeting belongs to in its folder
//! (`teams.json`, see [`link`]).

pub mod actions;
pub mod auth;
pub mod brief;
pub mod calendar;
pub mod chat;
pub mod graph;
pub mod link;
pub mod ops;
pub mod vtt;

use serde::{Deserialize, Serialize};

pub use auth::Account;
pub use calendar::{Attendee, Event};
pub use graph::Graph;

/// Shown in the settings until a real client ID is entered.
pub const CLIENT_ID_PLACEHOLDER: &str = "00000000-0000-0000-0000-000000000000";
/// Personal and work accounts.
pub const DEFAULT_TENANT: &str = "common";
/// The To Do list action items go to (created when missing).
pub const DEFAULT_TODO_LIST: &str = "Mostalgia";

/// Environment overrides (for testing an app registration without
/// touching the settings).
const ENV_CLIENT_ID: &str = "MOSTALGIA_MS_CLIENT_ID";
const ENV_TENANT: &str = "MOSTALGIA_MS_TENANT";
/// A stand-in for Microsoft on this computer, for trying the whole flow
/// without an account: sign-in goes to `<it>/<tenant>/oauth2/v2.0`, Graph
/// to `<it>/v1.0`. Only loopback addresses count, so it can't send a
/// token anywhere else.
const ENV_TEST_SERVER: &str = "MOSTALGIA_MS_TEST_SERVER";

/// The local test server from [`ENV_TEST_SERVER`], if set to a loopback URL.
pub(crate) fn test_server() -> Option<String> {
    loopback_url(&std::env::var(ENV_TEST_SERVER).ok()?)
}

fn loopback_url(v: &str) -> Option<String> {
    let u = url::Url::parse(v.trim()).ok()?;
    let local = matches!(u.host(), Some(url::Host::Domain("localhost")))
        || matches!(u.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
        || matches!(u.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback());
    (u.scheme() == "http" && local).then(|| v.trim().trim_end_matches('/').to_string())
}

/// The Teams settings (part of [`crate::config::Config`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TeamsConfig {
    /// The app registration's Application (client) ID in Entra ID.
    pub client_id: String,
    /// "common", "organizations", "consumers", a tenant id or a domain.
    pub tenant: String,
    /// Read the calendar for upcoming Teams meetings.
    pub calendar_sync: bool,
    /// Name a recording after the meeting it belongs to.
    pub auto_name: bool,
    /// Attendee names and words from the invite as hints for Whisper,
    /// and attendees as speaker-name suggestions.
    pub attendee_hints: bool,
    /// Post the summary to the meeting chat as soon as it's written.
    pub auto_post_summary: bool,
    /// Include the action items when posting the summary.
    pub post_action_items: bool,
    /// The To Do list for action items.
    pub todo_list: String,
    /// Only my own action items (owner is me or open) go to To Do.
    pub todo_only_mine: bool,
    /// Ask for the Teams transcript permission (needs admin consent).
    pub transcripts: bool,
    /// Minutes before a meeting its brief is ready (0 = no brief).
    pub brief_minutes: u32,
    /// Read the meeting chat (links, decisions typed during the call) into
    /// the summary and Ask.
    pub chat_context: bool,
    /// Recurring meetings: the summary says what changed since the last one.
    pub series_compare: bool,
    /// Display name and address of the signed-in account (from `/me`),
    /// shown in the settings without asking Microsoft each time.
    pub account_name: String,
    pub account_email: String,
}

impl Default for TeamsConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            tenant: DEFAULT_TENANT.into(),
            calendar_sync: true,
            auto_name: true,
            attendee_hints: true,
            auto_post_summary: false,
            post_action_items: true,
            todo_list: DEFAULT_TODO_LIST.into(),
            todo_only_mine: false,
            transcripts: false,
            brief_minutes: 10,
            chat_context: true,
            series_compare: true,
            account_name: String::new(),
            account_email: String::new(),
        }
    }
}

impl TeamsConfig {
    /// The client ID in use: the environment, else the settings. `None`
    /// while it's empty or still the placeholder.
    pub fn client_id(&self) -> Option<String> {
        let id = std::env::var(ENV_CLIENT_ID).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| self.client_id.clone());
        let id = id.trim().to_string();
        (!id.is_empty() && id != CLIENT_ID_PLACEHOLDER).then_some(id)
    }

    /// The tenant in use: the environment, else the settings, else "common".
    pub fn tenant(&self) -> String {
        let t = std::env::var(ENV_TENANT).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| self.tenant.clone());
        let t = t.trim();
        if t.is_empty() { DEFAULT_TENANT.into() } else { t.to_string() }
    }

    /// Whether the client ID looks like a GUID (the settings warn otherwise).
    pub fn client_id_valid(&self) -> bool {
        self.client_id().is_some_and(|id| is_guid(&id))
    }

    /// The delegated permissions to ask for.
    pub fn scopes(&self) -> Vec<&'static str> {
        let mut s = vec![
            "openid",
            "profile",
            "offline_access",
            "User.Read",
            "Calendars.Read",
            "OnlineMeetings.Read",
            "ChatMessage.Send",
            "Tasks.ReadWrite",
        ];
        if self.chat_context {
            s.push("Chat.Read");
        }
        if self.transcripts {
            s.push("OnlineMeetingTranscript.Read.All");
        }
        s
    }

    /// The To Do list's name (the default when left empty).
    pub fn todo_list(&self) -> &str {
        let name = self.todo_list.trim();
        if name.is_empty() { DEFAULT_TODO_LIST } else { name }
    }

    /// Signed in: an account was stored (the token may still have expired).
    pub fn signed_in(&self) -> bool {
        !self.account_email.is_empty() || !self.account_name.is_empty()
    }

    /// The account as shown in the settings ("Anna Lehmann (anna@firma.de)").
    pub fn account_label(&self) -> String {
        match (self.account_name.trim(), self.account_email.trim()) {
            ("", "") => String::new(),
            (name, "") => name.into(),
            ("", mail) => mail.into(),
            (name, mail) => format!("{name} ({mail})"),
        }
    }
}

/// `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` in hex.
pub fn is_guid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts.iter().zip([8, 4, 4, 4, 12]).all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The signed-in user's Graph client, refreshing the access token as needed.
pub fn client(cfg: &TeamsConfig) -> anyhow::Result<Graph> {
    let tokens = auth::TokenSource::new(cfg)?;
    Ok(Graph::new(Box::new(graph::ReqwestHttp::new()?), Box::new(move || tokens.access_token())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_is_no_client_id() {
        let mut c = TeamsConfig::default();
        assert_eq!(c.client_id(), None);
        c.client_id = CLIENT_ID_PLACEHOLDER.into();
        assert_eq!(c.client_id(), None);
        c.client_id = " 11111111-2222-3333-4444-555555555555 ".into();
        assert_eq!(c.client_id().as_deref(), Some("11111111-2222-3333-4444-555555555555"));
        assert!(c.client_id_valid());
        c.client_id = "not-a-guid".into();
        assert!(!c.client_id_valid());
    }

    #[test]
    fn transcripts_add_their_scope() {
        let mut c = TeamsConfig::default();
        assert!(!c.scopes().contains(&"OnlineMeetingTranscript.Read.All"));
        c.transcripts = true;
        assert!(c.scopes().contains(&"OnlineMeetingTranscript.Read.All"));
        assert!(c.scopes().contains(&"offline_access"));
        assert!(c.scopes().contains(&"Chat.Read"));
        c.chat_context = false;
        assert!(!c.scopes().contains(&"Chat.Read"));
    }

    #[test]
    fn old_configs_get_defaults() {
        let c: TeamsConfig = serde_json::from_str(r#"{"client_id":"x"}"#).unwrap();
        assert_eq!(c.tenant, DEFAULT_TENANT);
        assert_eq!(c.todo_list(), DEFAULT_TODO_LIST);
        assert!(c.calendar_sync);
    }

    #[test]
    fn test_server_is_loopback_only() {
        assert_eq!(loopback_url("http://127.0.0.1:9/").as_deref(), Some("http://127.0.0.1:9"));
        assert_eq!(loopback_url("http://localhost:9").as_deref(), Some("http://localhost:9"));
        assert_eq!(loopback_url("http://[::1]:9").as_deref(), Some("http://[::1]:9"));
        assert_eq!(loopback_url("https://evil.example"), None);
        assert_eq!(loopback_url("http://127.0.0.1.evil.example"), None);
        assert_eq!(loopback_url("http://10.0.0.1:9"), None);
    }

    #[test]
    fn account_label() {
        let mut c = TeamsConfig::default();
        assert!(!c.signed_in());
        c.account_name = "Anna Lehmann".into();
        c.account_email = "anna@firma.de".into();
        assert_eq!(c.account_label(), "Anna Lehmann (anna@firma.de)");
    }
}
