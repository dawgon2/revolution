# The pages (Resolve workflow)

Status: **written** (RESOLVE-UI-PLAN RS3; Cut page, timeline toolbar, track headers, transport and bins column RS4). Applies when Settings ▸ General ▸ Workflow is
DaVinci Resolve (the fork's default); the Premiere workflow keeps its header and workspaces.

## What a page is

> id: pages.what | does: the Resolve workflow's pages, one per stage of the work | ui: page.* | cmd: page.* | resolve: pages (Media, Cut, Edit, Fusion, Color, Fairlight, Deliver) | premiere: workspaces | when: choosing the Resolve workflow; finding where a tool lives | see: pages.panels

**[Resolve-style]** layout, **[Fork]** mechanism

### Why

Resolve splits the work into pages: bring media in, cut, grade, mix, deliver. Each page puts the
panels for one job on screen and hides the rest, so a beginner always sees a short list of tools
and a pro switches jobs with one key. In Revolution a page is a **built-in workspace**: the same
project, the same timeline and one undo history behind every page, only the panel layout changes.

### The window

1. Along the bottom, the **page bar**: the app name on the left, the pages in the middle (an icon
   over each name; the page you are on is lit and underlined in orange-red) and, on the right,
   **Project Manager** ({ui:pageBar.home}, the home screen of [projects.md](projects.md)) and
   **Settings** ({ui:pageBar.settings}, Edit ▸ Preferences ▸ General, `app.settings.general`).
2. Along the top, the menus on a row of their own, then the page's **toolbar**: buttons with a
   label that show or hide that page's panels, the project name in the middle.
3. Click a page in the bar to go there: **Media** ({ui:page.media}), **Cut** ({ui:page.cut}), **Edit** ({ui:page.edit}),
   **Fusion** ({ui:page.fusion}), **Color** ({ui:page.color}), **Fairlight**
   ({ui:page.fairlight}), **Deliver** ({ui:page.deliver}). The commands are `page.media`,
   `page.cut`, `page.edit`, `page.fusion`, `page.color`, `page.fairlight` and `page.deliver`; with the
   DaVinci Resolve Compatible keys, Shift+2, 3, 4, 6, 7 and 8 open Media, Cut, Edit, Color,
   Fairlight and Deliver.

### If you know Resolve

- **Revolution:** the Cut page has the dual timeline and six fast edits (below); Sync Bin,
  source tape, the boring detector and the trim / transition / title bars of Resolve's Cut page
  aren't there yet.

The Fusion page holds Revolution's much smaller node compositor ([compositor.md](compositor.md)), not
Fusion itself. The Photo page is not planned. Unlike Resolve, the menus
and the pages are one window's furniture: closing a panel on one page doesn't close it on the
others, and every page's layout can be changed and saved (below).

## The pages and their panels

> id: pages.panels | does: which panels each page shows and what Resolve calls them | ui: page.*, pageBar.* | resolve: Media Pool, Inspector, Fairlight mixer | premiere: Project panel, Effect Controls, Audio Track Mixer | when: looking for a panel on a page | see: pages.toolbar

**[Resolve-style]** layouts, built from **[Revolution]** panels

Resolve's panel names are used in this workflow (see "Terminology follows the workflow" in
[getting-started.md](getting-started.md)): Media Pool is the Project panel, Mixer is the Audio
Track Mixer. The toolbar's Inspector is a Resolve-style panel of its own (tabs Video, Audio,
Effects, Transition, Image, File) that edits the same clip settings as Effect Controls; Effect
Controls itself, with its keyframe lane, is called **Keyframes** here (the Edit page's toolbar
shows and hides it, as Resolve's Keyframes button does).

| Page | Workspace | Layout | Toolbar (left / right) |
|---|---|---|---|
| Media | Media Page | Media Browser and Source Viewer over the Media Pool; Metadata and the meters on the right | Media Browser, Media Pool / Audio Meters, Metadata, Inspector |
| Cut | Cut Page | Media Pool and Timeline Viewer (with audio meters) side by side over the Cut timeline: fast edits, the whole timeline, the zoomed-in timeline (below) | Media Pool, Effects / Quick Export, Mixer, Inspector |
| Edit | Edit Page | Media Pool (a Bins column, then the clips) and Effects / Markers as tabs down the left; Source Viewer and Timeline Viewer side by side over the Timeline with Resolve's timeline toolbar (below) | Media Pool, Effects, Markers, Keyframes / Quick Export, Mixer, Metadata, Inspector |
| Fusion | Compositing | the Compositor (node canvas with the selected node's settings under it) beside the Timeline Viewer, over the Media Pool and the Timeline ([compositor.md](compositor.md)) | Media Pool, Effects, Compositor / Metadata, Inspector |
| Color | Color Grading | the node grader: Gallery, viewer, Nodes; the clips strip; Primaries and scopes ([color/README.md](color/README.md)) | Gallery, Media Pool, Clips / Nodes, Lumetri Scopes, Effects |
| Fairlight | Fairlight Page | a small viewer and the meters with the loudness readout across the top; the Timeline; the Mixer (and Essential Sound) on the right | Media Pool, Effects, Markers / Mixer, Audio Meters, Metadata, Inspector |
| Deliver | Deliver Page | Render Settings down the left; the Timeline Viewer and the Render Queue over the Timeline | Render Settings / Render Queue |

Differences from Resolve: the panels keep Revolution's tab headers (title and ≡ menu); the Fairlight page's top strip shows the Mix meters,
not one meter per track (the per-track meters are in the Mixer); Resolve's Index, Sound Library,
Keyframes, ADR, LUTs and Lightbox have no Revolution panel yet, so their buttons are left out.

## The Cut page: fast edits on a dual timeline

> id: pages.cut | does: the Cut page: six fast edits from the media pool, the whole timeline above, a zoomed-in timeline under a fixed playhead | ui: page.cut, cut.smartInsert, cut.append, cut.rippleOverwrite, cut.closeUp, cut.placeOnTop, cut.sourceOverwrite, cut.blade, cut.zoomIn, cut.zoomOut | cmd: edit.smartInsert, edit.append, edit.rippleOverwrite, edit.closeUp, edit.placeOnTop, edit.sourceOverwrite, sequence.addEdit | resolve: Cut page | premiere: — (the Timeline panel and Insert / Overwrite) | when: a fast first cut; seeing the whole edit and the detail at once | see: pages.edit-toolbar

**[Resolve-style]** layout and edits, drawn by **[Revolution]**'s own timeline

### Why

The Edit page is for precise work; the Cut page is for speed. You see the **whole timeline** at
the top, so you always know where you are in the story, and a **zoomed-in timeline** below it
where the playhead stays in the middle and the edit moves under it, so trimming near the playhead
never needs zooming or scrolling. The edit buttons work straight from the media pool: no Source
Viewer, no In and Out marks needed. Beginners can build a first cut with Append alone; editors
used to the Edit page can use both pages on the same timeline, since every page shares it and
its undo history.

### Steps

1. Click **Cut** in the page bar ({ui:page.cut}; command `page.cut`).
2. Select a clip in the media pool (top left). With nothing selected the edit buttons are greyed
   and the bar says so.
3. Pick an edit in the bar over the timelines. Each is one undo step:
   - **Smart Insert** ({ui:cut.smartInsert}, `edit.smartInsert`): inserts the clip at the cut
     nearest the playhead, so you never split a shot by accident.
   - **Append** ({ui:cut.append}, `edit.append`): adds the clip to the end of the timeline.
   - **Ripple Overwrite** ({ui:cut.rippleOverwrite}, `edit.rippleOverwrite`): replaces the clip
     under the playhead and closes up (or opens) the difference in length.
   - **Close Up** ({ui:cut.closeUp}, `edit.closeUp`): puts the clip on the track above at the
     playhead, scaled in for a closer framing.
   - **Place on Top** ({ui:cut.placeOnTop}, `edit.placeOnTop`): puts the clip on the track above
     at the playhead (a cutaway or B-roll).
   - **Source Overwrite** ({ui:cut.sourceOverwrite}, `edit.sourceOverwrite`): puts the clip on the
     track above, lined up by source timecode with the clip under the playhead (multi-camera
     footage recorded with matching timecode).
4. Move through the edit: click or drag in either ruler, or play (Space, J / K / L). The lower
   timeline keeps the playhead centred; **+** ({ui:cut.zoomIn}) and the magnifier
   ({ui:cut.zoomOut}) zoom it.
5. On the left of the upper timeline: **Selection** ({ui:cut.tool.selection}), **Trim**
   ({ui:cut.tool.trim}: dragging an edge ripples the rest), **Cut at the playhead**
   ({ui:cut.blade}, `sequence.addEdit`), **Snapping** ({ui:cut.snap}) and **Linked Selection**
   ({ui:cut.linked}). Dragging clips, trimming edges, deleting and the right-click menus work
   in both timelines as on the Edit page.

### Choosing between the Cut and Edit pages

There is no right answer: many editors rough-cut on the Cut page and refine on the Edit page;
others stay on one. The Cut page favours speed and the overview (no Source Viewer, fewer tracks
shown, no track keyframes); the Edit page favours control (In / Out marks in the Source Viewer,
patching and targeting, keyframes, captions).

## The Edit page's timeline toolbar and track headers

> id: pages.edit-toolbar | does: Resolve's timeline toolbar and track headers on the Edit page | ui: timeline.rtool.*, timeline.redit.*, timeline.rtoggle.*, timeline.rzoom.* | cmd: edit.insert, edit.overwrite, edit.replace, sequence.snap, sequence.linkedSelection | resolve: timeline toolbar | premiere: Tools panel, Timeline toggles | when: editing on the Edit page in the Resolve workflow | see: pages.cut

**[Resolve-style]**

In the Resolve workflow the Timeline panel has Resolve's toolbar across its top instead of the
Tools strip beside it (the Premiere workflow keeps the Tools strip):

1. **Selection** ({ui:timeline.rtool.selection}, A), **Trim** ({ui:timeline.rtool.trim}, T) and
   **Blade** ({ui:timeline.rtool.blade}, B).
2. **Insert** ({ui:timeline.redit.insert}, F9), **Overwrite** ({ui:timeline.redit.overwrite},
   F10) and **Replace** ({ui:timeline.redit.replace}, F11): the Source Viewer's clip (its In and
   Out) at the timeline's In mark, or at the playhead.
3. **Snapping** ({ui:timeline.rtoggle.snap}), **Linked Selection** ({ui:timeline.rtoggle.linked})
   and **Add Marker** ({ui:timeline.rtoggle.marker}).
4. On the right: **Full Extent Zoom** ({ui:timeline.rzoom.fit}), **Detail Zoom** around the
   playhead ({ui:timeline.rzoom.detail}), zoom out, the zoom slider ({ui:timeline.rzoom.slider})
   and zoom in.

Each track header shows the source patch and the track's badge (V1, A1: lit when the track is
targeted), the lock, sync lock and output / mute / solo switches on its top line, then the
track's name and how many clips it holds. The viewers' transport is Resolve's: first frame, play
reverse, stop, play, last frame, loop, then Mark In and Mark Out; the other buttons stay on their
keys and menus. The media pool shows a **Bins** column (Master, then every bin): click a bin to
show its clips.

