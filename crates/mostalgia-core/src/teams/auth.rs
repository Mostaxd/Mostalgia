//! Signing in with Microsoft: OAuth 2.0 authorization code flow with PKCE
//! against Microsoft Entra ID, as a public client (no secret). The browser
//! comes back to a one-time listener on `http://localhost:<port>`. The
//! refresh token goes to the system keyring (split into chunks: Windows
//! keeps at most 2560 bytes per credential and Microsoft's refresh tokens
//! can be longer); access tokens stay in memory only.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use super::TeamsConfig;
use super::graph::{Body, Http, Method, Request, ReqwestHttp};
use crate::i18n::{tr, trf};

const KEYRING_SERVICE: &str = "mostalgia";
const KEYRING_USER: &str = "microsoft-refresh-token";
/// Characters per keyring entry (UTF-16 on Windows: 2 bytes each).
const CHUNK: usize = 1000;
/// How long the browser has to come back.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
/// Refresh this long before the access token expires.
const EXPIRY_MARGIN: Duration = Duration::from_secs(120);

/// The signed-in account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub email: String,
}

/// PKCE verifier and its S256 challenge.
#[derive(Clone, Debug)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Result<Self> {
        Ok(Self::from_verifier(random_token(32)?))
    }

    pub fn from_verifier(verifier: String) -> Self {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self { verifier, challenge }
    }
}

/// `bytes` random bytes, base64url.
pub fn random_token(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

fn authority(cfg: &TeamsConfig) -> String {
    format!("https://login.microsoftonline.com/{}/oauth2/v2.0", cfg.tenant())
}

fn require_client_id(cfg: &TeamsConfig) -> Result<String> {
    cfg.client_id().with_context(|| tr("Keine Client-ID für Microsoft eingetragen (Einstellungen → Microsoft Teams)"))
}

/// The page the browser opens to sign in.
pub fn authorize_url(cfg: &TeamsConfig, redirect_uri: &str, pkce: &Pkce, state: &str) -> Result<String> {
    let client_id = require_client_id(cfg)?;
    let mut u = url::Url::parse(&format!("{}/authorize", authority(cfg)))?;
    u.query_pairs_mut()
        .append_pair("client_id", &client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_mode", "query")
        .append_pair("scope", &cfg.scopes().join(" "))
        .append_pair("state", state)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("prompt", "select_account");
    Ok(u.into())
}

/// The code from the browser's request (`GET /?code=…&state=…`), checking
/// the state. Errors carry Microsoft's reason (e.g. the user cancelled).
pub fn parse_redirect(request_line: &str, state: &str) -> Result<String> {
    let target = request_line.split_whitespace().nth(1).unwrap_or_default();
    let u = url::Url::parse(&format!("http://localhost{target}")).context("odd redirect")?;
    let get = |k: &str| u.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned());
    if let Some(err) = get("error") {
        let desc = get("error_description").unwrap_or_default();
        let desc = desc.lines().next().unwrap_or_default();
        if err == "access_denied" {
            bail!("{}", tr("Anmeldung abgebrochen"));
        }
        bail!("{}", trf("Microsoft hat die Anmeldung abgelehnt: {}", &[&format!("{err} {desc}").trim()]));
    }
    if get("state").as_deref() != Some(state) {
        bail!("{}", tr("Die Antwort gehört nicht zu dieser Anmeldung"));
    }
    get("code").filter(|c| !c.is_empty()).context("no code in the redirect")
}

/// What the token endpoint returns.
#[derive(Clone, Debug, PartialEq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: u64,
}

