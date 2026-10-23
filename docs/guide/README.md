# The Revolution fork guide

A user guide for **this fork** (Linux / AMD, X-M5 6.2K F-Log2 workflow, Resolve-style colour page)
written for two readers: **people using the app**, and the **Grading Coach** (GRADE-PLAN G2.12),
which answers questions only from this guide plus the live state of the app. Started 2026-10-11
(owner request); kept up to date by every phase (see "Keeping it true" below).

## Where each feature comes from (provenance)

Every page and every section carries one of these tags, so a reader (and the coach) always knows
what a feature is modelled on and which manual or habit will transfer:

| Tag | Meaning |
|---|---|
| **[Resolve-style]** | Behaviour or workflow modelled on DaVinci Resolve, from its public manual and observation only (clean-room: no Blackmagic code, assets, icons or manual text; our own words). If you know Resolve, it works the way you expect; differences are listed in the section. |
| **[Revolution]** | Comes from upstream FilmCraft (storytold/filmcraft), mostly modelled on Premiere Pro (e.g. Lumetri Color, Effect Controls, the timeline). Works the way Premiere users expect. |
| **[Fork]** | Designed in this fork, with no direct equivalent in Resolve or Premiere (e.g. zero-copy 6.2K playback, the node explainer, the Grading Coach, shot comparison wording). |
| **[Standard]** | Follows a published specification or a well-known method, named in the section (e.g. DaVinci Wide Gamut / Intermediate from Blackmagic's published white paper, BT.2390 tone mapping, Oklab). |

A section can carry two tags (e.g. **[Resolve-style] [Fork]** for Shot Match: Resolve's idea, our
own solver and explanations).

## Contents

| Page | Covers | Status |
|---|---|---|
| [getting-started.md](getting-started.md) | Install, the two workflows (Settings ▸ General ▸ Workflow: Premiere / Resolve), importing X-M5 clips, what the hardware path does | written |
| [install.md](install.md) | Every release package: portable Linux folder, AppImage, .deb, .rpm, Flatpak, Arch PKGBUILD, Windows .msi and portable .zip, x86_64 and ARM; which to choose | written (PKG) |
| [pages.md](pages.md) | The Resolve workflow's pages (RS3): the page bar, each page's layout and toolbar, Deliver's render settings and queue, Save Layout and layout presets | written (RS3, LAY) |
| [projects.md](projects.md) | The Project Manager home screen (RS2): the projects folder, opening and making projects, sort / search / views, rename / duplicate / trash, export / import, At startup show | written (RS2) |
| [color/README.md](color/README.md) | How colour works in this fork, start here | written (G0–G1) |
| [color/color-management.md](color/color-management.md) | F-Log2 / F-Gamut input, DaVinci Wide Gamut / Intermediate working space, Rec.709 output, highlight roll-off | written (G0–G1) |
| [color/nodes.md](color/nodes.md) | What a node is, how the chain flows, one job per node, a starter tree for F-Log2 and other node orders with their trade-offs (CO1); the node canvas, parallel / layer / outside / splitter and combiner / key mixer nodes and what the explainer says about them (C2) | written (G0–G1, C2) |
| [color/primaries.md](color/primaries.md) | Lift / Gamma / Gain / Offset wheels, contrast and pivot, saturation, temperature / tint; HDR zone wheels | written (G0–G1, G4.2) |
| [color/curves.md](color/curves.md) | Custom and the five hue / luma / sat curves | partial (G0–G1) |
| [color/secondaries.md](color/secondaries.md) | Qualifier (HSL and 3D), windows, tracking incl. planar (G2, G4), Magic Mask (G3), colour warper (G4) | written (G2.1–G2.3, G3.3, G4.1, G4.3, G4.4) |
| [color/noise.md](color/noise.md) | Spatial noise reduction on a node (G2.4), temporal per clip (6.7 / 6.7b) | written (G2.4 spatial, 6.7b temporal) |
| [color/matching.md](color/matching.md) | Stills, wipe / split, copy / paste / append grades, "same grade?", shot comparison, Shot Match, node explainer, AI explanations | written (G0–G2b) |
| [color/reference-look.md](color/reference-look.md) | Reference pictures in the Gallery, "how do I get this look?": measured difference, grade recipe, on set, Do this | written (REF) |
| [compositor.md](compositor.md) | Comps: nodes, New Comp Clip, Open in Compositor, Convert to Comp Clip, two starter comps, what is cached | done for C1 (comp size, Media In source and node names are commands only); C4: expressions, groups (macros), templates |
| [compositor-trackers.md](compositor-trackers.md) | Tracker and Planar Tracker nodes: match move, stabilise, corner pin | C3.5a: numeric controls (viewer tools later) |
| [color/finishing.md](color/finishing.md) | Grain, highlight soft clip, node LUTs, scopes (noise reduction: noise.md) | written (grain, soft clip, LUT; scopes GUIDE-8) |
| [color/lumetri.md](color/lumetri.md) | Lumetri Color (Premiere-style) and how it relates to the node grader | written (cloud) |
| [color/coach.md](color/coach.md) | Grading Coach: guided lessons (the lesson list, card buttons, outline, progress, the seven lessons, writing a lesson file), context hints (the list, the switch, writing a hint) and Ask the coach | written (G2.12) |
| [color/recipes.md](color/recipes.md) | Task recipes in plain steps ("make the sky bluer without touching skin", "fix grey skin", "match a wide and a close-up") | written (cloud: 8 recipes) |
| [render-cache.md](render-cache.md) | The render cache (Phase 6.3): when it renders, the render bar colours, invalidation, the 2 s H.265 pieces, Settings ▸ Media Cache ▸ Render Cache, `cache.*` commands | written (6.3) |
| [network-storage.md](network-storage.md) | Projects and media on a network share (S1): where a file lives, Project Settings storage line, `media.storage`; local cache, server drops, safe saves, packages to come | started (S1.1) |
| [tracking.md](tracking.md) | Tracking: masks that follow, screen replacement with Corner Pin's Track Surface | written (Y3.3) |
| [trimming.md](trimming.md) | Edit-point selection, trimming to the playhead, dynamic trimming, the Source monitor, multicam controls | written (AI3-PREP) |
| [edit-menu.md](edit-menu.md) | The Edit and Sequence menus: undo, clipboard and attributes, selecting / finding / labels, project clean-up, sequence settings and tracks, timeline helpers (snap, linked selection, gaps, through edits, match frame, subsequence, simplify), rendering | written (AI3-PREP) |
| [editing.md](editing.md) | Cutting, three-point editing, JKL, trimming, multicam (links to proxies / render cache) | written (GUIDE-7) |
| [essential-sound.md](essential-sound.md) | Essential Sound: audio types and their sections, presets, loudness Auto-Match and its per-type targets, what each Repair / Clarity / Creative control drives, ducking, Remix (music to a length) | written (AI3-PREP) |
| [command-reference.md](command-reference.md) | The commands the feature pages only hint at, by family: project files and generated clips, Media Browser, Project panel views, metadata and relinking, Clip menu, mixer commands, graphics alignment and keys, playhead and markers, timeline edits by command, track targets, multi-camera keys, preferences and shortcuts as commands, introspection (AI3: every command has a guide section) | written (AI3) |
| [effect-reference.md](effect-reference.md) | Every effect and transition in the Effects panel, by folder (names only; kept identical to the catalogue by a test) | written (AI4.5c) |
| [cards.md](cards.md) | Copying camera cards (LINUX-PLAN L2.4): the verified copy, the checksum list, cards pulled mid-copy, the card bar and Eject, `card.*` commands, copy-first vs import-from-card | written (L2.4) |
| [effects.md](effects.md) | Applying and ordering clip effects, parameters and keyframes (interpolation), transitions and crossfades, effect presets and their keyframe modes | written (AI3-PREP) |
| [graphics.md](graphics.md) | Titles and graphics: graphic clips and layers, text and per-character styles, aligning and arranging, responsive design (pins, protected time, rolls), templates (.fcgt), source graphics, fonts | written (AI3-PREP) |
| [media.md](media.md) | Importing, generated media (bars, mattes, adjustment layers), bins, saving / auto-save / recovery, offline media and relinking, clip operations (speed, frame hold, nest, subclip, sync, merge), a clip's audio channels, EDL / FCPXML / OTIO / AAF | written (AI3-PREP) |
| [mixing.md](mixing.md) | The Audio Track Mixer and Audio Clip Mixer: clip vs track levels, track effects (inserts), submixes and sends, automation modes and recording, editing track keyframes | written (AI3-PREP) |
| [timeline.md](timeline.md) | Track targeting and source patching, selecting / nudging / slipping / sliding from the keyboard, moving the playhead, In / Out and split edits, markers | written (AI3-PREP) |
| [shortcuts.md](shortcuts.md) | Keyboard shortcut presets (Revolution, Premiere, Final Cut, Avid, Resolve), changing / sharing keys, panel shortcuts and conflicts, the Premiere workflow's workspaces | written (AI3-PREP) |
| [text.md](text.md) | Transcripts and editing by text, speech models, removing pauses and filler words, captions (from the transcript, import, edit, style, export, burn-in), text-to-speech narration vs recording | written (AI3-PREP) |
| [audio.md](audio.md) | Audio clean-up (AUDIO-PLAN A2): Voice Isolation (DeepFilterNet3) and its model, the manual voice clean-up process, Clean Up Voice (one click), reviewing voice-over takes | written (A2) |
| [keying.md](keying.md) | Green screen and other keys (Ultra Key, show matte, cleanup, spill, Luma Key, Color Key) | written (Y3.1) |
| [shader-effects.md](shader-effects.md) | Shader effects: the bundled examples, writing your own WGSL effect, three recipes to copy (vignette, duotone, pulse), broken / slow / quarantined / missing effects | written (Y3.4; recipes: cloud) |
| [packs.md](packs.md) | Look packs: presets, LUTs and graphics templates in one shareable file | written (Y3.5) |
| [proxies.md](proxies.md) | Automatic proxies (which clips get them, the background worker, badges, Auto / On / Off, cancelling, relinked media) | written (6.2) |
| [export.md](export.md) | Export presets, hardware HEVC and its fallbacks, bitrate vs constant quality (CRF), HDR → Main 10 | written (cloud; YouTube / Shorts specifics to add with Y phases) |
| [export-queue.md](export-queue.md) | Managing export presets, the export queue (several deliveries one after another), Quick Export | written (AI3-PREP) |
| [file-menu.md](file-menu.md) | The File and Clip menus: projects / sequences / templates, project settings and scratch disks, search bins and offline files, more import / export formats (frames, AAF, OMF, ALE, XML), auto-saves, clip video / audio / timecode / metadata options | written (AI3-PREP) |
| [hardware.md](hardware.md) | What runs on the AMD VCN / Intel / CPU, fallbacks, what the badges mean | written (cloud; badges arrive with Phase 6) |
| [linux.md](linux.md) | Linux desktop integration (LINUX-PLAN L1 / L2): where files live (XDG folders), and the desktop services added as L1 / L2 land | written (L2.1; grows with each step) |
| [updates.md](updates.md) | Updates (UPD): the once-a-day check and what it sends, Update / Later / Don't check, Settings ▸ Updates, roll back, `app.update.*` | written (UPD) |
| [glossary.md](glossary.md) | Plain-language definitions (log, gamut, node, key, matte, LUT, …) with the Resolve and Premiere names side by side | written (cloud) |
| [practice/audio.md](practice/audio.md) | Practice notes: dialogue first, music under dialogue, high-pass, noise reduction amount, de-essing, voice compression, master limiter, loudness targets by platform, crossfades, ambience beds, panning, small speakers, plosives, wind, headroom | written (AI3-PREP, batch 3) |
| [practice/delivery.md](practice/delivery.md) | Practice notes: exposing log, viewing LUTs, wide working spaces, tone mapping to SDR, 10-bit delivery, YouTube upload resolution, vertical cuts from open gate, masters, legal levels, display gamma, thumbnails, sidecar captions, checking exports, archiving | written (AI3-PREP, batch 4) |
| [practice/editing.md](practice/editing.md) | Practice notes: selects, string-outs, edit passes and versions, cutting on action, J / L cuts, B-roll, jump cuts, cutting to music, room tone, three-point edits, pacing, proxies, syncing sound, temp mix | written (AI3-PREP, batch 2) |
| [practice/grading.md](practice/grading.md) | Practice notes (AI3-PREP): how colourists commonly work, one technique per note with the Resolve, Premiere and Revolution way, why and pitfalls: log normalisation, balance first, black / white points, contrast and pivot, shoulders, saturation, skin line, skin and sky qualifiers, vignette, look node, split toning, grain, matching order, day for night | written (AI3-PREP, batch 1) |
| [practice/node-trees.md](practice/node-trees.md) | Practice notes: fixed node trees, parallel / layer / outside nodes, group and shared grades, hue and luma-vs-sat curves, printer-light balancing, gamut compression, glow and halation, skin softening, sharpening, window tracking, grade versions (with what Revolution has and what is not yet) | written (AI3-PREP, batch 5) |
| [practice/looks.md](practice/looks.md) | Practice notes: black and white, bleach bypass, faded blacks, golden hour, cool thriller, commercial, pastel, neon night, documentary, film-like contrast, creative LUTs, exteriors, low key, matching a reference | written (AI3-PREP, batch 6) |
| [practice/mixed-cameras.md](practice/mixed-cameras.md) | Practice notes: per-camera input transforms, hero camera, phones, grey cards (charts: not yet), mixed light, green casts, backlight, under- and overexposure, continuity over a day, drones, two-camera interviews, night streets, several skin depths, archival footage, screens in shot | written (AI3-PREP, batch 10) |
| [practice/youtube.md](practice/youtube.md) | Practice notes: hooks and cold opens, pattern interrupts, punch-ins, vertical captions and safe areas, chapters from markers, thumbnails, loudness, batch exports, template projects, B-roll libraries, screen recordings, music licences, review before publishing | written (AI3-PREP, batch 7) |
| [practice/titles.md](practice/titles.md) | Practice notes: reading time, contrast, type systems, templates, lower thirds that fit, safe margins, easing, matching in / out, credit rolls, tracked callouts, vertical and Japanese text, kinetic captions, logo bugs, graphics and the grade, fewer words | written (AI3-PREP, batch 8) |
| [practice/workflow.md](practice/workflow.md) | Practice notes: project folders, verified card copies, 3-2-1 backups, naming, bins, labels, saving, proxies vs render cache, a quiet desktop, track roles, keyboard first, review copies, archiving, hand-offs, notes | written (AI3-PREP, batch 9) |

## How pages are written

- **Real names only.** Every button, panel, menu item and command a page tells the reader to use
  is written with its on-screen label **and** its id: `` `grade.addNode` `` for commands,
  `` {ui:grade.addNode} `` for automation ids (`crates/ui-egui/src/automation.rs`). The coach
  uses these ids to highlight controls; a check (below) rejects ids that don't exist.
- **Why before how.** Each section says what the tool is for and when to reach for it before the
  steps. Beginners first, with an "If you know Resolve" / "If you know Premiere" note where habits
  differ.
- **Steps are numbered and checkable** (one action each), so a lesson can be made from them.
- **Our own words.** Never copy text from Resolve's, Premiere's or anyone else's manual.
- Short pages, plain language, no marketing.

## Section headers (for the coach)

Every `##` section starts with one **compact header** line, so the coach (and later a local
model, AI-PLAN AI3) can find and use a section from a few words instead of its whole text. It is
a blockquote directly under the heading, before or after the provenance tag line:

```
> id: audio.cleanup-voice | does: one click sets the voice clean-up controls | ui: essentialSound.cleanupVoice | cmd: audio.cleanupVoice | resolve: … | premiere: … | when: voice-over, interviews | see: audio.voice-isolation
```

| Key | | What goes in it |
|---|---|---|
| `id` | required, first | `<page id>.<slug>`: the page's path without `.md`, `/` written as `.` (`color/nodes.md` → `color.nodes.…`), then a short slug; lower case, digits, `.` and `-`; unique in the guide |
| `does` | required | what the feature or technique does, in a few words (at most 120 characters) |
| `ui` | optional | automation ids the guide names as `{ui:…}` (so the UI check covers them), or a prefix ending in `*` (`grade.qualifier.*`) |
| `cmd` | optional | command ids (an engine command, or one the guide names in backticks, such as the desktop's `page.deliver`), or a prefix ending in `*` (`card.*`) |
| `resolve`, `premiere` | optional | the closest name in DaVinci Resolve / Premiere Pro; `—` when there is none |
| `when` | optional | when to reach for it |
| `see` | optional | ids of related sections |

Fields are separated by ` | `, each written `key: value`; lists by commas. Write the names in
our own words like the rest of the page. The check (`coach_ask_tests.rs`,
`section_headers_are_valid_unique_and_point_at_real_things`) refuses a malformed header, an id
that doesn't start with its page id or is used twice, an unknown command, a `ui` entry that isn't
a `{ui:…}` id of the guide (or a prefix of one) and a `see` that names no section. Every
section of every page but this one must have a header (a "To write." stub needs none until it is
written).

## Keeping it true

- Every working step that adds or changes something a user can see or do updates the matching
  page in the same commit (or the next one), with the right provenance tag. A phase isn't accepted
  while its pages are still "to write" for features it shipped.
- The Grading Coach (G2.12) embeds every page (`crates/engine/src/coach_ask.rs`, `GUIDE_PAGES`;
  a test fails when a page is added without being listed) and searches them for "Ask the
  coach". Its tests are the guide check: every `##` section must carry a provenance tag
  (`coach_ask_tests.rs`), and every `{ui:…}` id must be a control the app shows, builds at run
  time or registers in a dialog (`crates/ui-egui/tests/ui/coach_ui.rs`,
  `coach_ask_every_guide_ui_id_is_known`). Command ids named in a page's steps are checked each
  time the coach answers; a step naming an unknown one is left out. A page's untagged
  introduction (a status line) is not searched.
- Behaviour claims that differ from Resolve or Premiere must say so explicitly ("unlike Resolve,
  …"), so nobody is misled by a habit.