## Show and hide panels from the toolbar

> id: pages.toolbar | does: the page toolbar shows and hides that page's extra panels | ui: pageToolbar.* | resolve: interface toolbar | premiere: Window menu | when: a panel you need isn't on the page | see: pages.layout

**[Resolve-style]**

### Why

A page starts with the panels most people need for that job. The toolbar adds the rest when you
need them and takes them away again when you need the room, without hunting through Window.

### Steps

1. On the Edit page, click **Inspector** in the toolbar ({ui:pageToolbar.Inspector}). It
   opens as a column on the right; the button is lit while the panel is on screen.
2. Click **Inspector** again to hide it.
3. Click **Media Pool** ({ui:pageToolbar.Project}) to hide the Media Pool and give the viewers
   and the timeline the whole width; click again to bring it back on the left.
4. A button whose panel is a hidden tab (Effects behind the Media Pool) brings that tab forward.
5. **Mixer** ({ui:pageToolbar.AudioTrackMixer}) opens the Mixer on the right of the Edit page,
   the way Resolve does.

A button on the left of the toolbar opens its panel on the left of the page, one on the right
opens it on the right. Hiding the last panel of a page is refused (the status bar says why).

## The Inspector: a clip's settings in Resolve's layout

> id: pages.inspector | does: shows and edits the selected clip's transform, crop, opacity, speed and stabilization in Resolve's units | ui: inspector.*, pageToolbar.Inspector | resolve: Inspector | premiere: Effect Controls | when: you want to move, zoom, crop, fade, speed up or stabilize one clip | see: pages.toolbar

