# Contributing to Minifold

Thanks for helping. Minifold is maintained by one person, so a few rules keep it manageable.

## Before you start

- **Bugs:** open an issue with the bug report form first.
- **New features or bigger changes:** open a feature request and wait for a go-ahead before writing code,
  so your work isn't wasted on something that won't be merged.
- **Security problems:** report them privately, see [SECURITY.md](SECURITY.md).

## Ground rules

- **Privacy comes first.** Nothing may send data off the server: no telemetry, no update checks, no CDNs or web fonts.
- **Keep it simple.** Small, focused pull requests are reviewed faster than large ones.
- **Licenses:** new dependencies must have a permissive license that passes `cargo deny check`.
  By contributing you agree your work is released under the [MIT License](LICENSE).

## Pull requests

1. Fork the repo and create a branch from `main`.
2. Make your change, and add a test for new logic.
3. Run `cargo clippy --all-targets -- -D warnings` and `cargo test` (the programs they need are listed under "Develop" in the README).
4. Open a pull request and fill in the template.

The "PR checks" workflow runs clippy, the tests and the license checks on your pull request
(a first-time contributor's run waits for the maintainer's approval). Only the maintainer merges.