fn token_request(http: &dyn Http, cfg: &TeamsConfig, mut form: Vec<(String, String)>) -> Result<Tokens> {
    let client_id = require_client_id(cfg)?;
    form.insert(0, ("client_id".into(), client_id));
    form.push(("scope".into(), cfg.scopes().join(" ")));
    let req = Request { method: Method::Post, url: format!("{}/token", authority(cfg)), headers: Vec::new(), body: Some(Body::Form(form)) };
    let resp = http.send(&req)?;
    let v: serde_json::Value = serde_json::from_str(&resp.body).unwrap_or_default();
    if !resp.ok() {
        let code = v["error"].as_str().unwrap_or("error");
        let desc = v["error_description"].as_str().unwrap_or_default().lines().next().unwrap_or_default().to_string();
        if code == "invalid_grant" {
            bail!("{}", tr("Die Anmeldung bei Microsoft ist abgelaufen. Bitte neu anmelden."));
        }
        bail!("{}", trf("Microsoft hat die Anmeldung abgelehnt: {}", &[&format!("{code} {desc}").trim()]));
    }
    Ok(Tokens {
        access_token: v["access_token"].as_str().context("no access token")?.into(),
        refresh_token: v["refresh_token"].as_str().map(str::to_string),
        expires_in: v["expires_in"].as_u64().unwrap_or(3600),
    })
}

pub fn exchange_code(http: &dyn Http, cfg: &TeamsConfig, code: &str, redirect_uri: &str, verifier: &str) -> Result<Tokens> {
    token_request(
        http,
        cfg,
        vec![
            ("grant_type".into(), "authorization_code".into()),
            ("code".into(), code.into()),
            ("redirect_uri".into(), redirect_uri.into()),
            ("code_verifier".into(), verifier.into()),
        ],
    )
}

pub fn refresh(http: &dyn Http, cfg: &TeamsConfig, refresh_token: &str) -> Result<Tokens> {
    token_request(http, cfg, vec![("grant_type".into(), "refresh_token".into()), ("refresh_token".into(), refresh_token.into())])
}

/// Where the refresh token is kept: the keyring (tests use memory).
pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}

pub struct Keyring;

impl SecretStore for Keyring {
    fn get(&self, key: &str) -> Result<Option<String>> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, key).context(tr("Schlüsselbund nicht erreichbar"))?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e).context(tr("Schlüsselbund nicht erreichbar")),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        keyring::Entry::new(KEYRING_SERVICE, key)
            .and_then(|e| e.set_password(value))
            .context(tr("Die Microsoft-Anmeldung konnte nicht im Schlüsselbund gespeichert werden"))
    }

    fn delete(&self, key: &str) -> Result<()> {
        match keyring::Entry::new(KEYRING_SERVICE, key).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e).context(tr("Schlüsselbund nicht erreichbar")),
        }
    }
}

/// Saves the refresh token in chunks: `<user>` holds the number of chunks,
/// `<user>-0`, `<user>-1`, … the parts.
pub fn save_refresh_token(store: &dyn SecretStore, token: &str) -> Result<()> {
    let old = chunk_count(store)?;
    let chars: Vec<char> = token.chars().collect();
    let parts: Vec<String> = chars.chunks(CHUNK).map(|c| c.iter().collect()).collect();
    for (i, part) in parts.iter().enumerate() {
        store.set(&format!("{KEYRING_USER}-{i}"), part)?;
    }
    store.set(KEYRING_USER, &parts.len().to_string())?;
    for i in parts.len()..old {
        store.delete(&format!("{KEYRING_USER}-{i}"))?;
    }
    Ok(())
}

fn chunk_count(store: &dyn SecretStore) -> Result<usize> {
    Ok(store.get(KEYRING_USER)?.and_then(|n| n.trim().parse().ok()).unwrap_or(0))
}

pub fn load_refresh_token(store: &dyn SecretStore) -> Result<Option<String>> {
    let n = chunk_count(store)?;
    if n == 0 {
        return Ok(None);
    }
    let mut out = String::new();
    for i in 0..n {
        match store.get(&format!("{KEYRING_USER}-{i}"))? {
            Some(part) => out.push_str(&part),
            None => return Ok(None),
        }
    }
    Ok(Some(out))
}

pub fn delete_refresh_token(store: &dyn SecretStore) -> Result<()> {
    let n = chunk_count(store)?;
    for i in 0..n {
        store.delete(&format!("{KEYRING_USER}-{i}"))?;
    }
    store.delete(KEYRING_USER)
}

