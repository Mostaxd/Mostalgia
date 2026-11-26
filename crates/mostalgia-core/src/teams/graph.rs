//! Microsoft Graph calls. HTTP goes through the [`Http`] trait so tests
//! answer with canned Graph responses instead of a real account.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

use super::calendar::{self, Event};
use super::chat::{self, ChatLine};
use crate::i18n::{tr, trf};

pub const GRAPH_ROOT: &str = "https://graph.microsoft.com/v1.0";

/// Most pages followed for one list (`@odata.nextLink`).
const MAX_PAGES: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    Json(Value),
    /// `application/x-www-form-urlencoded` (the token endpoint).
    Form(Vec<(String, String)>),
}

#[derive(Clone, Debug)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Body>,
}

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json(&self) -> Result<Value> {
        if self.body.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&self.body).context("Microsoft sent a response that isn't JSON")
    }
}

/// Sends a request; the real one is [`ReqwestHttp`].
pub trait Http: Send + Sync {
    fn send(&self, req: &Request) -> Result<Response>;
}

pub struct ReqwestHttp(reqwest::blocking::Client);

impl ReqwestHttp {
    pub fn new() -> Result<Self> {
        Ok(Self(
            reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent(concat!("Mostalgia/", env!("CARGO_PKG_VERSION")))
                .build()
                .context("creating the HTTP client")?,
        ))
    }
}

impl Http for ReqwestHttp {
    fn send(&self, req: &Request) -> Result<Response> {
        let mut b = match req.method {
            Method::Get => self.0.get(&req.url),
            Method::Post => self.0.post(&req.url),
        };
        for (k, v) in &req.headers {
            b = b.header(k, v);
        }
        b = match &req.body {
            Some(Body::Json(v)) => b.json(v),
            Some(Body::Form(pairs)) => {
                let body = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish();
                b.header("Content-Type", "application/x-www-form-urlencoded").body(body)
            }
            None => b,
        };
        let resp = b.send().with_context(|| tr("Microsoft ist nicht erreichbar"))?;
        let status = resp.status().as_u16();
        let body = resp.text().unwrap_or_default();
        Ok(Response { status, body })
    }
}

/// Gives a valid access token (refreshing it when needed).
pub type TokenFn = Box<dyn Fn() -> Result<String> + Send + Sync>;

/// The signed-in user as Graph knows them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Me {
    pub id: String,
    pub name: String,
    pub email: String,
}

/// A Teams meeting as Graph's `onlineMeetings` has it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OnlineMeeting {
    pub id: String,
    /// The meeting chat (`chatInfo.threadId`).
    pub chat_id: Option<String>,
}

/// A Teams transcript of a meeting.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptInfo {
    pub id: String,
    pub created: Option<DateTime<Utc>>,
}

/// A task created in To Do.
#[derive(Clone, Debug, PartialEq)]
pub struct CreatedTask {
    pub id: String,
    pub title: String,
}

pub struct Graph {
    http: Box<dyn Http>,
    token: TokenFn,
    root: String,
}

impl Graph {
    pub fn new(http: Box<dyn Http>, token: TokenFn) -> Self {
        Self { http, token, root: GRAPH_ROOT.into() }
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("https://") { path.to_string() } else { format!("{}{path}", self.root) }
    }

    fn call(&self, method: Method, path: &str, body: Option<Value>, extra: &[(&str, &str)]) -> Result<Response> {
        let token = (self.token)()?;
        let mut headers = vec![("Authorization".to_string(), format!("Bearer {token}")), ("Accept".into(), "application/json".into())];
        headers.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let req = Request { method, url: self.url(path), headers, body: body.map(Body::Json) };
        let resp = self.http.send(&req)?;
        if !resp.ok() {
            bail!("{}", graph_error(&resp));
        }
        Ok(resp)
    }

    fn get(&self, path: &str, extra: &[(&str, &str)]) -> Result<Value> {
        self.call(Method::Get, path, None, extra)?.json()
    }

    fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.call(Method::Post, path, Some(body), &[])?.json()
    }

    /// Every item of a list, following `@odata.nextLink`.
    fn get_all(&self, path: &str, extra: &[(&str, &str)]) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut next = Some(path.to_string());
        for _ in 0..MAX_PAGES {
            let Some(p) = next.take() else { break };
            let page = self.get(&p, extra)?;
            if let Some(items) = page["value"].as_array() {
                out.extend(items.iter().cloned());
            }
            next = page["@odata.nextLink"].as_str().map(str::to_string);
        }
        Ok(out)
    }

    pub fn me(&self) -> Result<Me> {
        let v = self.get("/me?$select=id,displayName,mail,userPrincipalName", &[])?;
        Ok(Me {
            id: v["id"].as_str().unwrap_or_default().into(),
            name: v["displayName"].as_str().unwrap_or_default().into(),
            email: v["mail"].as_str().or(v["userPrincipalName"].as_str()).unwrap_or_default().into(),
        })
    }

    /// Events between `from` and `to`, earliest first (all of them; see
    /// [`Event::is_teams`] for the Teams meetings among them).
    pub fn events(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Event>> {
        let fmt = |t: DateTime<Utc>| t.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let mut u = url::Url::parse(&format!("{}/me/calendarView", self.root))?;
        u.query_pairs_mut()
            .append_pair("startDateTime", &fmt(from))
            .append_pair("endDateTime", &fmt(to))
            .append_pair(
                "$select",
                "id,subject,start,end,attendees,organizer,isOnlineMeeting,onlineMeetingProvider,onlineMeeting,bodyPreview,body,webLink,isCancelled,location,seriesMasterId,type",
            )
            .append_pair("$orderby", "start/dateTime")
            .append_pair("$top", "50");
        let items = self.get_all(u.as_str(), &[("Prefer", "outlook.timezone=\"UTC\", outlook.body-content-type=\"text\"")])?;
        let mut events: Vec<Event> = items.iter().filter_map(calendar::parse_event).filter(|e| !e.cancelled).collect();
        events.sort_by_key(|e| e.start);
        Ok(events)
    }

    /// Events from now until `hours` ahead (and those still running).
    pub fn upcoming(&self, now: DateTime<Utc>, hours: i64) -> Result<Vec<Event>> {
        let events = self.events(now - Duration::hours(4), now + Duration::hours(hours))?;
        Ok(events.into_iter().filter(|e| e.end > now).collect())
    }

    /// The Teams meeting behind a join link (organizer or attendee).
    pub fn online_meeting(&self, join_url: &str) -> Result<Option<OnlineMeeting>> {
        let mut u = url::Url::parse(&format!("{}/me/onlineMeetings", self.root))?;
        u.query_pairs_mut().append_pair("$filter", &format!("JoinWebUrl eq '{}'", join_url.replace('\'', "''")));
        let v = self.get(u.as_str(), &[])?;
        Ok(v["value"].as_array().and_then(|a| a.first()).map(|m| OnlineMeeting {
            id: m["id"].as_str().unwrap_or_default().into(),
            chat_id: m["chatInfo"]["threadId"].as_str().map(str::to_string),
        }))
    }

    /// The meeting chat of an event: its online meeting's chat thread.
    pub fn meeting_chat(&self, event: &Event) -> Result<String> {
        let Some(join) = event.join_url.as_deref() else {
            bail!("{}", tr("Der Termin ist keine Teams-Besprechung"));
        };
        let meeting = self.online_meeting(join)?.with_context(|| tr("Teams kennt diese Besprechung nicht (nur Geschäfts- oder Schulkonten)"))?;
        meeting.chat_id.with_context(|| tr("Die Besprechung hat keinen Chat"))
    }

    /// The chat's messages written between `from` and `to`, oldest first
    /// (newest pages first from Graph, so a long-running meeting chat of a
    /// recurring series is only read back as far as needed).
    pub fn chat_messages(&self, chat_id: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<ChatLine>> {
        let mut u = url::Url::parse(&format!("{}/chats/{}/messages", self.root, encode_path(chat_id)))?;
        u.query_pairs_mut()
            .append_pair("$top", "50")
            .append_pair("$orderby", "createdDateTime desc")
            .append_pair("$filter", &format!("createdDateTime lt {}", to.format("%Y-%m-%dT%H:%M:%SZ")));
        let mut out = Vec::new();
        let mut next = Some(u.to_string());
        for _ in 0..MAX_PAGES {
            let Some(p) = next.take() else { break };
            let page = self.get(&p, &[])?;
            let items = page["value"].as_array().cloned().unwrap_or_default();
            let mut older = false;
            for v in &items {
                let created = v["createdDateTime"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map(|t| t.with_timezone(&Utc));
                if created.is_some_and(|t| t < from) {
                    older = true;
                }
                out.extend(chat::parse_message(v));
            }
            if older {
                break;
            }
            next = page["@odata.nextLink"].as_str().map(str::to_string);
        }
        Ok(chat::window(out, from, to, &[]))
    }

    /// Posts HTML to a chat; returns the message id.
    pub fn post_chat_message(&self, chat_id: &str, html: &str) -> Result<String> {
        let v = self.post(
            &format!("/chats/{}/messages", encode_path(chat_id)),
            json!({ "body": { "contentType": "html", "content": html } }),
        )?;
        Ok(v["id"].as_str().unwrap_or_default().into())
    }

    /// The meeting's Teams transcripts, oldest first.
    pub fn transcripts(&self, meeting_id: &str) -> Result<Vec<TranscriptInfo>> {
        let items = self.get_all(&format!("/me/onlineMeetings/{}/transcripts", encode_path(meeting_id)), &[])?;
        let mut out: Vec<TranscriptInfo> = items
            .iter()
            .map(|t| TranscriptInfo {
                id: t["id"].as_str().unwrap_or_default().into(),
                created: t["createdDateTime"].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&Utc)),
            })
            .filter(|t| !t.id.is_empty())
            .collect();
        out.sort_by_key(|t| t.created);
        Ok(out)
    }

    /// A transcript as WebVTT (`<v Name>` voice tags).
    pub fn transcript_vtt(&self, meeting_id: &str, transcript_id: &str) -> Result<String> {
        let path = format!(
            "/me/onlineMeetings/{}/transcripts/{}/content?$format=text/vtt",
            encode_path(meeting_id),
            encode_path(transcript_id)
        );
        Ok(self.call(Method::Get, &path, None, &[("Accept", "text/vtt")])?.body)
    }

    /// The id of the To Do list called `name`, created when missing.
    pub fn todo_list(&self, name: &str) -> Result<String> {
        let lists = self.get_all("/me/todo/lists", &[])?;
        if let Some(id) = lists
            .iter()
            .find(|l| l["displayName"].as_str().is_some_and(|n| n.trim().eq_ignore_ascii_case(name.trim())))
            .and_then(|l| l["id"].as_str())
        {
            return Ok(id.into());
        }
        let v = self.post("/me/todo/lists", json!({ "displayName": name }))?;
        v["id"].as_str().map(str::to_string).with_context(|| tr("To Do hat die Liste nicht angelegt"))
    }

    /// Adds a task to a To Do list.
    pub fn add_task(&self, list_id: &str, task: &super::actions::ActionItem, note: &str) -> Result<CreatedTask> {
        let mut body = json!({
            "title": task.task,
            "body": { "content": note, "contentType": "text" },
        });
        if let Some(due) = task.due_date {
            body["dueDateTime"] = json!({ "dateTime": format!("{}T00:00:00", due.format("%Y-%m-%d")), "timeZone": "UTC" });
        }
        let v = self.post(&format!("/me/todo/lists/{}/tasks", encode_path(list_id)), body)?;
        Ok(CreatedTask { id: v["id"].as_str().unwrap_or_default().into(), title: v["title"].as_str().unwrap_or(&task.task).into() })
    }
}

