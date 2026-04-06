# Upstream Update Strategy

This document is the single source of truth for how the `BlumDev/Win-CodexBar` fork evaluates, records, and integrates upstream changes from `Finesssee/Win-CodexBar`.

## Goals

- Avoid re-evaluating the same upstream release multiple times.
- Keep a durable log of what we reviewed, what we adopted, and what we intentionally skipped.
- Reduce merge risk by preferring small, well-scoped fork commits and selective upstream intake.
- Make it obvious when we should cherry-pick, rebase, merge, or deliberately ignore a release.

## Current Repository Model

- `origin`: `BlumDev/Win-CodexBar`
- `upstream`: `Finesssee/Win-CodexBar`
- Local development happens in the fork first.
- In-app updater banners are treated as review prompts, not as approval to install upstream binaries.

## Working Rules

### 1. Never use the in-app updater to overwrite the fork build

The app may notify about upstream releases, but we do not install them directly through the built-in updater for this fork.

Reason:
- the upstream installer or binary can overwrite local fork-specific behavior
- source control history would not reflect what changed
- conflict resolution becomes harder after a blind binary update

### 2. Evaluate upstream releases before integrating them

For every upstream release we check:
- what changed
- whether it matters for this fork
- whether the change overlaps heavily with our modified files
- whether the change is better cherry-picked, merged, or ignored

### 3. Prefer selective intake over broad merges

Default preference order:
1. cherry-pick isolated upstream commits
2. manual reimplementation of a small upstream fix
3. dedicated integration branch from `upstream/main`
4. full upstream merge only when we intentionally resync a large portion of the fork

### 4. Keep fork commits small and topic-based

The smaller and cleaner our fork commits are, the easier future upstream sync becomes.

Practical rule:
- separate UI, auth, updater, provider, and build-system work into different commits when possible

### 5. Record every reviewed release here

Every time we review an upstream release, add an entry to the ledger below with:
- date reviewed
- upstream version
- summary of upstream change
- decision
- rationale
- follow-up action, if any

## Decision Matrix

### Cherry-pick directly

Use when:
- the upstream change is isolated
- files do not overlap much with fork customizations
- the change is clearly useful and low risk

Examples:
- pricing table update
- one provider parser fix
- one reset-time parsing fix

### Reimplement locally

Use when:
- the upstream idea is useful
- but the exact commit would conflict heavily with our fork changes

Examples:
- small UI fix in a heavily customized screen
- updater tweak in a file we already rewired

### Integration branch from `upstream/main`

Use when:
- several upstream releases accumulated
- a release includes important structural or dependency changes
- direct cherry-picking would be messier than replaying our fork changes

Recommended flow:
1. create branch from latest `upstream/main`
2. identify our fork-only commits
3. replay them one by one
4. resolve conflicts deliberately
5. verify build and key providers
6. merge back into fork `main`

### Skip for now

Use when:
- the release does not matter for our deployment model
- the release is mostly packaging or localization we do not need
- integration cost is higher than value

Skipping is valid. It should still be documented here.

## How To Review a New Upstream Release

Recommended review checklist:
1. fetch upstream tags and commits
2. inspect changelog or release notes
3. inspect commit list and changed files
4. classify: relevant, optional, or not relevant
5. decide: cherry-pick, reimplement, integration branch, or skip
6. log the decision in this document

Useful commands in PowerShell at `D:\Apps\Win-CodexBar`:

```powershell
git fetch upstream --tags
git log --oneline --decorate main..upstream/main
git diff --stat <old-tag>..<new-tag>
git show <commit>
```

## What Happens If We Ignore Upstream Too Long?

Yes, the risk goes up over time.

Main reasons:
- dependency drift
- installer or packaging changes that may become prerequisites later
- broader file overlap in `app.rs`, `preferences.rs`, provider modules, and updater logic
- harder conflict resolution if both sides touched the same architecture repeatedly

That said, it only becomes dangerous if we also let our own fork changes grow in large, mixed commits.

### Risk controls

To keep long-term sync manageable:
- review upstream regularly, even if we do not integrate immediately
- keep this ledger up to date
- keep our own commits scoped and descriptive
- do a periodic upstream replay branch when several relevant releases pile up
- avoid bundling unrelated fork work into one giant commit

### Escalation threshold

