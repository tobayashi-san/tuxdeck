# Official Stream Deck SDK libraries (test fixtures)

The e2e and UI tests run plugins and a property inspector built on Elgato's own
libraries, so TuxDeck is checked against what real plugins use, not only against our
reading of the protocol.

| File | What |
|---|---|
| `plugin-src.mjs` | the test plugin (source) |
| `plugin-v1.mjs` | bundled with `@elgato/streamdeck` 1.4.1 |
| `plugin-v2.mjs` | bundled with `@elgato/streamdeck` 2.1.2 |
| `plugin-v3.mjs` | bundled with `@elgato/streamdeck` 3.0.4 |
| `sdpi-components.js` | `sdpi-components` 4.0.1 (property inspector components) |

Regenerate (needs network, npm and esbuild): `sh tests/fixtures/elgato-sdk/build.sh`.
Licences: [LICENSES.md](LICENSES.md). These files are used by the tests only; they are
not part of TuxDeck.
