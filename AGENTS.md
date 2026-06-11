# Repository Guidelines

## Current Project State
- This repository currently ships a Windows port of CodexBar implemented in Rust (`rust/`).
- Many files in `docs/` and some workflows reference the upstream macOS/Swift project. Treat those as historical or
  upstream-sync material unless the task is explicitly about upstream parity.
- When repo docs conflict, trust the active Rust sources in `rust/src` and the Rust manifests (`rust/Cargo.toml`).

## Project Structure & Modules
- `rust/src`: Main application code (CLI, providers, tray, native UI, browser cookie extraction, settings).
- `rust/src/providers`: Provider-specific fetch/parsing/auth logic. Keep provider boundaries clean.
- `rust/src/native_ui` and `rust/src/tray`: egui UI and tray integration.
- `rust/src/browser`: Browser detection + cookie extraction for Windows.
- `rust/assets`, `rust/icons`, `rust/gen`, `rust/wix`: UI assets, generated schemas, installer packaging.
- `docs`: Mixed documentation (Windows port docs plus upstream/macOS references). Update only the relevant docs.

## Build, Test, Run
- Work from `rust/` for most tasks: `cd rust`.
- Build: `cargo build` (debug) or `cargo build --release`.
- Test: `cargo test`.
- Run CLI locally: `cargo run -- --help`, `cargo run -- -p claude`, `cargo run -- cost`.
- Run the tray app (Windows): `cargo run -- menubar`.
- Format/lint before handoff when code changed: `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings`
  (or explain why not run).
- There is no active root-level `Scripts/` build pipeline in this port. Do not rely on legacy `Scripts/*.sh` commands.

## Coding Style & Naming
- Prefer small, typed structs/enums and focused modules; keep changes local.
- Keep provider-specific logic inside the provider module instead of adding cross-provider branching.
- Preserve clear error handling and user-facing diagnostics (`anyhow`/`thiserror` + friendly messages where applicable).
- Use `tracing` for diagnostics; do not log raw secrets, cookies, or tokens.
- Avoid adding dependencies/tooling without confirmation.

## Testing Guidelines
- Add or extend focused Rust tests near the changed module (`#[cfg(test)]` unit tests are common in this repo).
- For parser/fetcher changes, add deterministic samples/fixtures where practical.
- Run `cargo test` after code changes; include any skipped checks in handoff.
- If UI/tray behavior changed, do a manual Windows validation when possible (`codexbar menubar`).

## Commit & PR Guidelines
- Use short imperative commit messages (for example: `Fix Claude CLI parser`, `Improve cookie import errors`).
- Keep commits scoped to one change.
- In PRs/patches, include:
  - Summary of behavior changes
  - Commands run (`cargo test`, `cargo fmt`, etc.)
  - Screenshots/GIFs for UI changes (Windows)
  - Linked issue/reference when relevant

## Agent Notes
- Active implementation is the Rust Windows port. Root Swift/macOS docs and scripts are not the default workflow here.
- Keep provider data siloed: never show identity/plan/email fields from provider A in provider B UI.
- Claude CLI output is user-configurable; do not depend on a customizable status line for usage parsing.
- Cookie import UX uses explicit browser selection in Preferences. Do not assume Chrome-only in general UI flows.
- Be conservative with secret handling (manual cookies, API keys, token accounts); use existing redaction/storage helpers.
- Prefer Windows-native validation for tray/DPAPI/browser-cookie behavior; WSL/Linux can be insufficient for those paths.

## Persistence & Runtime Paths

| What | Path |
|------|------|
| Settings | `%APPDATA%\CodexBar\settings.json` |
| Claude OAuth credentials | `~\.claude\.credentials.json` → key `claudeAiOauth` |
| Usage logs / history | `data/` in repo root |
| Active binary | `rust/target/x86_64-pc-windows-gnu/release/codexbar.exe` |

The `codexbar.exe` in the repo root is a pre-built snapshot (March 2024) — **not** the active binary.