/// One path segment, percent-encoded (Graph ids contain `=`, `:` and `@`).
fn encode_path(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20")
}

/// A readable reason for a failed Graph call.
pub fn graph_error(resp: &Response) -> String {
    let v: Value = serde_json::from_str(&resp.body).unwrap_or(Value::Null);
    let code = v["error"]["code"].as_str().unwrap_or_default();
    let message = v["error"]["message"].as_str().unwrap_or_default();
    let reason = match resp.status {
        401 => tr("Die Anmeldung bei Microsoft ist abgelaufen. Bitte neu anmelden.").to_string(),
        403 => trf("Microsoft erlaubt das nicht ({}). Fehlt eine Berechtigung oder die Zustimmung des Admins?", &[&if code.is_empty() { "403" } else { code }]),
        404 => tr("Bei Microsoft nicht gefunden").to_string(),
        429 => tr("Microsoft bremst gerade (zu viele Anfragen). Bitte gleich noch einmal versuchen.").to_string(),
        s => trf("Microsoft Graph antwortet mit Fehler {}", &[&s]),
    };
    if message.is_empty() { reason } else { format!("{reason}: {message}") }
}

#[cfg(test)]
pub(crate) mod mock {
    //! Canned Graph answers for tests.
    use std::sync::{Arc, Mutex};