/// Access tokens of this process, by client id.
static ACCESS: Mutex<Option<(String, String, Instant)>> = Mutex::new(None);

fn cache_access(client_id: &str, tokens: &Tokens) {
    let expires = Instant::now() + Duration::from_secs(tokens.expires_in).saturating_sub(EXPIRY_MARGIN);
    *ACCESS.lock().unwrap_or_else(|e| e.into_inner()) = Some((client_id.into(), tokens.access_token.clone(), expires));
}

fn cached_access(client_id: &str) -> Option<String> {
    let guard = ACCESS.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().filter(|(id, _, exp)| id == client_id && Instant::now() < *exp).map(|(_, t, _)| t.clone())
}

fn clear_access() {
    *ACCESS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Valid access tokens for the signed-in account, refreshed from the
/// stored refresh token when needed.
pub struct TokenSource {
    cfg: TeamsConfig,
    http: Box<dyn Http>,
    store: Box<dyn SecretStore>,
}

impl TokenSource {
    pub fn new(cfg: &TeamsConfig) -> Result<Self> {
        require_client_id(cfg)?;
        Ok(Self { cfg: cfg.clone(), http: Box::new(ReqwestHttp::new()?), store: Box::new(Keyring) })
    }

    pub fn with(cfg: &TeamsConfig, http: Box<dyn Http>, store: Box<dyn SecretStore>) -> Self {
        Self { cfg: cfg.clone(), http, store }
    }

    pub fn access_token(&self) -> Result<String> {
        let client_id = require_client_id(&self.cfg)?;
        if let Some(t) = cached_access(&client_id) {
            return Ok(t);
        }
        let refresh_token = load_refresh_token(self.store.as_ref())?.with_context(|| tr("Nicht bei Microsoft angemeldet"))?;
        let tokens = refresh(self.http.as_ref(), &self.cfg, &refresh_token)?;
        if let Some(rt) = tokens.refresh_token.as_deref().filter(|rt| *rt != refresh_token) {
            save_refresh_token(self.store.as_ref(), rt)?;
        }
        cache_access(&client_id, &tokens);
        Ok(tokens.access_token)
    }
}

/// Whether a refresh token is stored (signed in, as far as we know).
pub fn has_session() -> bool {
    load_refresh_token(&Keyring).ok().flatten().is_some()
}

/// Opens a URL in the default browser.
pub fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    let status = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).status();
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("open").arg(url).status();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let status = std::process::Command::new("xdg-open").arg(url).status();
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => bail!("{}", tr("Der Browser konnte nicht geöffnet werden")),
    }
}

/// Listens on localhost (IPv4 and, on the same port, IPv6: browsers
/// differ in what `localhost` means).
struct Loopback {
    listeners: Vec<TcpListener>,
    port: u16,
}

impl Loopback {
    fn bind() -> Result<Self> {
        let v4 = TcpListener::bind("127.0.0.1:0").context("no free port for the sign-in")?;
        let port = v4.local_addr()?.port();
        let mut listeners = vec![v4];
        if let Ok(v6) = TcpListener::bind(("::1", port)) {
            listeners.push(v6);
        }
        for l in &listeners {
            l.set_nonblocking(true)?;
        }
        Ok(Self { listeners, port })
    }

    fn redirect_uri(&self) -> String {
        format!("http://localhost:{}", self.port)
    }