## Build & Deploy

```powershell
# Always stop the app first — the release binary is file-locked while running
Get-Process codexbar -ErrorAction SilentlyContinue | Stop-Process -Force

cd rust
cargo build           # debug (~3 min)
cargo build --release # release (~4 min)
```

Launch: run the binary directly from Explorer or Autostart — **not** via `Start-Process` from a tool session, otherwise the tray icon won't close cleanly.

## Key Structs

| Struct | Purpose |
|--------|---------|
| `ProviderData` | Per-provider UI state (`session_percent`, `weekly_percent`, `session_reset`, …) |
| `UsageSnapshot` | Raw fetch output; holds primary / secondary / model_specific `RateWindow` |
| `RateWindow` | `used_percent`, `window_minutes`, `resets_at (DateTime<Utc>)`, `reset_description` |
| `FetchContext` | Input to `Provider::fetch_usage` (`source_mode`, cookies, api_key) |
| `ProviderFetchResult` | Output: `UsageSnapshot` + `source_label` + optional `CostSnapshot` |

## Claude Provider

**Files:** `rust/src/providers/claude/`

**Source fallback chain (Auto mode):** OAuth → Web (browser cookies) → CLI

### OAuth endpoint (correct)
```
GET https://api.anthropic.com/api/oauth/usage
Authorization: Bearer <accessToken from ~/.claude/.credentials.json>
```
Response is **snake_case** with `utilization` already a percentage (0–100):
```json
{ "five_hour": {"utilization": 28.0, "resets_at": "…"}, "seven_day": {…} }
```
`api.claude.ai` does **not** resolve via DNS — always use `api.anthropic.com`.

### Token refresh (`oauth.rs`)
The provider is **OAuth-only** in this build (`supports_web`/`supports_cli` are `false`,
`SourceMode::Web`/`Cli` return `UnsupportedSource`), so there is no fallback — the OAuth
token must stay valid. `fetch()` auto-refreshes:
- If the access token is expired (or the API returns 401) and a `refreshToken` exists,
  it POSTs to the token endpoint, persists the rotated tokens, and retries.
- Token endpoint: `POST https://console.anthropic.com/v1/oauth/token`
- Client id (public, same as `claude` CLI): `9d1c250a-e61b-44d9-88ed-5944d1962f5e`
- Body: `{ grant_type: "refresh_token", refresh_token, client_id }`
- Response: `{ access_token, refresh_token (rotated!), expires_in }`
- **Refresh tokens rotate** — the new one must be written back or the next refresh fails.
- Write-back is surgical (only `accessToken`/`refreshToken`/`expiresAt` in the
  `claudeAiOauth` object) and atomic (temp file + rename), preserving `subscriptionType`,
  `mcpOAuth`, etc. and avoiding corruption if the `claude` CLI reads concurrently.

### Known fixes already applied
- `oauth.rs`: wrong host (`api.claude.ai`) → `api.anthropic.com/api/oauth/usage`
- `oauth.rs`: structs were camelCase (`fiveHour`, `resetsAt`) → corrected to snake_case
- `oauth.rs`: added automatic OAuth token refresh (was read-only → errored on expiry)
- `app.rs` (`refresh_providers`): provider list was replaced with 0%-placeholders on every refresh → now only rebuilt when the enabled-provider set changes; individual slots update in-place

## Refresh Logic (`native_ui/app.rs :: refresh_providers`)

- Spawns a thread → Tokio runtime → all providers fetched in parallel (`tokio::spawn`)
- Each completed task writes directly into `s.providers[idx]` (in-place, no flash)
- Default interval: 300 s; configurable in Settings
- Reset times shown relative by default (`reset_time_relative: true`): format `3h 9m` / `5d 21h`

## Git Remotes

```
origin    https://github.com/BlumDev/Win-CodexBar.git   ← your fork
upstream  https://github.com/Finesssee/Win-CodexBar.git ← upstream
```
