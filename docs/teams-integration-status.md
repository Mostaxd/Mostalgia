# Teams integration: status (night of 2026-11-26)

Branch `feature/teams-integration`, local only (not pushed, no CI run).

## What's built

| Feature | Core | CLI | App |
|---|---|---|---|
| Sign in with Microsoft (auth code + PKCE, localhost redirect, refresh token chunked in the keyring) | ✓ | `teams login/logout/status/setup` | Settings → Microsoft Teams |
| Client ID / tenant configurable, placeholder until set, env overrides | ✓ | `teams setup --client-id …` | Settings |
| Calendar: upcoming Teams meetings | ✓ | `teams upcoming` | Settings list, "next meeting" card on New recording |
| Recording linked to the running meeting (`teams.json` in its folder) | ✓ | on `start` | on start |
| Recording named after the meeting, summary keeps the name | ✓ | ✓ | ✓ |
| Recording starts only when call detection sees you join (the calendar never starts one) | | | ✓ |
| Attendee names + invite words as Whisper hints (before the glossary) | ✓ | ✓ | ✓ |
| Invite language narrows language detection (only with "auto" and no meeting languages set) | ✓ | | ✓ |
| One-to-one call: "Others" gets the other person's name | ✓ | ✓ | ✓ |
| Attendees offered as names in the speaker menu | | | ✓ |
| Post summary (+ action items) to the meeting chat, manual or automatic | ✓ | `teams post` | Teams menu, setting |
| Action items to Microsoft To Do (each once, optionally only mine) | ✓ | `teams todo` | Teams menu |
| Teams transcript → speaker-name suggestions (text-aligned offset) | ✓ | `teams transcript --apply` | Teams menu |
| Import a meeting nobody recorded from its Teams transcript | ✓ | `teams import` | — |
| Pre-meeting brief: agenda, people, last meetings with them, still-open action items | ✓ | `teams brief` | notification N min before + Brief button |
| Link a meeting recorded earlier to its calendar event | ✓ | `teams link` | Teams menu |

All Graph traffic goes through an `Http` trait; tests use canned Graph
answers (no real account needed). 50 Teams tests in `mostalgia-core`.

## What Mosta needs to do

1. Register the app in Entra ID (steps in `docs/teams-integration.md`),
   paste the client ID in Settings → Microsoft Teams, press **Sign in**.
2. Try it: a Teams meeting in the calendar, start recording during it,
   check the name, the "Others" name in a 1:1, and the Teams menu.
3. Personal Microsoft accounts: calendar, brief and To Do work; chat
   posting and transcripts need a work/school account.
4. Teams transcripts need admin consent for
   `OnlineMeetingTranscript.Read.All`; turn the switch on, sign in again.

## Not tested for real

- Nothing has talked to Microsoft yet (no registration exists). The flows
  follow the documented endpoints; expect small fixes on first contact
  (scopes, the `JoinWebUrl` filter, chat permissions in some tenants).
- The app UI compiles; screenshots were made headless (see below), but
  nobody clicked through it yet.

## Ideas not done

- An LLM-written brief (today it's assembled from the library, no AI call).
- Import Teams transcripts from the app (CLI only so far).
- Speaker-count hint for diarization from the attendee list (no-shows
  would over-split, so left out).
