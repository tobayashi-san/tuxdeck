# Security

TuxDeck runs as a background service with a local HTTP API and a plugin interface
(on `127.0.0.1` only). The API can install plugins and thus run programs, so bugs in the
Host/Origin check, the API token check, the plugin host or package extraction are
security relevant. The plugin WebSocket (SDK) is not token-protected, like in the official
app: plugins get their connection parameters on the command line.

## Reporting a vulnerability

Please **do not open a public issue**. Report it privately via
["Report a vulnerability"](https://github.com/tobayashi-san/tuxdeck/security/advisories/new)
in the repository's "Security" tab. Helpful details: affected version, steps to reproduce
and the possible impact.

You will usually get a reply within a week. Only the latest version is supported.
