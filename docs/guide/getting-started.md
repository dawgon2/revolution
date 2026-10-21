# Getting started

Status: **written** (install, workflow, X-M5 import, hardware path).

## Install

> id: getting-started.install | does: gets Revolution running: a ready-made Linux build or a build from source, then the first launch | resolve: installer from Blackmagic's site | premiere: Creative Cloud app | when: "how do I install Revolution?"; what graphics card it needs | see: projects.opening, getting-started.workflow

**[Fork]**

There are two ways to get this fork. Both give the same program; the ready-made build is quicker,
building from source gets you the newest work between releases.

**A ready-made build.** Every release has notes saying what is new and a package for each
common system ([install](install.md) compares them). The quickest start on Linux:

1. Download `revolution-<version>-linux-x86_64.tar.gz` of the newest release.
2. Unpack it into a folder of your own (no administrator rights needed).
3. Start the Revolution program in that folder. Nothing is installed system-wide; to remove
   Revolution, delete the folder.

**From source.** Revolution is pure Rust, so the Rust toolchain is all you need.

1. Install Rust with `rustup` (the stable toolchain).
2. Get the source and, in its folder, run `cargo run --release -p filmcraft`. The first build
   takes a while; later ones only rebuild what changed.
3. Optional: Japanese text in the interface and in titles needs the fonts of the separate
   craft-fonts repository. Clone it next to Revolution and build with `CRAFT_FONTS_DIR` set to its
   folder. Without it everything else works the same.

**What your computer needs.** A graphics card that supports Vulkan (most AMD, Intel and NVIDIA
cards from the last ten years): playback, grading and effects run on it. For smooth playback of
H.265 (HEVC) and H.264 camera files, the card's video engine is used through VA-API when it is
there (AMD and Intel on Linux); otherwise Revolution decodes in software, which is slower, and
proxies help ([proxies](proxies.md)).

**The first launch.** In this fork Revolution opens on the **Project Manager**, where you pick or
make a project ([projects](projects.md#opening-the-project-manager)). The workflow (Resolve
or Premiere: page layout, names and keyboard) is chosen in the next section; you can change it at
any time.

Where this differs: Resolve and Premiere come with installers and accounts; Revolution has
installers too (or none, with the portable folder), but no sign-in and no licence key.

## Choose your workflow: Premiere or Resolve

> id: getting-started.workflow | does: one setting switches between the Premiere and Resolve layouts, names and keys | ui: page.* | cmd: settings.setWorkflow, settings.getWorkflow, app.settings.general | resolve: Resolve's pages and names | premiere: Premiere's workspaces and names | when: first launch; your hands know the other app; Sequence vs Timeline names | see: pages.what

**[Resolve-style] [Fork]** on top of **[Revolution]**'s Premiere-style layout

### Why

Revolution is laid out like Premiere Pro. If your hands know DaVinci Resolve, one setting switches
the keyboard and the window furniture to Resolve's habits, and switching back puts everything as
it was.

### Steps

1. Choose Edit ▸ Preferences ▸ **General…** (`app.settings.general`, Ctrl+,). The window is
   titled "Preferences"; the first row of the **General** category is **Workflow**.
2. Pick **DaVinci Resolve** in the Workflow list (command `settings.setWorkflow`, parameter
   `workflow`: `premiere` or `resolve`). It applies at once; there is no OK to press.
3. Check the result:
   - A **page bar** appears along the bottom of the window: **Media**, **Edit**, **Color**,
     **Fairlight**, **Deliver** (ids {ui:page.media}, {ui:page.edit}, {ui:page.color},
     {ui:page.fairlight}, {ui:page.deliver}), with Project Manager and Settings on its right.
     Each page is a built-in workspace laid out like Resolve's page (Media Page, Edit Page,
     Color Grading, Fairlight Page, Deliver Page); Deliver holds the export settings and queue.
     The Premiere header gives way to the menus and a toolbar of panel buttons for the page.
     The whole story is in [pages.md](pages.md).
   - The keyboard preset becomes **DaVinci Resolve Compatible**.
   - The Color page is the **Color Grading** workspace: the node grader's Gallery, Nodes and
     Primaries panels around the viewer, with Lumetri Scopes below. Lumetri Color is not gone:
     it stays as a tab next to Primaries.
4. To go back, pick **Premiere Pro** in the same list. Your previous keyboard preset returns.
   The page bar disappears; the Color Grading workspace itself is available in both workflows
   (Window ▸ Workspaces ▸ **Color Grading**, `window.workspace.colorgrading`). With the Resolve
   workflow on, Window ▸ Workspaces ▸ **Color** (`window.workspace.color`, Shift+6) opens Color
   Grading too instead of the Premiere-style Color workspace.