    /// The first request's line that carries `code` or `error` (favicon
    /// requests and the like get a 404).
    fn wait(&self, cancel: &AtomicBool, timeout: Duration) -> Result<(String, TcpStream)> {
        let until = Instant::now() + timeout;
        loop {
            if cancel.load(Ordering::Relaxed) {
                bail!("{}", tr("Anmeldung abgebrochen"));
            }
            if Instant::now() > until {
                bail!("{}", tr("Die Anmeldung hat zu lange gedauert"));
            }
            for l in &self.listeners {
                if let Ok((mut stream, _)) = l.accept() {
                    stream.set_nonblocking(false)?;
                    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                    let line = read_request_line(&mut stream);
                    if line.contains("code=") || line.contains("error=") {
                        return Ok((line, stream));
                    }
                    let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn read_request_line(stream: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
        match stream.read(&mut byte) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&byte[..n]),
        }
    }
    String::from_utf8_lossy(&buf).lines().next().unwrap_or_default().to_string()
}

fn respond(mut stream: TcpStream, ok: bool, message: &str) {
    let title = if ok { tr("Angemeldet") } else { tr("Anmeldung fehlgeschlagen") };
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>Mostalgia</title>\
         <body style=\"font-family:system-ui,sans-serif;max-width:32em;margin:5em auto;text-align:center\">\
         <h1>{}</h1><p>{}</p></body>",
        escape_html(title),
        escape_html(message)
    );
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
}

pub(crate) fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Signs in: opens Microsoft's page through `open`, waits for the browser
/// to come back, stores the refresh token and returns the account.
/// `cancel` stops waiting.
pub fn login(cfg: &TeamsConfig, open: &dyn Fn(&str) -> Result<()>, cancel: &AtomicBool) -> Result<Account> {
    let client_id = require_client_id(cfg)?;
    let http = ReqwestHttp::new()?;
    let loopback = Loopback::bind()?;
    let redirect = loopback.redirect_uri();
    let pkce = Pkce::new()?;
    let state = random_token(16)?;
    open(&authorize_url(cfg, &redirect, &pkce, &state)?)?;
    let (line, stream) = loopback.wait(cancel, LOGIN_TIMEOUT)?;
    let result = parse_redirect(&line, &state).and_then(|code| exchange_code(&http, cfg, &code, &redirect, &pkce.verifier));
    let tokens = match result {
        Ok(t) => {
            respond(stream, true, tr("Du kannst dieses Fenster schließen und zu Mostalgia zurückkehren."));
            t
        }
        Err(e) => {
            respond(stream, false, &format!("{e:#}"));
            return Err(e);
        }
    };
    let refresh_token = tokens.refresh_token.as_deref().with_context(|| tr("Microsoft hat kein Refresh-Token geschickt (fehlt offline_access?)"))?;
    save_refresh_token(&Keyring, refresh_token)?;
    cache_access(&client_id, &tokens);
    let token = tokens.access_token.clone();
    let graph = super::Graph::new(Box::new(http), Box::new(move || Ok(token.clone())));
    let me = graph.me()?;
    Ok(Account { name: me.name, email: me.email })
}

/// Signs out: forgets the tokens (the app registration's consent stays).
pub fn logout() -> Result<()> {
    clear_access();
    delete_refresh_token(&Keyring)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::teams::graph::mock::MockHttp;

    #[derive(Default)]
    struct Memory(Mutex<HashMap<String, String>>);

    impl SecretStore for Memory {
        fn get(&self, key: &str) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        fn set(&self, key: &str, value: &str) -> Result<()> {
            self.0.lock().unwrap().insert(key.into(), value.into());
            Ok(())
        }
        fn delete(&self, key: &str) -> Result<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }

    fn cfg() -> TeamsConfig {
        TeamsConfig { client_id: "11111111-2222-3333-4444-555555555555".into(), tenant: "organizations".into(), ..TeamsConfig::default() }
    }

    #[test]
    fn pkce_matches_the_rfc_example() {
        // RFC 7636, appendix B.
        let p = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(p.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let random = Pkce::new().unwrap();
        assert_eq!(random.verifier.len(), 43);
        assert_ne!(random.verifier, Pkce::new().unwrap().verifier);
    }

    #[test]
    fn authorize_url_has_everything() {
        let p = Pkce::from_verifier("v".into());
        let u = authorize_url(&cfg(), "http://localhost:5000", &p, "st").unwrap();
        assert!(u.starts_with("https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?"));
        for part in ["client_id=11111111-", "code_challenge_method=S256", "state=st", "redirect_uri=http%3A%2F%2Flocalhost%3A5000", "offline_access", "Calendars.Read"] {
            assert!(u.contains(part), "{part} missing in {u}");
        }
        assert!(authorize_url(&TeamsConfig::default(), "http://localhost:1", &p, "s").is_err(), "no client id, no sign-in");
    }

    #[test]
    fn redirect_is_checked() {
        assert_eq!(parse_redirect("GET /?code=abc&state=st HTTP/1.1", "st").unwrap(), "abc");
        assert!(parse_redirect("GET /?code=abc&state=other HTTP/1.1", "st").is_err());
        let err = parse_redirect("GET /?error=invalid_client&error_description=AADSTS700016%3A+app+not+found HTTP/1.1", "st").unwrap_err();
        assert!(format!("{err}").contains("AADSTS700016"), "{err}");
    }

    #[test]
    fn long_tokens_are_chunked() {
        let store = Memory::default();
        let long: String = (0..2500).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
        save_refresh_token(&store, &long).unwrap();
        assert_eq!(store.get(KEYRING_USER).unwrap().as_deref(), Some("3"));
        assert_eq!(load_refresh_token(&store).unwrap().as_deref(), Some(long.as_str()));
        save_refresh_token(&store, "short").unwrap();
        assert_eq!(load_refresh_token(&store).unwrap().as_deref(), Some("short"));
        assert!(store.get(&format!("{KEYRING_USER}-2")).unwrap().is_none(), "old chunks removed");
        delete_refresh_token(&store).unwrap();
        assert_eq!(load_refresh_token(&store).unwrap(), None);
        assert!(store.0.lock().unwrap().is_empty());
    }

    #[test]
    fn refresh_rotates_the_stored_token() {
        clear_access();
        let http = MockHttp::default();
        http.on(Method::Post, "/oauth2/v2.0/token", 200, r#"{"access_token":"AT2","refresh_token":"RT2","expires_in":3599}"#);
        let store = Memory::default();
        save_refresh_token(&store, "RT1").unwrap();
        let cfg = TeamsConfig { client_id: "99999999-2222-3333-4444-555555555555".into(), ..cfg() };
        let src = TokenSource::with(&cfg, Box::new(http.clone()), Box::new(store));
        assert_eq!(src.access_token().unwrap(), "AT2");
        assert_eq!(src.access_token().unwrap(), "AT2", "second call comes from the cache");
        assert_eq!(http.requests().len(), 1);
        let Some(Body::Form(form)) = &http.requests()[0].body else { panic!() };
        assert!(form.contains(&("grant_type".into(), "refresh_token".into())));
        assert!(form.contains(&("refresh_token".into(), "RT1".into())));
        assert_eq!(load_refresh_token(src.store.as_ref()).unwrap().as_deref(), Some("RT2"));
        clear_access();
    }

    #[test]
    fn expired_refresh_token_asks_to_sign_in_again() {
        let http = MockHttp::default();
        http.on(Method::Post, "/token", 400, r#"{"error":"invalid_grant","error_description":"AADSTS70008: expired"}"#);
        let err = refresh(&http, &cfg(), "old").unwrap_err();
        assert!(!format!("{err}").contains("AADSTS"), "plain words: {err}");
    }

    #[test]
    fn loopback_receives_the_code() {
        let lb = Loopback::bind().unwrap();
        let port = lb.port;
        let client = std::thread::spawn(move || {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"GET /?code=C0DE&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
            let mut page = String::new();
            let _ = s.read_to_string(&mut page);
            page
        });
        let (line, stream) = lb.wait(&AtomicBool::new(false), Duration::from_secs(10)).unwrap();
        assert_eq!(parse_redirect(&line, "xyz").unwrap(), "C0DE");
        respond(stream, true, "ok <b>");
        let page = client.join().unwrap();
        assert!(page.starts_with("HTTP/1.1 200"));
        assert!(page.contains("ok &lt;b&gt;"));
    }
}
