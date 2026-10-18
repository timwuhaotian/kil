# Security policy

## Supported versions

Only the latest release, [`Kil-arm64.dmg`](https://github.com/timwuhaotian/kil/raw/main/releases/Kil-arm64.dmg)
and `main`, gets security fixes.

## Reporting a vulnerability

Please **don't open a public issue.** Report it privately through GitHub:
[**Report a vulnerability**](https://github.com/timwuhaotian/kil/security/advisories/new).

Include what an attacker could do, the steps to reproduce it, and the macOS version and Kil
build. You'll get a reply within a week. Once a fix is released, the advisory is published
with credit to you, unless you'd rather not be named.

## Scope

Kil runs your shell with your privileges and executes whatever you type; that is not a
vulnerability. These are, for example:

- Output from a program (escape sequences, hyperlinks, titles) that makes Kil run commands,
  write files or leak data without the user asking.
- The session server's socket in `~/Library/Application Support/kil/` accepting another user,
  or anything that lets one user reach another's panes.
- A release DMG or app whose signature or notarization doesn't hold.