Read the current state with `settings.getWorkflow`.

### The default, and the Resolve theme

**[Fork]** In this fork the default workflow is **Resolve** (RESOLVE-UI-PLAN RS1). A fresh install
starts in Resolve. Preferences written before RS1 are switched to Resolve **once**, the first time
the new build starts, through the same switch as above, so your previous keyboard preset and dark
theme are remembered and come back when you choose **Premiere Pro**. After that, your choice is
final: a Premiere workflow you picked yourself is never switched again. An unreadable preferences
file starts over in Resolve. Upstream Revolution builds (without the fork feature) keep Premiere as
the default and never migrate. A session without a data directory (the headless `filmcraft-cli`,
its MCP server, or the app when it has no data folder) also starts in Resolve, with the DaVinci
Resolve Compatible keys and the Resolve theme, so `settings.getWorkflow` and the keyboard agree.

The Resolve workflow shows the **Resolve** dark theme (near-black bars, cool grey panels, an
orange-red accent). Choosing **Premiere Pro** puts back the dark theme you had. Settings ▸
Appearance ▸ Dark Theme (and View ▸ Appearance ▸ **Resolve**, `view.theme.resolve`) lets either
workflow use any theme, and a theme you pick yourself follows you across workflow switches in both
directions: pick the Resolve theme while in Premiere, or Darkest while in Resolve, and it stays.
Only while you have not picked one yourself does the switch swap the theme (Resolve gets the
Resolve theme; going back to Premiere restores the theme you had before). A highlight colour you set yourself still replaces the Resolve accent.

### Terminology follows the workflow

**[Resolve-style]** wording, **[Fork]** mechanism (RESOLVE-UI-PLAN RS2b). In the Resolve workflow
the menus, panel titles and labels use Resolve's word for each thing; the Premiere workflow keeps
the wording below in its right-hand column. Only the words change: command ids (`sequence.*`
stays `sequence.*`), automation ids, project files and the control protocol are the same in both
workflows, so a project opens identically in either. Switching the workflow
(`settings.setWorkflow`) relabels everything at once, with no restart. In other interface
languages the Resolve word is used only where the overlay has one; every other string shows its
normal translation.

| Resolve workflow | Premiere workflow |
|---|---|
| Timeline (File ▸ New ▸ Timeline…, Timeline Settings…, the Timeline menu) | Sequence |
| Compound Clip (New Compound Clip…) | Nest |
| Media Pool | Project panel |
| Source Viewer / Timeline Viewer | Source Monitor / Program Monitor |
| Keyframes (the panel with the keyframe lane; the Inspector is a panel of its own) | Effect Controls |
| Mixer | Audio Track Mixer |
| Render Settings, Render Queue (Add to Render Queue) | Export Options, export queue (Send to Queue) |
| Blade Tool | Razor Tool |
| Adjustment Clip… | Adjustment Layer… |
| Render Cache (Render Cache In to Out) | Render In to Out, render previews |
| Working Folders | Scratch Disks |

A timeline and a sequence are the same thing. If you know Premiere, read "Timeline" as "Sequence" everywhere in this guide; where a page names a
renamed thing it gives both terms, for example "Timeline (Premiere: Sequence)". The Export mode
is the Deliver page in this workflow ([pages.md](pages.md)), and the collect / consolidate
"Project Manager…" item is relabelled by the Project Manager home screen (RS2), not by this
overlay.

### What happens to your shortcuts

- Edited shortcuts are never lost. Leaving Premiere with unsaved key edits first saves them as a
  preset named "<name> (before Resolve)". Going back with edits in the Resolve set saves them as
  "DaVinci Resolve Compatible (edited)". Saved presets are never overwritten.
- If you chose a different preset by hand while in Resolve, that one stays when you go back.
- If saving fails, the switch is refused and nothing changes.

### The Resolve keymap

Only keys that Blackmagic documents publicly are assigned; everything else keeps its Revolution
default. Ctrl takes the place of Cmd on Linux and Windows. For the colour work:

| Keys | Command |
|---|---|
| Alt+S | `grade.addNode` (add serial node) |
| Shift+S | `grade.insertNode` (insert node before) |
| Ctrl+D | `grade.toggleNode` (enable / disable node) |
| Shift+D | `grade.bypass` |
| Ctrl+Alt+G | `stills.grab` |
| Shift+2 / 4 / 6 / 7 / 8 | the pages: Media, Edit, Color, Fairlight, Deliver |

