## What does this change?



## Why?

Link the issue it fixes, if there is one (for example "Fixes #12").

## Checklist

- [ ] `cargo clippy --all-targets -- -D warnings` and `cargo test` pass locally
- [ ] Nothing new sends data off the server (no telemetry, no external requests, no CDNs)
- [ ] New dependencies have a permissive license (`cargo deny check` passes)
- [ ] User-facing changes are noted under a new version heading in `CHANGELOG.md`
