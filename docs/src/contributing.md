# Contributing

## Build and test

You need Rust 1.99.1 (the workspace pins it).

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo deny check
bash scripts/test-next-version.sh
```

The tests run against an in-memory fake of the FreeAgent API (`crates/mcp-freeagent-books/tests/support/fake_freeagent.rs`) with a real token endpoint, expiring access tokens and optionally rotating refresh tokens. They cover the whole sign-in through FreeAgent and MCP sessions through the OAuth and bearer guards, so no FreeAgent account is needed. To try changes against real books, run the server with `FREEAGENT_ENVIRONMENT=sandbox` and a [sandbox company](https://signup.sandbox.freeagent.com/signup).

## Adding or changing a tool

Tools live in `crates/mcp-freeagent-books/src/tools/`, one module per area. A test checks every tool has a description and the right annotations: names starting `get_`, `list_` or `search_`, `business_snapshot` and `api_get` must be read-only, and every `delete_` tool, `mark_tax_return` and `api_request` must be destructive. Tool names must stay short enough for the `freeagent_` prefix on `/mcp`.

Document a new tool on its page under `docs/src/tools/`, and a new variable in `docs/src/configuration.md`.

## Prompts and the plugin

Prompts are in `crates/mcp-freeagent-books/src/prompts.rs`. The [Claude Code plugin](claude-code-plugin.md)'s command files are generated from them; after changing a prompt, run:

```bash
UPDATE_PLUGIN=1 cargo test -p mcp-freeagent-books plugin
```

A test fails if the command files and the prompts drift apart, and another checks that prompts name only tools that exist.

## This documentation

The site is built with [mdBook](https://rust-lang.github.io/mdBook/) from `docs/`. To preview it:

```bash
cargo install --locked mdbook@0.5.4 mdbook-mermaid@0.17.1 lychee@0.24.2
bash scripts/build-docs.sh
mdbook serve docs --open
```

`scripts/build-docs.sh` builds the book and checks every internal link and anchor.

## Pull requests

- Branch from `develop` as `feature/<name>`, `bugfix/<name>` or `hotfix/<name>`, and open the pull request against `develop`.
- Use conventional commit messages (`feat:`, `fix:`, `docs:`, `test:`, `chore:`, `refactor:`).
- Test data must be synthetic: never copy real books, contacts or bank transactions into fixtures.
- CI runs fmt, clippy, tests, cargo-deny, a Docker build, the script tests and the documentation build.
