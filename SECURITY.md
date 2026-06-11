# Security Policy

## Supported versions

Pre-1.0 there are no backports: fixes ship in the next release.

| Version | Supported |
| --- | --- |
| Latest 0.x release (currently 0.2.0) | yes |
| Anything older | no |

## Reporting a vulnerability

Report privately — **do not open a public issue**.

- Preferred: [GitHub private vulnerability reporting](https://github.com/OWNER/prun/security/advisories/new)
  <!-- TODO: replace OWNER -->
- Fallback: `security@example.com` <!-- TODO: replace with a real inbox -->

Include a repro if you can — a directory layout plus the command or click path
is ideal — and your platform. You'll get an acknowledgement within **7 days**
and a fix or a concrete plan within **30 days**.

## Scope

Prun deletes directories. Anything that makes it delete the **wrong** directory
is the worst-case bug and is explicitly in scope, highest priority:

- **Escaping the scanned root** — scan or `clean` touching paths outside the
  root the user pointed it at (`..` tricks, crafted names, canonicalization
  gaps).
- **Tricking the `Reclaimable` guard** — `clean` must refuse any path a scan
  never surfaced; any bypass counts, even one that assumes a compromised
  webview.
- **Junction / symlink traversal** — reparse points or symlinks that redirect
  sizing or deletion outside the matched artifact tree.
- **Webview → IPC escalation** — script running in the UI (e.g. via
  ruleset-controlled strings, as in the 0.2.0 XSS-to-delete fix) reaching the
  Tauri commands with attacker-chosen arguments.

Also in scope: the updater/signing path, and dependency advisories with a
demonstrated practical impact on the above.

Out of scope: attacks that require already running arbitrary code as the user,
and advisories with no concrete path into Prun's behavior.

## No bounty

There is no bounty program. Good reports get a prompt fix and credit in the
changelog if you want it.
