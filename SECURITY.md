# Security policy

Mostalgia stores meeting recordings, transcripts and API keys, so security
reports are taken seriously. Thank you for helping keep its users safe.

## Supported versions

Security fixes go into the latest release (and `main`). Older versions don't
get separate patches; please update to the latest release.

## Reporting a vulnerability

**Please don't open a public issue for security problems.** Report them
privately instead, in either of these ways:

- GitHub's private reporting: open the repository's
  [Security tab](https://github.com/Mostaxd/Mostalgia/security) and click
  **Report a vulnerability**.
- Email: mostafa152804@gmail.com

Helpful details to include:

- what the problem is and what an attacker could do with it,
- steps to reproduce, or a proof of concept,
- the Mostalgia version or commit, your distribution, and which transcription
  and AI providers were involved.

## What to expect

- A reply within a week, confirming the report and the next steps.
- Updates while a fix is prepared, and a coordinated date before anything is
  made public.
- Credit in the release notes and the security advisory once it's fixed,
  unless you'd rather stay anonymous.

## Scope

In scope: the code in this repository, including the desktop app, the
`mostalgia` CLI, how recordings, transcripts and API keys are stored, and what
is sent to transcription and AI providers.

Out of scope: vulnerabilities in third-party providers or upstream libraries
themselves (please report those to their maintainers; we'll still update
Mostalgia once a fix exists), and issues that need an attacker to already have
full access to your user account.
