## What this changes

<!-- What the change does and why. Link the issue: Fixes #123 -->

## How it was tested

<!-- Commands run, and the platforms you ran it on (Windows, macOS, browsers). -->

- [ ] `go test ./internal/...`
- [ ] `cargo test --workspace --locked`
- [ ] `node --test tests/unit/*.test.mjs tests/contract/*.test.mjs`
- [ ] End-to-end areas run, if the change affects them: <!-- e.g. session, clipboard -->

## Checklist

- [ ] Replaced code, messages, docs and tests are deleted, not left beside the new ones
- [ ] Interface text is in all three catalogs (`en`, `zh-Hans`, `zh-Hant`)
- [ ] Protocol changes are in `proto/`, with the version bumped if compatibility breaks
- [ ] User-facing docs are updated
