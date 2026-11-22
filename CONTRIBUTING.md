# Contributing to Deskway

## Before you start

- For a bug, open an issue with the bug report template.
- For a feature or a larger change, open an issue first and describe the problem it solves, so
  the approach can be agreed before you write code.
- For a security problem, don't open an issue. Follow [SECURITY.md](SECURITY.md) instead.

## Setting up

You need Go 1.26 or later, Rust (stable) and Node.js 22 or later. The host and the `deskway` CLI
build only on Windows and macOS. The server and the web client build anywhere. To run the tests:

```sh
go test ./internal/...
cargo test --workspace --locked
node --test tests/unit/*.test.mjs tests/contract/*.test.mjs
```

`scripts/try-local.ps1` runs a server and a host on one Windows computer. The macOS app is built
with `scripts/build-mac-host.sh --install`. The end-to-end tests are described at the top of
`tests/e2e/run.mjs`.

Read [Architecture](docs/architecture.md) before changing code. It names the module that owns
each piece of state and sets out the rules every change follows.

## What a change must include

- Put the change where it belongs. Each piece of state has one owner, as described in the
  architecture document. Reuse existing modules and helpers instead of adding parallel ones. When
  you replace a mechanism, delete the old one completely, including its code, messages, docs and
  tests.
- Add tests at the one layer where the behavior lives. Extending an existing table or test helper
  is better than writing a new one.
- Make protocol changes in `proto/` (`messages.json`, `tools.json`, `vectors.json`), and bump the
  version when a change breaks compatibility. The contract tests check every side against these
  files.
- Add interface text to all three catalogs (`en`, `zh-Hans`, `zh-Hant`) in `web/static/i18n/` or
  `host/i18n/`, using the terms in [Translations](docs/i18n.md). Code, comments and docs are
  written in English, and `tests/unit/i18n.test.mjs` checks the web and host code for Chinese
  text.
- If the change affects behavior that users or self-hosters see, update the docs in the same pull
  request.

## Pull requests

- Keep each pull request to one change, with a title that says what it does.
- Fill in the pull request template, including how you tested the change and on which platforms.
  Changes to `host/src/platform/` must be tested on the platform they touch.
- The **Tests** workflow runs on every pull request. It runs the Go and browser tests on Linux and
  the Rust tests on Windows and macOS, and checks that the host and the `deskway` CLI build for
  Intel macOS. It must pass before a pull request is merged.

## License

Deskway is licensed under the [AGPL-3.0](LICENSE). By contributing, you agree that your
contribution is licensed under the same terms.