Create a dedicated integration branch from `upstream/main` when any of these happens:
- 3 or more relevant upstream releases accumulated
- upstream changes core dependencies or build chain
- upstream changes the same UI files we changed repeatedly
- a security, auth, or provider-breaking fix lands upstream

## Integration Playbook for a Future Important Update

If a genuinely important upstream release lands after a long gap:

1. do not install via app updater
2. create `codex/upstream-resync-YYYYMMDD`
3. branch from latest `upstream/main`
4. list fork-only commits
5. replay or reimplement fork changes in priority order:
   - build/toolchain
   - auth and provider fixes
   - updater behavior
   - dashboard/UI refinements
6. run validation on the rebuilt app
7. merge back into fork `main`
8. record the result in this ledger

## Fork-Specific Decisions Already In Place

These decisions affect future update handling:
- taskbar icon is embedded in the fork build via Windows resource embedding
- identical upstream release banners can be dismissed persistently via `ignored_update_version`
- `Gemini` is labeled `Gemini CLI` in the fork UI
- several provider quota visualizations differ from upstream
- the fork uses the GNU target build path on this machine

These are likely conflict hotspots during future upstream UI or updater work.

## Update Ledger

### 2026-03-22 to 2026-03-24: fork customization phase

Reviewed area:
- local fork-only changes, not a formal upstream release intake

Key fork changes added:
- provider quota handling refinements
- redesigned dashboard
- GNU target build default on this Windows machine
- persistent ignored update banners
- embedded Windows app icon
- `Gemini CLI` naming
- quota severity color logic

Result:
- fork diverged intentionally from upstream UI and updater behavior

### 2026-03-24: upstream v1.2.3

Summary:
- useful smaller fixes including Codex code review reset handling and GPT-5.4 pricing updates

Decision:
- partially adopt useful isolated ideas

Rationale:
- several changes were low-risk and easy to integrate
- large UI/state changes were not worth taking wholesale

Outcome:
- selected useful concepts were integrated into the fork
- broad upstream UI changes were not merged as a whole

### 2026-03-29: upstream v1.2.5

Summary:
- Simplified Chinese localization across Windows UI
- CJK fallback fonts for localized rendering
- localized reset and status string fixes

Decision:
- skipped for now

Rationale:
- low value for this fork's immediate goals
- high overlap with heavily customized UI files
- did not solve our active fork issues

Outcome:
- no upstream merge performed
- fork kept its own UI and updater path

### 2026-03-31: upstream v1.2.8

Summary:
- installer/runtime packaging fix: bundle Microsoft Visual C++ Redistributable for Windows installs

Decision:
- skipped for current fork workflow

Rationale:
- this fork is built and run locally with the GNU toolchain on this machine
- change is relevant mainly for official installer-based deployment
- not important enough to justify an upstream integration cycle right now

Outcome:
- review completed
- no source integration required at this time

### 2026-04-06: upstream v1.2.12

Summary:
- release notes advertise updater hardening, CLI path hardening, Kiro path hardening, Infini provider support, and several CLI/provider fixes
- the tagged commit history and release notes are not perfectly aligned, so `upstream/main` was reviewed directly instead of trusting the tag range blindly

Decision:
- partially adopt useful isolated ideas

Rationale:
- the security-oriented hardening changes were low-risk and useful in the fork
- the Claude session-key fix was useful and isolated
- larger provider/UI additions such as Infini, NanoGPT, and summary-view work were intentionally skipped because they overlap with fork-specific UI and product direction

Outcome:
- adopted:
  - updater SHA256 verification via release metadata
  - safer CLI binary resolution to avoid CWD-hijack style path resolution
  - hardened Kiro CLI discovery with validated PATH/install-location lookup and explicit env override
  - Claude session env-key support and browser-like request headers
- skipped for now:
  - Infini provider
  - NanoGPT provider
  - upstream summary-view UI
- sync risk remains moderate in `app.rs` and other UI files, but this intake stayed focused on non-UI security/provider surfaces

## Future Ledger Entry Template

```markdown
### YYYY-MM-DD: upstream vX.Y.Z

Summary:
- 

Decision:
- cherry-pick / reimplement / integration branch / skip

Rationale:
- 

Outcome:
- 
```

## Maintenance Rule

Whenever we review an upstream release, update this file in the same session.

If we adopt any upstream change, also note:
- whether it was cherry-picked or reimplemented
- which files were conflict-heavy
- whether future sync risk increased or decreased
