# Security policy

Ground Station's daemon (`gsd`) stores prompts, tool output, shell commands and file paths from your agent sessions. Anything that lets that data leave the machine, reach another local user, or bypass redaction is a security issue. Please report it privately.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting: [github.com/groundstationhq/groundstation/security/advisories/new](https://github.com/groundstationhq/groundstation/security/advisories/new). Don't open a public issue for security problems.

Include what you can: the version (`groundstation --version`), platform, the configuration in effect (`groundstation config` masks the token), and steps to reproduce. A proof of concept helps but isn't required.

You'll get an acknowledgement within 3 business days and a fix or a plan within 30 days for confirmed issues. We'll credit you in the release notes unless you'd rather not be named.

## Scope

In scope:

- `gsd` and the `groundstation` CLI, including the hook and plugin files they install into agent configs.
- The redaction pipeline (`[redaction]` in `config.toml`): secrets, environment values, excluded fields and path hashing.
- The local HTTP API on `127.0.0.1:4318` and the embedded UI.
- `install.sh` and the release workflows.

Out of scope: vulnerabilities in the agents themselves (Claude Code, Codex, OpenCode, pi) or in their transcript formats, unless Ground Station mishandles them.

## Supported versions

Ground Station is pre-alpha. Only the latest release receives fixes.

## What the daemon does by default

So expectations are clear:

- Listens on loopback only and rejects requests whose `Host` header isn't a loopback name.
- Keeps every byte on the machine (`transport.mode = "local-only"`) unless you configure a cloud endpoint.
- Redacts well-known credential formats and `*_KEY`, `*_TOKEN`, `*_SECRET` and `DATABASE_URL` values before anything is written to disk.
- Creates its data directory, database, spool and log readable by your user only.