    use super::*;

    type Route = (Method, String, u16, String);

    /// Answers requests whose URL contains a route's pattern (first match
    /// wins) and records every request.
    #[derive(Clone, Default)]
    pub struct MockHttp {
        routes: Arc<Mutex<Vec<Route>>>,
        pub sent: Arc<Mutex<Vec<Request>>>,
    }

    impl MockHttp {
        pub fn on(&self, method: Method, pattern: &str, status: u16, body: &str) -> &Self {
            self.routes.lock().unwrap().push((method, pattern.into(), status, body.into()));
            self
        }

        pub fn graph(&self) -> Graph {
            Graph::new(Box::new(self.clone()), Box::new(|| Ok("token".into())))
        }

        pub fn requests(&self) -> Vec<Request> {
            self.sent.lock().unwrap().clone()
        }
    }

    impl Http for MockHttp {
        fn send(&self, req: &Request) -> Result<Response> {
            self.sent.lock().unwrap().push(req.clone());
            let routes = self.routes.lock().unwrap();
            let hit = routes.iter().find(|(m, p, _, _)| *m == req.method && req.url.contains(p.as_str()));
            match hit {
                Some((_, _, status, body)) => Ok(Response { status: *status, body: body.clone() }),
                None => Ok(Response { status: 404, body: format!(r#"{{"error":{{"code":"NotMocked","message":"{}"}}}}"#, req.url) }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::MockHttp;
    use super::*;
    use crate::teams::actions::ActionItem;

    #[test]
    fn me_falls_back_to_upn() {
        let m = MockHttp::default();
        m.on(Method::Get, "/me?", 200, r#"{"id":"u1","displayName":"Anna Lehmann","mail":null,"userPrincipalName":"anna@firma.de"}"#);
        let me = m.graph().me().unwrap();
        assert_eq!(me, Me { id: "u1".into(), name: "Anna Lehmann".into(), email: "anna@firma.de".into() });
        let req = &m.requests()[0];
        assert!(req.headers.iter().any(|(k, v)| k == "Authorization" && v == "Bearer token"));
    }

    #[test]
    fn calendar_follows_pages_and_drops_cancelled() {
        let m = MockHttp::default();
        // The next-link route must come first: it also contains "calendarView".
        m.on(
            Method::Get,
            "skiptoken=2",
            200,
            r#"{"value":[{"id":"e2","subject":"Sprint review","start":{"dateTime":"2026-11-26T09:00:00.0000000","timeZone":"UTC"},"end":{"dateTime":"2026-11-26T10:00:00.0000000","timeZone":"UTC"},"isCancelled":true}]}"#,
        );
        m.on(
            Method::Get,
            "/me/calendarView",
            200,
            r#"{"value":[{"id":"e1","subject":"Standup","start":{"dateTime":"2026-11-26T08:00:00.0000000","timeZone":"UTC"},"end":{"dateTime":"2026-11-26T08:15:00.0000000","timeZone":"UTC"},"isOnlineMeeting":true,"onlineMeetingProvider":"teamsForBusiness","onlineMeeting":{"joinUrl":"https://teams.microsoft.com/l/meetup-join/abc"}}],
               "@odata.nextLink":"https://graph.microsoft.com/v1.0/me/calendarView?$skiptoken=2"}"#,
        );
        let from = "2026-11-26T00:00:00Z".parse().unwrap();
        let to = "2026-11-27T00:00:00Z".parse().unwrap();
        let events = m.graph().events(from, to).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].subject, "Standup");
        assert!(events[0].is_teams());
        let reqs = m.requests();
        assert_eq!(reqs.len(), 2);
        assert!(reqs[0].url.contains("startDateTime=2026-11-26T00%3A00%3A00Z"));
        assert!(reqs[0].headers.iter().any(|(k, v)| k == "Prefer" && v.contains("UTC")));
    }

    #[test]
    fn posts_to_the_meeting_chat() {
        let m = MockHttp::default();
        m.on(Method::Get, "/me/onlineMeetings?", 200, r#"{"value":[{"id":"MSo1","chatInfo":{"threadId":"19:meeting_abc@thread.v2"}}]}"#);
        m.on(Method::Post, "/chats/", 201, r#"{"id":"1700000000000"}"#);
        let g = m.graph();
        let event = Event { join_url: Some("https://teams.microsoft.com/l/meetup-join/x'y".into()), ..Event::default() };
        let chat = g.meeting_chat(&event).unwrap();
        assert_eq!(chat, "19:meeting_abc@thread.v2");
        assert_eq!(g.post_chat_message(&chat, "<p>Hi</p>").unwrap(), "1700000000000");
        let reqs = m.requests();
        assert!(reqs[0].url.contains("x%27%27y"), "quotes are doubled in the filter: {}", reqs[0].url);
        assert!(reqs[1].url.ends_with("/chats/19%3Ameeting_abc%40thread.v2/messages"), "{}", reqs[1].url);
        assert_eq!(reqs[1].body, Some(Body::Json(json!({"body":{"contentType":"html","content":"<p>Hi</p>"}}))));
    }

    #[test]
    fn no_chat_for_plain_events() {
        let m = MockHttp::default();
        let err = m.graph().meeting_chat(&Event::default()).unwrap_err();
        assert!(!format!("{err}").is_empty());
        assert!(m.requests().is_empty());
    }

    #[test]
    fn todo_list_is_found_or_created() {
        let m = MockHttp::default();
        m.on(Method::Get, "/me/todo/lists", 200, r#"{"value":[{"id":"L1","displayName":"Tasks"},{"id":"L2","displayName":"mostalgia"}]}"#);
        assert_eq!(m.graph().todo_list("Mostalgia").unwrap(), "L2");

        let m = MockHttp::default();
        m.on(Method::Get, "/me/todo/lists", 200, r#"{"value":[]}"#);
        m.on(Method::Post, "/me/todo/lists", 201, r#"{"id":"NEW"}"#);
        assert_eq!(m.graph().todo_list("Mostalgia").unwrap(), "NEW");
        assert_eq!(m.requests()[1].body, Some(Body::Json(json!({"displayName":"Mostalgia"}))));
    }

    #[test]
    fn tasks_carry_their_due_date() {
        let m = MockHttp::default();
        m.on(Method::Post, "/tasks", 201, r#"{"id":"T1","title":"Send the offer"}"#);
        let item = ActionItem {
            task: "Send the offer".into(),
            owner: Some("Anna".into()),
            due: Some("2026-12-01".into()),
            due_date: chrono::NaiveDate::from_ymd_opt(2026, 12, 1),
            done: false,
        };
        let t = m.graph().add_task("L1", &item, "From: Weekly").unwrap();
        assert_eq!(t.id, "T1");
        let Some(Body::Json(body)) = &m.requests()[0].body else { panic!() };
        assert_eq!(body["dueDateTime"]["dateTime"], "2026-12-01T00:00:00");
        assert_eq!(body["body"]["content"], "From: Weekly");
    }

    #[test]
    fn errors_are_readable() {
        let m = MockHttp::default();
        m.on(Method::Get, "/me?", 403, r#"{"error":{"code":"Forbidden","message":"Missing scope"}}"#);
        let err = format!("{:#}", m.graph().me().unwrap_err());
        assert!(err.contains("Forbidden") && err.contains("Missing scope"), "{err}");
    }

    #[test]
    fn transcripts_sorted_and_fetched_as_vtt() {
        let m = MockHttp::default();
        m.on(Method::Get, "/content", 200, "WEBVTT\n\n00:00:00.000 --> 00:00:02.000\n<v Anna>Hallo</v>\n");
        m.on(
            Method::Get,
            "/transcripts",
            200,
            r#"{"value":[{"id":"t2","createdDateTime":"2026-11-26T10:00:00Z"},{"id":"t1","createdDateTime":"2026-11-26T09:00:00Z"}]}"#,
        );
        let g = m.graph();
        let ts = g.transcripts("MSo1").unwrap();
        assert_eq!(ts.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["t1", "t2"]);
        assert!(g.transcript_vtt("MSo1", "t1").unwrap().starts_with("WEBVTT"));
        assert!(m.requests()[1].url.contains("%24format=text%2Fvtt") || m.requests()[1].url.contains("$format=text/vtt"));
    }
}
