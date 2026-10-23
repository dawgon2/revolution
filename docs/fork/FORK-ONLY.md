# Fork-only code manifest

Everything here must be left out of any branch prepared for upstream (UPSTREAM-PLAN "Contributing
back", FORK-PLAN §1a.25 / §1a.36). New fork-only work follows FORK-PLAN §1a.36: its own crate or
module, one `#[cfg(feature = "fork-ffmpeg")]` hook marked `// FORK-ONLY`, commits prefixed
`FORK-ONLY:`, and an entry below. `grep -rn "FORK-ONLY" crates` finds every hook.

| What | Where | Hook | Since |
|---|---|---|---|
| ProRes RAW via system ffmpeg (planned, RAW-PLAN R2) | `crates/fork-prores-raw` | one line in the app crate, feature `fork-ffmpeg` | — |
| Resolve as the default workflow + one-time prefs migration (RESOLVE-UI-PLAN RS1) | `crates/engine/src/fork_resolve.rs` (+ `fork_resolve_tests.rs`); `Workflow` default in `crates/engine/src/settings.rs` | `cfg_attr` lines on `Workflow` and `cfg(feature = "fork-resolve-default")` hooks in `autosave.rs` (`Preferences::load` / `from_value`) and `lib.rs` (`start_autosave`), all marked `// FORK-ONLY`; engine feature `fork-resolve-default` enabled by `apps/filmcraft` and `apps/filmcraft-cli`; one `fork_resolve::without_data_dir` call each in `apps/filmcraft/src/main.rs` and `apps/filmcraft-cli/src/main.rs` (`Backend::open`) plus its CLI test, marked `// FORK-ONLY` | RS1 |
| Dolphin / KIO thumbnails of `.fcproj` (FORK-PLAN §1a.46, LINUX-PLAN L2.2): a thin C++ KF6 `ThumbnailCreator` running `filmcraft-cli thumbnail` | `fork/kde-thumbnailer/` (own CMake project, not in the Cargo workspace) | none: nothing outside the folder refers to it; the L2 `PKGBUILD` builds it as the split package `filmcraft-kde-thumbnailer` (remove that `package_` function with it). The `thumbnail` CLI command, MIME type and `.thumbnailer` file it relies on are generic (upstream-safe) | L2.2 |
| Dell build-and-test service DC1 (FORK-PLAN §1a.56): a systemd user timer on the Dell runs job branches `dell-ci/<kind>-<id>` (ci, test-<crate>, intel, soak-<name>) and writes 3-line results | `fork/dell-ci/` (bash + systemd units, not in the Cargo workspace) | none: nothing outside the folder refers to it | DC1 |
| Send to my training inbox (AI-PLAN AI5 item 8, FORK-PLAN §1a.92): the whole Assistant conversation, unstripped, to `<data dir>/assistant-inbox/`; runner-side import `tools/fork/ai-inbox-import.py` | `crates/engine/src/fork_inbox.rs` (+ `fork_inbox_tests.rs`), `tools/fork/ai-inbox-import.py` | engine feature `fork-inbox` (on in `apps/filmcraft` and `apps/filmcraft-cli`): `mod fork_inbox` in `lib.rs`, the `inbox` arm and `INBOX` in `coach_share.rs`, the **Send to my training inbox** button in `crates/ui-egui/src/panels/share_example.rs`, all marked `// FORK-ONLY` | 2026-10-16 |
| Update check against the fork's public releases (FORK-PLAN §1a.77, UPD): `CHANNEL` = `dawgon2/filmcraft-cachyos`, tags `fork-*` | `crates/engine/src/fork_update.rs` | `update::channel()` behind engine feature `fork-update` (enabled by `apps/filmcraft`); without it `channel()` is `None` and nothing is checked. `update.rs` / `update_install.rs` are generic (upstream-safe) | 2026-10-16 |

The RS1 commit also adds generic, upstream-safe pieces (not behind the feature): `PREFS_VERSION` 3,
the `workflowReturnTheme` preference, the `resolve` dark theme (`ThemeKind::Resolve`, View ▸
Appearance ▸ Resolve) and `set_workflow` swapping the dark theme with the keymap.

## Older fork-only code (before §1a.36, not yet isolated)

The pipeline's existing "remove fork-only parts" step still applies to these by hand: ffmpeg
fallbacks in `crates/platform/src/ffmpeg.rs`, `vaapi_pipe.rs`, `hevc_export.rs`,
`crates/engine/src/media_pool.rs`; FORK-PLAN / HANDOFF / `*-PLAN.md` / `docs/fork/`; the X-M5
benches in `crates/ui-egui/examples/bench/`.

Removed in U0.2 (2026-10-13, one VA-API stack): `crates/platform/src/va_hevc.rs` and
`crates/hevc/src/accel.rs` (+ `crates/hevc/tests/accel.rs`), both fork-only. Upstream's
`crates/platform/src/vaapi/va.rs` is removed too, so a contribution touching the VA-API decoder must
bring the fork's `va.rs` display layer with it, or adapt to upstream's.


- **Workflow repository guard (2026-10-22):** `.github/workflows/{revolution-release,freebsd,windows-arm64}.yml`
  entry jobs run only when `github.repository == 'dawgon2/revolution'` (public). The private `dawgon2/filmcraft-fork`
  ran the full release build on every `revolution-*` tag plus the nightly FreeBSD / weekly ARM64 builds, used up the
  Free plan's private-repo Actions quota and got Actions blocked account-wide (Q68).
- **No scheduled FreeBSD / Windows ARM64 builds (2026-10-22, owner):** GitHub paused the public fork's workflows
  ("due to the scale of GitHub Actions usage"); the nightly FreeBSD and weekly ARM64 schedules inherited from upstream
  were removed (manual `workflow_dispatch` still works) and both workflows disabled on dawgon2/revolution.