**[Resolve-style]**

### Why

Most changes to a single clip (make it bigger, move it, crop it, fade it, slow it down, steady a
shaky shot) live in one place. The Inspector keeps them in short sections you can open and close,
so the controls for one job are together and the rest are out of the way.

### Steps

1. Select a clip in the timeline. If the Inspector isn't on screen, click **Inspector** in the
   page toolbar ({ui:pageToolbar.Inspector}).
2. Pick a tab at the top: **Video** ({ui:inspector.tab.video}), **Audio**
   ({ui:inspector.tab.audio}), **Effects**, **Transition**, **Image** or **File**
   ({ui:inspector.tab.file}). A tab that doesn't apply to the clip is greyed out.
3. On the Video tab, open a section by clicking its header (**Transform**
   {ui:inspector.section.transform}, **Cropping**, **Composite**, **Speed Change**,
   **Stabilization**, **Temporal NR**). The dot on the left of a header turns that section off and
   on; the arrow on the right resets it.
4. Type a value or drag in a field. In **Transform**, **Zoom** 1.000 is the clip's own size
   (2.000 is twice as big); **Position** and **Anchor Point** count pixels from the centre of the
   frame, with Y going up; **Rotation Angle** turns counter-clockwise. Turn the link off to set
   Zoom X and Y apart; **Flip Image** mirrors the clip.
