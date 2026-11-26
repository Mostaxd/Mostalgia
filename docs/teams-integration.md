# Microsoft Teams integration

Mostalgia can connect to a Microsoft 365 account (Microsoft Graph) to know
which Teams meeting you are in, and to send results back to Teams. Everything
is optional, off until you sign in, and talks to Microsoft only with your own
delegated token: there is no Mostalgia server in between.

## Features

| # | Feature | Graph permission | Account type |
|---|---------|------------------|--------------|
| 1 | Sign in with Microsoft (OAuth 2.0 auth code + PKCE, loopback redirect, refresh token in the system keyring) | `User.Read`, `offline_access` | work + personal |
| 2 | Calendar sync: upcoming Teams meetings, recordings named after the event (recording still starts only when call detection sees you join) | `Calendars.Read` | work + personal |
| 3 | Attendee names and invite terms as Whisper hints; the event's language as a language hint; attendees as speaker-name suggestions, 1:1 calls named automatically | (from 2) | work + personal |
| 4 | Post the summary and action items to the meeting chat | `OnlineMeetings.Read`, `ChatMessage.Send` | work/school |
| 5 | Fetch the Teams transcript (when the meeting had transcription on) and suggest speaker names from it | `OnlineMeetingTranscript.Read.All` (admin consent) | work/school |
| 6 | Pre-meeting brief: agenda from the invite, past meetings with the same people and their open action items | (from 2) | work + personal |
| 7 | Action items to Microsoft To Do (a "Mostalgia" list) | `Tasks.ReadWrite` | work + personal |
| 8 | Meeting chat: what was typed during the call (links, files, decisions) goes into the summary and Ask | `Chat.Read` | work/school |
| 9 | Recurring meetings: meetings of the same series are linked; the summary ends with "Since last time" (what changed against the previous summary) | (from 2) | work + personal |

Teams transcripts (5) need a tenant admin to consent to
`OnlineMeetingTranscript.Read.All`, so they have their own switch and the
scope is only requested when it is on.

## Registering the app in Microsoft Entra ID

Mostalgia is a public client (a desktop app can't keep a secret), so the
registration has no client secret.

1. Open <https://entra.microsoft.com> → **Identity → Applications → App
   registrations → New registration**.
2. Name: `Mostalgia`. Supported account types: **Accounts in any
   organizational directory and personal Microsoft accounts** (or only your
   organization, then use its tenant id below).
3. Redirect URI: platform **Public client/native (mobile & desktop)**, URI
   `http://localhost`. Entra accepts any port for `http://localhost` on native
   apps; Mostalgia listens on a free port for the one sign-in.
4. Register. Copy the **Application (client) ID**.
5. **Authentication** → *Advanced settings* → **Allow public client flows: Yes**.
6. **API permissions → Add a permission → Microsoft Graph → Delegated**:
   `User.Read`, `offline_access`, `Calendars.Read`, `OnlineMeetings.Read`,
   `ChatMessage.Send`, `Chat.Read`, `Tasks.ReadWrite`, and (optional, admin consent)
   `OnlineMeetingTranscript.Read.All`. For a work tenant, an admin can press
   **Grant admin consent** so users aren't asked.
7. In Mostalgia: **Settings → Microsoft Teams**, paste the client ID, leave
   the tenant at `common` (or your tenant id / domain), press **Sign in with
   Microsoft**. The CLI works too: `mostalgia teams login`.

The client ID can also come from the environment: `MOSTALGIA_MS_CLIENT_ID`
and `MOSTALGIA_MS_TENANT` win over the settings.

## How it works

- `mostalgia-core/src/teams/`: `auth` (PKCE, loopback, token refresh, keyring
  storage split into chunks so long refresh tokens fit the Windows credential
  store), `graph` (Graph calls behind an `Http` trait, mocked in tests),
  `calendar` (events, matching a recording to the meeting it belongs to),
  `link` (the event a meeting belongs to, saved as `teams.json` in the
  meeting's folder, so the library schema is unchanged; it also keeps the
  meeting chat), `chat` (chat messages as text, the window around the
  recording), `vtt` (Teams
  transcripts and speaker suggestions), `actions` (action items from the
  summary, Markdown to Teams HTML), `brief` (pre-meeting brief).
- Nothing is sent to Microsoft unless you press a button (or turn on the
  automatic post). The calendar is read every few minutes while you're signed in
  and calendar sync is on.

## Meeting chat and recurring series

- Before a summary is written for a Teams meeting, Mostalgia reads the
  meeting chat from 15 minutes before the meeting (or recording) to 30
  minutes after it ended, leaving out system messages, deleted messages and
  its own posted summary. Only the messages in that window are kept (a
  recurring meeting shares one chat across all its occurrences). They go to
  the summary model in a `<meeting_chat>` block, timed like the transcript,
  and into the Ask index as `chat` passages. Teams menu → **Read the meeting
  chat** does it by hand; `mostalgia teams chat <id>` too.
- Meetings belong to the same series when Graph gives them the same
  `seriesMasterId`, or (a series set up again, or a meeting linked before
  this existed) the same subject without numbers and the same organizer.
  The summary gets the last earlier summary of the series and is asked for
  a closing "Since last time" section. The Teams menu links to the last and
  next recording of the series; `mostalgia teams series <id>` lists them.
- Both have switches (Settings → Microsoft Teams, `teams setup --chat`,
  `--series`). Turning on the chat after signing in needs a new sign-in for
  the `Chat.Read` permission.