Ctrl+W is assigned to `compare.set`, but pressed from the keyboard it changes nothing yet (the
command needs a still and a mode; use the Gallery buttons). The editing keys (undo, select all,
delete, ripple delete and so on) also follow Resolve's; open the shortcuts dialog to see the
full list.

### If you know Resolve

The pages are Revolution workspaces, not separate programs, so edits, colour and export share one
project and one undo history; each page's layout can be rearranged and saved ([pages.md](pages.md)). The keymap is a convenience, not a copy of Resolve.

### Window frame: Revolution's or your desktop's

**[Fork]** On Linux and Windows, Settings ▸ General, just below Workflow (`general.titleBar`),
chooses who draws the top edge of the window:

- **Revolution (like DaVinci Resolve)**, the default: no desktop frame. Minimise, maximise and close
  sit at the right end of the top row; drag the top row to move the window, double-click it to
  maximise, and drag the window's outer edge or corners to resize it. Dragging to a screen edge
  still tiles the window, and Plasma's keyboard shortcuts (Meta+Up and so on) still work.
- **System**: your desktop's own frame (Plasma's decoration theme, for example Breeze or Klassy),
  with its window menu (Keep Above, window rules), shadow and rounded corners.

Which to pick is taste: Revolution's gives the picture a few more pixels and looks like Resolve;
the desktop's matches your other windows and keeps the decoration's extras. On a tiling window
manager the System choice usually fits better. The change applies when you press OK. macOS
always keeps its own window buttons.

## Import X-M5 clips

> id: getting-started.import-xm5 | does: from a Fujifilm X-M5 card to a graded sequence: copy, import, colour, proxies, sequence | cmd: card.offload, file.import, clip.interpretFootage, sequence.colorSettings, media.createProxies | resolve: Clone tool, Media Pool import, colour-managed project | premiere: Media Browser ingest, Interpret Footage, proxies | when: every X-M5 shoot | see: cards.insert, color.color-management.input, proxies.auto

**[Revolution]** **[Fork]**

1. **Copy the card.** Put the SD card in: the card bar offers a verified copy (`card.offload`,
   see [cards.md](cards.md)). Format the card in the camera only after the copy says it is
   verified.
2. **Import the copy** (File ▸ Import, `file.import`): the clips are H.265 (HEVC) `.MOV` files,
   10-bit.
3. **Check the colour.** F-Log2 clips are meant to look flat and grey before you grade them.
   Revolution reads the colour space from the file, including the F-Log / F-Log2 profile the
   X-M5 writes into its own metadata; if a clip looks wrong after grading starts, set it by hand with Interpret Footage ▸ **Fujifilm F-Log2 / F-Gamut** (`clip.interpretFootage`,
   see [color/color-management.md](color/color-management.md)).
4. **Let the proxies build.** Heavy clips such as 6.2K get a smaller proxy in the background while
   you work ([proxies.md](proxies.md)); nothing waits for them.
5. **Make the sequence** and set its colour management to DaVinci Wide Gamut / Intermediate
   (Sequence ▸ Color Management…, `sequence.colorSettings`) before grading.

The two choices that matter most are made in the camera:

| In the camera | Good for | Trade-offs |
|---|---|---|
| **H.265 4:2:0 10-bit** (main setting) | Most work; decodes on the AMD video engine, zero-copy, smooth 6.2K playback | Less colour detail for heavy keying |
| **H.265 4:2:2 10-bit** | Green screen and strong colour work | Decoded by the Intel video engine and handed to the AMD card (see [hardware.md](hardware.md)); slower to export as 4:2:2 |
| **6.2K 3:2 open gate** (6240 × 4160) | Reframing: one recording gives a 16:9 wide shot, a 9:16 vertical cut and a 3:2 photo-style frame | Large files; a 16:9 sequence leaves out the top and bottom |
| **16:9 (4K or 6.2K 16:9)** | Straight-to-YouTube projects | No room to reframe vertically |

For the sequence itself: a **4K 16:9 (3840 × 2160)** sequence with the open-gate clips scaled to
fill (Clip ▸ Scale to Frame Size, then Motion ▸ Position to reframe) is the usual YouTube setup;
a **6240 × 4160** sequence keeps the whole frame for vertical and photo-style exports.

## What the hardware path does

> id: getting-started.hardware | does: decoding, display, grade and H.265 export run on the graphics card | resolve: GPU decode and processing | premiere: Mercury Playback Engine GPU | when: wondering why 6.2K plays smoothly, or doesn't | see: hardware.why

**[Fork]**

Decoding, display, the grade and H.265 export run on the AMD graphics card and its video
engine, with automatic fallbacks; 4:2:2 clips can't use the video engine. The whole story, and
the switches, are in [hardware.md](hardware.md).
