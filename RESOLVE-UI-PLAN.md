# RESOLVE-UI-PLAN.md: Resolve style as the fork's default (Project Manager home screen first)

Owner decision, 2026-10-12 (FORK-PLAN §1a.39). Read this in every session that works on an RS
step. Clean-room rules (FORK-PLAN §4, AGENTS.md §1) apply in full: Resolve's *layout and
behaviour* are reproduced from observation and its public manual only; no Blackmagic logo,
icons, names, artwork or code; FilmCraft's own icons (`crates/ui-egui/src/icons.rs`) and brand.

## 0. What the owner asked for

- **"Resolve style" is the fork's default.** Settings ▸ General ▸ **Workflow: Premiere / Resolve**
  already exists (GRADE-PLAN G1.8: Resolve keymap preset, page bar, Color → node grader); in the
  fork, fresh installs *and* the owner's existing preferences start in **Resolve**.
- **Premiere style stays a full, working option.** Nothing Premiere-style is removed or allowed to
  rot: it is upstream's default and the owner doesn't want to stray from upstream. Every RS step
  keeps the Premiere workflow pixel- and behaviour-identical (its screenshot checks and tests
  stay green) and is tested in both workflows.
- **The thing the owner likes most: Resolve's Project Manager home screen.** On launch the app
  shows your projects and you pick one to go into (or make a new one), instead of dropping you
  into an empty project. This is RS2 and comes first after the default switch.

## 1. Visual reference (maintainer-local, never committed)

The owner's screenshots of DaVinci Resolve 21 are in `plan/resolve/screenshots/` (gitignored,
copied from `~/Pictures/Screenshots/resolve*.png`). They are Blackmagic's UI: look at them for
layout, never commit them or anything cut from them (icons, the three-petal logo, wordmark).

| File | Shows | Used by |
|---|---|---|
| `resolveHomePage.png` | Project Manager (home screen) | RS2 |
| `resolveCutPage.png`, `resolveEditPage.png`, `resolveFairlightPage.png`, `resolveFusionPage.png`, `resolveDeliverPage.png` | Page layouts, top toolbar, bottom page bar | RS3 |
| `resolveColorWheels.png`, `resolveHDRColorWheels.png`, `resolveCurves.png`, `resolveQualifier.png`, `resolveWindow.png`, `resolveTracker.png`, `resolveBlur.png`, `resolveKey.png`, `resolveRGBMixer.png`, `resolveColorSlice.png`, `resolveColorWarper.png`, `resolveMotionEffects.png`, `resolveInputSizing.png`, `resolve3DStereo.png` | Color page palettes (tool tabs under the viewer) | GRADE-PLAN G1.8 Color page layout, G2, G4 |

Observed colours (sampled from the screenshots; colours aren't protected, use them as theme
token values): menu bar / page bar / home title bar `#17181a`; home grid background `#212126`;
page panels `#28282e`; selection / active-page accent `#fa5b4a` (orange-red); secondary button
text `#747475`.

## 2. Steps

### RS1: Resolve is the fork's default workflow (small; fork-only hook)
1. `crates/engine/src/settings.rs` `Workflow`: the default becomes `Resolve` **only** with a new
   engine feature `fork-resolve-default`, enabled by `apps/filmcraft` in the fork build. Upstream
   (feature off) keeps `Premiere`. Follow FORK-PLAN §1a.36: the `cfg_attr` hook lines are marked
   `// FORK-ONLY`, the commit is prefixed `FORK-ONLY:`, and the hook is listed in
   [`docs/fork/FORK-ONLY.md`](docs/fork/FORK-ONLY.md). `cargo xtask ci` must be green with the
   feature on and off.
2. One-time migration of existing preferences (bump `PREFS_VERSION`, fork feature only): a prefs
   file written before RS1 switches to Resolve once, through `set_workflow` so the previous
   keymap is remembered as `workflowReturnPreset`. A later explicit switch back to Premiere is
   respected forever (never re-migrated).
3. A **Resolve theme** (`ThemeKind::Resolve` in `crates/ui-egui/src/theme.rs`, token values from
   §1) used by default in the Resolve workflow; the Premiere workflow keeps `Dark` / `Medium` /
   `Light`. Settings ▸ Appearance still lets either workflow pick any theme.
