#!/usr/bin/env python3
"""A stand-in for the Microsoft sign-in and Graph, for trying Mostalgia's Teams
integration without an account. Usage: fake-microsoft.py <port> <request-log>
(see docs/teams-integration.md)."""
import json, http.server, sys, urllib.parse, datetime as dt
PORT = int(sys.argv[1]); LOG = sys.argv[2]
ROOT = f"http://127.0.0.1:{PORT}/v1.0"
now = dt.datetime.now(dt.timezone.utc).replace(second=0, microsecond=0)
def iso(t): return t.strftime("%Y-%m-%dT%H:%M:%S.0000000")
def z(t): return t.strftime("%Y-%m-%dT%H:%M:%SZ")
# Meeting 2 (imported) happens at 2026-11-26 10:00 local (UTC+0 here) - chat timed around it.
M2 = dt.datetime(2026, 11, 26, 10, 0, tzinfo=dt.timezone.utc)
def event(eid, subject, start, mins=30, series="S1", join="https://teams.microsoft.com/l/meetup-join/weekly"):
    return {"id": eid, "subject": subject, "type": "occurrence", "seriesMasterId": series,
            "start": {"dateTime": iso(start), "timeZone": "UTC"}, "end": {"dateTime": iso(start + dt.timedelta(minutes=mins)), "timeZone": "UTC"},
            "organizer": {"emailAddress": {"name": "Anna Lehmann", "address": "anna@contoso.com"}},
            "attendees": [{"type": "required", "status": {"response": "accepted"}, "emailAddress": {"name": "Anna Lehmann", "address": "anna@contoso.com"}},
                          {"type": "required", "status": {"response": "accepted"}, "emailAddress": {"name": "Mosta Ahmed", "address": "mosta@contoso.com"}},
                          {"type": "resource", "emailAddress": {"name": "Room 4", "address": "room4@contoso.com"}}],
            "isOnlineMeeting": True, "onlineMeetingProvider": "teamsForBusiness", "onlineMeeting": {"joinUrl": join},
            "body": {"contentType": "text", "content": "Agenda: budget, legal review, the Q4 deck.\n________________\nMicrosoft Teams meeting\nJoin..."},
            "webLink": "https://outlook/x", "isCancelled": False, "location": {"displayName": "Microsoft Teams Meeting"}}
EVENTS = [event("occ-19", "Weekly sync", dt.datetime(2026, 11, 19, 10, 0, tzinfo=dt.timezone.utc)),
          event("occ-26", "Weekly sync", M2),
          event("occ-now", "Weekly sync", now - dt.timedelta(minutes=5)),
          event("occ-soon", "Design review", now + dt.timedelta(minutes=8), series="S2", join="https://teams.microsoft.com/l/meetup-join/design")]
MSGS = [
  {"id": "c3", "messageType": "message", "createdDateTime": z(M2 + dt.timedelta(minutes=12)), "from": {"user": {"displayName": "Anna Lehmann"}},
   "body": {"contentType": "html", "content": "<p>Decision: we go with <b>option B</b></p>"}},
  {"id": "c2", "messageType": "message", "createdDateTime": z(M2 + dt.timedelta(minutes=4)), "from": {"user": {"displayName": "Mosta Ahmed"}},
   "body": {"contentType": "html", "content": "<p>Deck: <a href=\"https://contoso.sharepoint.com/q4.pptx\">Q4 deck</a></p>"}},
  {"id": "c1", "messageType": "systemEventMessage", "createdDateTime": z(M2 + dt.timedelta(minutes=1)), "body": {"contentType": "html", "content": "<systemEventMessage/>"}},
  {"id": "c0", "messageType": "message", "createdDateTime": z(M2 - dt.timedelta(days=7)), "from": {"user": {"displayName": "Anna Lehmann"}},
   "body": {"contentType": "text", "content": "last week's chat"}},
]
VTT = "WEBVTT\n\n00:00:01.000 --> 00:00:05.000\n<v Anna Lehmann>Legal said yes, budget is approved.</v>\n\n00:00:06.000 --> 00:00:10.000\n<v Mosta Ahmed>Great, I posted the deck in the chat.</v>\n"
LONG_REFRESH = "R" * 3000
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def log(self, body=""):
        open(LOG, "a").write(json.dumps({"m": self.command, "p": self.path, "auth": self.headers.get("Authorization", ""), "body": body}) + "\n")
    def send(self, obj, code=200, ctype="application/json"):
        b = (obj if isinstance(obj, str) else json.dumps(obj)).encode()
        self.send_response(code); self.send_header("Content-Type", ctype); self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        self.log(); u = urllib.parse.urlparse(self.path); q = urllib.parse.parse_qs(u.query); p = u.path
        if p.endswith("/oauth2/v2.0/authorize"):
            loc = q["redirect_uri"][0] + "?code=CODE123&state=" + urllib.parse.quote(q["state"][0])
            self.send_response(302); self.send_header("Location", loc); self.end_headers(); return
        if p == "/v1.0/me": return self.send({"id": "me", "displayName": "Mosta Ahmed", "mail": "mosta@contoso.com"})
        if p == "/v1.0/me/calendarView":
            s = dt.datetime.fromisoformat(q["startDateTime"][0].replace("Z", "+00:00")); e = dt.datetime.fromisoformat(q["endDateTime"][0].replace("Z", "+00:00"))
            evs = [x for x in EVENTS if s <= dt.datetime.fromisoformat(x["start"]["dateTime"][:19] + "+00:00") < e]
            return self.send({"value": evs})
        if p == "/v1.0/me/onlineMeetings":
            return self.send({"value": [{"id": "OM-weekly", "chatInfo": {"threadId": "19:meeting_weekly@thread.v2"}}]})
        if p.startswith("/v1.0/chats/") and p.endswith("/messages"):
            return self.send({"value": MSGS})
        if p.endswith("/transcripts"): return self.send({"value": [{"id": "T1", "createdDateTime": z(M2 + dt.timedelta(minutes=31))}]})
        if p.endswith("/content"): return self.send(VTT, ctype="text/vtt")
        if p == "/v1.0/me/todo/lists": return self.send({"value": [{"id": "L1", "displayName": "Tasks"}]})
        self.send({"error": {"code": "NotFound", "message": p}}, 404)
    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0)); body = self.rfile.read(n).decode(); self.log(body); p = urllib.parse.urlparse(self.path).path
        if p.endswith("/oauth2/v2.0/token"):
            f = urllib.parse.parse_qs(body)
            if f["grant_type"][0] == "authorization_code":
                assert f["code"][0] == "CODE123" and f.get("code_verifier"), f
            else:
                assert f["refresh_token"][0] == LONG_REFRESH, "refresh token came back wrong from the keyring"
            return self.send({"access_token": "AT-" + f["grant_type"][0], "refresh_token": LONG_REFRESH, "expires_in": 3600})
        if p.startswith("/v1.0/chats/"): return self.send({"id": "posted-1"}, 201)
        if p == "/v1.0/me/todo/lists": return self.send({"id": "L2", "displayName": json.loads(body)["displayName"]}, 201)
        if p.endswith("/tasks"): return self.send({"id": "T" + str(len(body))}, 201)
        self.send({"error": {"code": "NotFound"}}, 404)
http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