5. **Cropping** trims each edge (Crop Left, Right, Top, Bottom) and **Softness** feathers the cut.
   **Composite** sets **Composite Mode** and **Opacity** (0–100).
6. **Speed Change** sets **Direction** (Forward / Reverse) and **Speed**; the clip's duration
   follows.
7. To animate a value, move the playhead and click the diamond at the end of its row: it adds a
   keyframe there (click it again on that frame to remove it). Shape the keyframes in
   **Keyframes** ({ui:pageToolbar.EffectControls}).
8. The **File** tab lists the clip's name, its media's path, codec, resolution, frame rate,
   duration and, for sound, sample rate and channels.

### How it differs

- **From Resolve:** our own drawing and icons; the Inspector has no keyframe lane of its own (the
  diamonds add and remove keyframes at the playhead; curves are in Keyframes). Stabilization is
  FilmCraft's Warp Stabilizer, and Temporal NR is the clip's own noise reduction (the same rows
  as in Effect Controls), not Resolve's.
- **From Premiere:** Effect Controls edits the very same settings in Premiere's units: Zoom 1.000
  is Scale 100 %, Position counts from the top-left corner with Y going down, and Rotation turns
  clockwise. Switching workflows never changes the clip; only the numbers shown differ. In the
  Resolve workflow Effect Controls itself, with its keyframe lane, is called **Keyframes**.

## Deliver: render settings and the render queue

> id: pages.deliver | does: render settings and a render queue on the Deliver page | ui: export.*, page.deliver | cmd: export.queue.start, file.exportMedia, page.deliver | resolve: Deliver page render settings and queue | premiere: Export mode, Adobe Media Encoder queue | when: turning the timeline into files, several at once | see: pages.what

**[Resolve-style]** layout, **[Revolution]** export settings

### Why

The Deliver page is where a finished timeline becomes a file. The settings are Revolution's export
settings ([export.md](export.md)), the same ones File ▸ Export ▸ Media shows; in the Resolve
workflow that menu item (and Ctrl+M, `mode.export`) opens the Deliver page.

### Steps

1. Click **Deliver** in the page bar ({ui:page.deliver}).
2. In **Render Settings** ({ui:pageToolbar.RenderSettings} shows or hides it) set the File Name
   ({ui:export.fileName}), the Location and a Preset ({ui:export.preset}).
3. Choose the **Range** under the settings ({ui:export.range}): the entire timeline, In to Out,
   work area or custom.
4. Click **Add to Render Queue** ({ui:export.addToQueue}) to queue it, or **Render**
   ({ui:export.render}) to export now (`file.exportMedia`).
5. The **Render Queue** on the right ({ui:pageToolbar.RenderQueue}) lists the jobs; **Start
   Render** ({ui:export.queue.start}) works through them (`export.queue.start`).

If you know Premiere: this is the Export mode's Media File settings and queue, arranged around
the Timeline Viewer; the Summary box is replaced by the estimated file size under the settings.

## Rearrange a page and keep it

> id: pages.layout | does: moves and resizes panels on a page and saves or resets the layout | cmd: window.workspace.saveChanges, window.workspace.reset, window.workspace.delete | resolve: Workspace ▸ Reset UI Layout | premiere: Workspaces ▸ Save Changes / Reset | when: making a page fit your screen and habits | see: pages.toolbar

**[Revolution]** workspaces, **[Fork]** page layouts

### Why

The page layouts are starting points. Drag a panel's tab to move it, drag a gutter to resize, and
save the result so the page opens your way every time.

### Steps

1. Change the page: drag tabs and gutters, or use the toolbar.
2. Choose Window ▸ Workspaces ▸ **Save Changes to this Workspace**
   (`window.workspace.saveChanges`). The page's workspace (for example "Edit Page") keeps the
   new layout.
3. To go back to the original, choose Window ▸ Workspaces ▸ **Edit Workspaces…**, select the
   page's workspace and delete its saved changes (`window.workspace.delete`), or **Reset to
   Saved Layout** (`window.workspace.reset`) to undo unsaved changes.

In the Resolve workflow Window ▸ Workspaces lists the pages' workspaces (Media Page, Edit Page,
Fairlight Page, Deliver Page, Color Grading) instead of Assembly, Editing, Audio and Color;
choosing one of those names by command or key opens its page. Back in the Premiere workflow the
page workspaces are not listed and the Premiere workspaces are exactly as they were. Switching
the workflow keeps each workflow's layout as you left it, unsaved changes included, and puts it
back when you return. A workspace of your own saved before this release under one of the page
names (for example "Edit Page") is renamed "Edit Page (custom)" once, so it stays in the list.
In this workflow the Import mode (`mode.import`) opens the Media page.