4. Tests: default per feature, migration once / not after an explicit Premiere choice, garbage
   prefs fall back safely, switching back and forth restores keymap and theme. Guide page
   (§1a.21): update `docs/guide/` Settings ▸ Workflow ([Fork] tag for the default).

### RS2: Project Manager home screen (the owner's main ask; upstream-contributable)
Behaviour (from `resolveHomePage.png` and the Resolve manual's "Project Manager" chapter):
1. **Shown at launch** in the Resolve workflow when no project path was given on the command
   line / by "open with" (those still open straight in). Also opened by the **Home** button on
   the page bar (bottom right), **File ▸ Project Manager** and Resolve's **Shift+1**. Closing it
   (Close / Esc) returns to the open project, or stays if none is open.
   New preference **Settings ▸ General ▸ At startup show: Project Manager / Last project / New
   empty project**; Resolve workflow defaults to Project Manager, Premiere keeps today's
   behaviour. This keeps the feature generic, so it can go upstream (UPSTREAM-PLAN
   contributions) without the fork-only default.
2. **Layout:** a full-window screen (no panels behind it). Title bar: close (×) left, a
   **Local** tab (Resolve's Network / Cloud tabs: not planned, leave them out), "FilmCraft" title
   centre. Row 2: sidebar toggle + breadcrumb ("Projects › folder"); right side: new folder,
   thumbnail size slider, sort (name / date modified / date created), info toggle (details of
   the selected project), **grid / list** view toggle, search. Body: a grid of project tiles
   (16:9 thumbnail ~214×120 at default size, name under it, accent outline when selected),
   first tile **New Project** (FilmCraft's own plus icon). List view: name, modified, created,
   resolution, frame rate, timelines count. Footer: **Export** / **Import** (left),
   **New Project** / **Open** / **Close** (right).
3. **Where projects come from.** Resolve keeps projects in a database; FilmCraft has `.fcproj`
   files, so the screen shows: (a) a **projects folder** (new preference, default
   `~/Videos/FilmCraft Projects`, created on first use; sub-folders are the screen's folders),
   plus (b) `prefs.general.recent_projects` that live elsewhere, shown in an "Other locations"
   group. Missing files are listed greyed with **Locate…** / **Remove from list**.
4. **Thumbnails and metadata** without opening the whole project: on every save, write a small
   poster frame (the frame under the playhead of the active sequence, ≤ 480 px PNG) and a summary
   (name, modified, resolution, fps, sequence count) next to the project or in a cache keyed by
   path + mtime. Project files are hostile input: read only the summary, cap sizes, a corrupt or
   foreign file shows a "can't read" tile, never a crash (regression tests with garbage files).
5. **Actions** (each a command, agent-drivable, automation ids on every tile / button):
   `projects.show`, `projects.list` (folder, sort, search → entries), `projects.open`,
   `projects.new` (asks for a name, creates `<name>.fcproj` in the current folder and opens it),
   `projects.rename`, `projects.duplicate`, `projects.newFolder`, `projects.reveal` (file
   manager), `projects.removeFromList`, `projects.trash` (OS trash, with confirmation; never a
   permanent delete), `projects.export` (archive: reuse the existing collect / consolidate
   Project Manager engine into a folder or `.zip`) and `projects.import` (copy an archive / a
   `.fcproj` into the projects folder). Double-click or Enter opens; right-click menu has the
   rest. Opening a project lands on the page it was last saved on.
6. **Naming clash:** FilmCraft's existing File ▸ **Project Manager…** is Premiere's collect /
   consolidate tool. In the Resolve workflow that menu item is labelled **Media Management…**
   (Resolve's name for the same job) and File ▸ Project Manager opens RS2; the Premiere workflow
   keeps its current labels and reaches RS2 through File ▸ **Projects…**. Command ids don't
   change.
7. **Acceptance:** launch with no args → home screen with the owner's recent projects; new
   project → lands on the Edit page; reopen → tile has a thumbnail; garbage `.fcproj` in the
   folder → "can't read" tile; Premiere workflow launch unchanged (screenshot compare);
   `ui.screenshot` of the home screen compared side by side with `resolveHomePage.png` for
   layout; guide page `docs/guide/projects.md` ([Resolve-style] tag).

### RS2b: Resolve terminology (owner, 2026-10-12; same lane as RS1 + RS2)
The owner: Resolve calls a *sequence* a **timeline**. In the Resolve workflow, every on-screen
label uses Resolve's word for the same thing; the Premiere workflow keeps today's wording.
1. **Mechanism:** all UI text already goes through `i18n::t` (`tl!` / `tlf!`,
   `crates/ui-egui/src/i18n/`). Add a terminology overlay (e.g. `i18n/terms-resolve.tsv`, English
   source string → Resolve wording) consulted before the language catalogue while the workflow
   is Resolve, including command labels and menu paths that come from the engine registry.
   Generic and upstream-contributable ("terminology follows the workflow"). Other languages: use
   that language's Resolve term where the overlay has one, else the normal translation.
2. **Only words change.** Command ids (`sequence.*` stays `sequence.*`), the project file, the
   control protocol, automation ids and the data model don't change; the guide names both terms
   ("Timeline (Premiere: Sequence)").
3. **Terms** (from Resolve's public manual; extend as RS3 finds more):

   | Premiere / FilmCraft today | Resolve workflow |
   |---|---|
   | Sequence (New Sequence, Sequence Settings, Sequence menu) | Timeline (New Timeline, Timeline Settings, Timeline menu) |
   | Nest / Nested sequence | Compound Clip |
   | Project panel | Media Pool |
   | Source Monitor / Program Monitor | Source Viewer / Timeline Viewer |
   | Effect Controls | Inspector |
   | Audio Track Mixer | Mixer |
   | Export Media / Export Settings / export queue | Deliver / Render Settings / Render Queue |
   | Razor tool | Blade |
   | Adjustment Layer | Adjustment Clip |
   | Render In to Out / render previews | Render Cache |
   | Scratch Disks | Working Folders |
   | Workspaces (where they map to pages) | Pages |
   | Project Manager (collect / consolidate) | Media Management (RS2 item 6) |

4. **Tests:** every overlay key exists as a real source string (no dead entries), switching the
   workflow relabels menus and panels live, Premiere wording is byte-identical to before.

### RS3: Resolve page chrome and page layouts (after RS2)
Make each page look like its Resolve counterpart in the Resolve workflow, reusing FilmCraft's
existing panels (no new editing features here):
1. **Chrome on every page:** menu bar; a per-page toolbar row of labelled panel toggles (left:
   Media Pool, Effects, Index, Sound Library…; right: Mixer, Metadata, Inspector…) with the
   project name centred; the bottom **page bar** (app name left, pages centred as icon + label
   with the active one highlighted and underlined in the accent, Home and Settings buttons
   right). G1.8 already has a page bar: restyle it to this.
2. **Pages → FilmCraft workspaces:** Media → Assembly; Cut → a Cut layout of the Editing
   workspace (media pool left, single viewer right, upper overview timeline + lower detail
   timeline) once it is built, hidden until then; Edit → Editing (bins left, source + timeline
   viewers, timeline below, Inspector right when toggled); Fusion → hidden until
   COMPOSITOR-PLAN gives it something to show; Color → the G1.8 Color page layout; Fairlight →
   Audio (meters strip top, timeline, mixer right); Deliver → Export (render settings left,
   viewer centre, render queue right, timeline below). Resolve's Photo page: not planned.
3. Every layout is a saved workspace preset, so the owner can still rearrange panels; the
   Premiere workflow keeps today's workspaces untouched.
4. Acceptance: `ui.screenshot` per page next to the matching reference in §1, both workflows,
   both GPUs not needed (UI only).

### RS4: Resolve Edit page tools and the Cut page (owner, live, 2026-10-20; FORK-PLAN §1a.112)

The owner compared Revolution's Edit page with Resolve 21's Edit and Cut pages (screenshots in
`plan/resolve/screenshots/owner-2026-10-20/`, local only) and asked: "can you match our ui and editing
tools with resolve as best as possible (and also add a cut page doing the same thing)". RS3 matched
the chrome (menu row, page toolbar, page bar); RS4 does the panels and editing tools inside the
pages. Resolve workflow only: the Premiere workflow stays as it is (its tests stay green). Clean-room
as in §0: layout and behaviour from observation and the public manual, our own icons.

1. **Edit commands (engine).** Resolve's edit set as engine commands, each one undo step, each with a
   test: `edit.insert` / `edit.overwrite` / `edit.replace` / `edit.placeOnTop` / `edit.append` /
   `edit.rippleOverwrite` / `edit.fitToFill` (timeline viewer drop overlay + toolbar buttons + Resolve
   keys F9 / F10 / F11 / F12 / Shift+F12 / Shift+F10 / Shift+F11), and the Cut page's
   `edit.smartInsert` (inserts at the edit point nearest the playhead), `edit.closeUp` (the source on
   the track above at the playhead, scaled in) and `edit.sourceOverwrite` (on the track above, lined up
   by source timecode). Existing `source.insert` / `source.overwrite` stay (Premiere).
2. **Inspector (Resolve).** A Resolve-style Inspector panel for the Resolve workflow: tabs Video /
   Audio / Effects / Transition / Image / File; Video: Transform (Zoom X/Y linked, Position X/Y,
   Rotation, Anchor X/Y, Flip), Cropping, Composite (mode, opacity), Speed Change, Stabilization and
   the rest that the engine supports, each section with an on dot, keyframe diamonds and reset, values
   in Resolve units (zoom 1.000, position in pixels from centre). Same parameters as Effect Controls
   underneath (no second copy of the maths). The page toolbar's Inspector toggle opens it in Resolve.
3. **Edit page timeline.** Resolve's horizontal timeline toolbar between viewer and timeline (timeline
   view options, Selection / Trim / Dynamic Trim / Blade, Insert / Overwrite / Replace, Snapping,
   Linked Selection, Position Lock, Flags and Markers, zoom buttons + slider, monitor volume and DIM)
   replaces the vertical tool palette in the Resolve layout; Resolve-style track headers (destination
   badge, lock, auto track select, enable; "Video 1" + clip count; audio S / M and channel format).
4. **Viewers and media pool.** Viewer header row (zoom %, timeline name menu, timecode) and centred
   transport (jog, previous / reverse / stop / play / next, loop); panel tab strips hidden in Resolve
   page layouts (single panels show a Resolve-style header instead); a Bins sidebar (Master, bins,
   Smart Bins) on the media pool.
5. **Cut page.** A `cut` page between Media and Edit: media pool top left with its own toolbar row
   (Smart Insert, Append, Ripple Overwrite, Close Up, Place on Top, Source Overwrite), the timeline
   viewer top right with audio meters, and the dual timeline: the upper timeline shows the whole
   timeline, the lower one is zoomed in around a fixed centred playhead (the timeline moves under it);
   tools column (Blade, Trim, …) and compact track headers on the left. Both timelines edit the same
   sequence with today's timeline code.
6. **Docs, Coach and acceptance.** Guide pages (`docs/guide/edit/resolve-edit-page.md`,
   `docs/guide/edit/cut-page.md`, provenance [Resolve-style]); Dell FEATURE-QUEUE rows (§1a.100);
   `ui.screenshot` of the Edit and Cut pages next to the owner's references; Premiere screenshots
   unchanged; `cargo xtask ci`; tag `resolve-ui-3`; ships in Revolution 0.2.

## 3. Order and parallelism

- RS1 → RS2 → RS2b → RS3 (FORK-PLAN §1a.6 has the authoritative order; RS2b belongs to the
  RS1 + RS2 lane and to tag `resolve-ui-1`). RS1 + RS2 are UI / engine work
  that doesn't touch the decode / render paths, so they run as a **leaf-builder lane next to
  Phase 6** as soon as a lane slot is free (routine §3), with the usual review before merge; tag
  `resolve-ui-1` once both pass, shipped in the next published build. **RS3** is its own phase
  right after G2c, before Y1 (tag `resolve-ui-2`), and finishes GRADE-PLAN G1.8's Color page
  layout along the way.
- The owner may reorder any of this.
