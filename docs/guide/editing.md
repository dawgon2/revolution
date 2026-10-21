# Editing

**[Revolution] [Resolve-style] [Fork]** · cutting on the timeline, JKL shuttle, trimming and
multicam; written for the fork after Phase 6 (GUIDE-7)

The editing tools come from upstream FilmCraft, which models them on Premiere. The fork defaults to
the **Resolve** workflow (Settings ▸ Workflow), which changes the keyboard preset and some words
on screen (Sequence → Timeline, Project panel → Media Pool) but not what the tools do. Shortcuts
below are given for both presets; every command can also be run by its id from the command
palette, the control channel or MCP.

If playback stutters while you cut heavy footage, don't fight it here: let Revolution make proxies
([proxies.md](proxies.md)) or render the busy part into the cache
([render-cache.md](render-cache.md)).

## There is no single right order

> id: editing.orders | does: common ways to cut a film, from assembling everything first to cutting scene by scene | resolve: Cut and Edit pages | premiere: Editing workspace | when: starting an edit | see: editing.cutting

**[Fork]**

People start cutting in very different ways (say with 3 hours of footage for a 10-minute video),
and all of them work. Two common ones:

- **Rough first, then trim** (good for beginners and long material): lay every usable take on the
  timeline in order, then cut away what you don't need, then tighten each edit. Easy to follow,
  but you handle a lot of material twice.
- **Three-point editing** (common with professionals and interviews): watch each clip in the
  Source monitor, mark only the part you want, and drop just that part into the timeline. Less
  cleanup later, but you decide more up front.

Mixing the two is normal. Try both on a short project and keep what feels natural.

## Cutting clips on the timeline

> id: editing.cutting | does: splits a clip so a part can be removed, moved or trimmed | cmd: tool.razor, sequence.addEdit, sequence.addEditAllTracks, edit.rippleDelete, sequence.lift, sequence.extract | resolve: Blade, Split Clip | premiere: Razor, Add Edit | when: removing a part of a clip; cutting through video and all audio tracks at once | see: editing.trimming

### Why

**[Revolution]**

A cut splits one clip into two, so you can remove, move or trim one part without touching the
other. You can cut where the playhead is, or click exactly where you want with the razor tool.

### Steps: cut at the playhead

**[Revolution]** · Resolve keys **[Resolve-style]**

1. Move the playhead to the frame where the cut should go (click the time ruler, or step with the
   Left / Right arrow keys).
2. Run **Add Edit** (`sequence.addEdit`): Cmd+K in the Premiere preset, Cmd+B in the Resolve
   preset (Cmd is Ctrl on Linux and Windows). It cuts the targeted tracks at the playhead.
3. To cut every track at once, use **Add Edit to All Tracks** (`sequence.addEditAllTracks`).
4. Check the result: a thin line now shows the edit point on the clip.

### Steps: cut with the razor tool

**[Revolution]**

1. Pick the razor tool (`tool.razor`): **C** in the Premiere preset, **B** in the Resolve preset.
2. Click the clip where you want the cut.
3. Go back to the selection tool (`tool.selection`): **V** in Premiere, **A** in Resolve.

### Removing material

**[Revolution]**

- **Delete** (`edit.clear`) removes the selected clips and leaves a gap.
- **Ripple Delete** (`edit.rippleDelete`, Shift+Delete) removes them and closes the gap.
- With In and Out marks set on the timeline, **Lift** (`sequence.lift`) removes that range and
  leaves a gap, **Extract** (`sequence.extract`) removes it and closes the gap.

Lift and Extract are on ; and ' in the Premiere preset; the Resolve preset gives them no key of
their own yet, so run them from the Sequence (Timeline) menu or the command palette.

## Three-point editing

> id: editing.three-point | does: sets three of the four In / Out points and lets Revolution work out the fourth | cmd: markers.markIn, markers.markOut, source.insert, source.overwrite | resolve: three-point editing, Insert / Overwrite | premiere: three-point editing, Insert / Overwrite | when: placing exactly the part of a clip you want | see: editing.jkl

### Why

**[Revolution]**

Three-point editing means you set three of the four points (Source In, Source Out, Timeline In,
Timeline Out) and Revolution works out the fourth. It is the quickest way to put only the good
part of a clip into the edit.

### Steps: insert or overwrite a marked part of a clip

**[Revolution]** · Resolve keys **[Resolve-style]**

1. Double-click a clip in the Project panel / Media Pool to open it in the Source monitor.
2. Play to the start of the part you want and press **I** (`markers.markIn`), then to the end and
   press **O** (`markers.markOut`).
3. On the timeline, move the playhead to where the clip should go and switch on the target tracks
   (the track headers' source / target buttons) for the tracks it should land on.
4. Press **Insert** (`source.insert`): comma in Premiere, F9 in Resolve. Everything after the
   playhead moves later to make room.
   Or press **Overwrite** (`source.overwrite`): period in Premiere, F10 in Resolve. It replaces
   whatever is under it and moves nothing.
5. Check the timeline: the new clip starts at the playhead and has the length you marked.

**X** (`markers.markClip`) marks the whole clip under the playhead; Alt+X
(`markers.clearInOut`) clears both marks.

## JKL: playing and shuttling with three keys

> id: editing.jkl | does: J, K and L play backwards, stop and play forwards, faster with each press | cmd: playback.reverse, playback.stop, playback.forward, playback.toggle | resolve: JKL | premiere: JKL | when: finding frames fast without the mouse | see: editing.three-point

### Why

**[Revolution]** · same keys in Premiere and Resolve

Your left hand stays on J, K and L, so you can find frames fast without the mouse. Each key press
in the same direction speeds playback up, which is the fastest way to skim long material.

### Steps: shuttle with J, K and L

**[Revolution]**

1. Press **L** (`playback.forward`) to play forward at normal speed.
2. Press **L** again: each extra press **doubles** the speed (2×, 4×, up to 8×).
3. Press **K** (`playback.stop`) to stop.
4. Press **J** (`playback.reverse`) to play backward; pressing it again doubles the reverse speed
   in the same way.
5. Press **Space** (`playback.toggle`) to play or pause at normal speed.
6. Use the **Left / Right** arrow keys to step one frame at a time once you are close.

The keys act on the monitor you last clicked: the Source monitor or the timeline / Program
monitor. At 2× and faster, 6.2K originals may skip frames on playback; proxies keep fast shuttling
smooth ([proxies.md](proxies.md)).

**Difference from Resolve and Premiere:** both of them also have a "hold K and tap J / L" frame
step and a slow shuttle (K+L held).

- **Revolution:** no "hold K and tap J / L" frame step yet, and no K+L slow shuttle on normal
  playback: slow shuttle works while trimming (Shift+J / Shift+L in trim mode, quarter speed,
  below).

## Trimming

> id: editing.trimming | does: moves edit points with ripple, roll, slip and slide | cmd: tool.ripple, tool.rolling, tool.slip, tool.slide, trim.forward, trim.backward, trim.edit | resolve: Trim edit mode | premiere: Ripple, Rolling, Slip and Slide tools | when: tightening a cut without cutting again | see: editing.cutting

### Why

**[Revolution]**

Trimming moves an edit point: it makes one clip a bit longer or shorter without cutting again.
Most of the polish in an edit happens here. The four trim tools differ in what else moves:

| Tool | Command | What changes | Premiere key |
|---|---|---|---|
| **Ripple** | `tool.ripple` | one clip gets longer / shorter, everything after it moves | B |
| **Rolling** | `tool.rolling` | the cut point moves between two clips, total length stays the same | N |
| **Slip** | `tool.slip` | which part of the clip is shown changes; its position and length stay | Y |
| **Slide** | `tool.slide` | the clip moves between its neighbours, which get longer / shorter | U |

In the Resolve preset the selection tool **A** already trims (drag an edit point) and **B** is
the blade, so the trim tools are reached from the toolbar or the command palette.

### Steps: trim an edit point with the keyboard

**[Revolution]**

1. Click an edit point (the line between two clips) with the selection tool, or press
   **Trim Edit** (`trim.edit`, Shift+T in Premiere) to select the nearest one under the playhead.
2. Press **Toggle Trim Type** (`trim.toggleType`, Ctrl+T) until the edit point shows the trim you
   want (ripple, rolling, or ripple on one side).
3. Press **Alt+Left** / **Alt+Right** (`trim.backward` / `trim.forward`, Premiere preset) to
   move it one frame, or add Shift (`trim.backwardMany` / `trim.forwardMany`) to move it several
   frames. A rolling edit point moves the cut and keeps the sequence length; a ripple one changes
   one side and shifts everything after it.
4. Or press **L** / **J** while the edit point is selected: the trim plays live (dynamic
   trimming), and **K** stops and keeps it. Shift+J / Shift+L shuttle at quarter speed.
5. Undo (Cmd+Z) puts the whole trim back in one step. To change which part of a clip shows
   instead, use Slip (`tool.slip`); Slide (`tool.slide`) moves the clip between its neighbours.

## Multicam

> id: editing.multicam | does: puts several cameras of one moment in one clip and cuts between angles live | cmd: clip.createMulticam, multicam.switchAngle, multicam.cut, multicam.recordToggle, clip.multicamFlatten | resolve: Multicam clips | premiere: Multi-Camera Source Sequence | when: interviews, events, music filmed with several cameras | see: proxies.what

### Why

**[Revolution]** · Resolve-style notes **[Resolve-style]**

When several cameras recorded the same moment, multicam puts them in one clip with **angles**. You
then watch all angles side by side and switch between them while playing, like a live vision
mixer, instead of stacking and cutting tracks by hand.

Revolution follows Premiere's model (a "Multi-Camera Source Sequence"). Resolve's equivalent is a
**Multicam clip**, made from the Media Pool; the steps are very similar.

### Steps: make a multicam clip and cut it

**[Revolution]**

1. In the Project panel / Media Pool select the clips from all cameras.
2. Run **Clip ▸ Create Multi-Camera Source Sequence…** (`clip.createMulticam`). Pick how the
   clips are lined up: by **In** or **Out** points, **timecode**, a **marker**, or by **audio**
   (best when the cameras had no shared timecode).
3. Put the new multicam clip on the timeline (drag it, or Insert / Overwrite as above).
4. Switch the Program monitor to the multicam view (`multicam.toggleView`, Shift+0 in Premiere)
   so all angles show as a grid.
5. Press **0** (`multicam.recordToggle`) to start recording switches and play the timeline.
   Click an angle in the grid, or press **1–9** (`multicam.selectCamera1` …), to cut to that
   camera. Press **0** again (or stop playback) to finish; the whole pass is one undo step.
6. Refine: every switch is an ordinary edit point, so the trim tools above work on it. While
   stopped, **1–9** switch the angle of the clip at the playhead, and **Ctrl+1–9**
   (`multicam.cutToCamera1` …) add a cut at the playhead and show that camera from there.
   These keys are from the Premiere preset.

`multicam.cut` and `multicam.switchAngle` do the same from the command line / MCP, with a
`camera` number (1-based) or an `angle` (0-based). **Flatten** (`clip.multicamFlatten`) turns a
finished multicam edit into plain clips.

Heavy footage: playing four 6.2K angles at once is far beyond what one graphics card decodes in
real time, so multicam is where proxies help most ([proxies.md](proxies.md)).

## Resolve edit commands

> id: editing.resolve-edits | does: places, replaces and fits a clip with Resolve's edit set | cmd: edit.insert, edit.overwrite, edit.replace, edit.placeOnTop, edit.append, edit.rippleOverwrite, edit.fitToFill, edit.smartInsert, edit.closeUp, edit.sourceOverwrite | resolve: Insert, Overwrite, Replace, Place on Top, Append, Ripple Overwrite, Fit to Fill, Smart Insert, Close Up, Source Overwrite | premiere: Insert, Overwrite, Replace With Clip | when: you edit the Resolve way, or on the Cut page | see: editing.three-point

### Why

**[Resolve-style]**

Resolve has one command for each common way of getting a clip onto the timeline. They all work
from the same three things: the source (the Source monitor's clip with its In and Out), the
destination tracks (the ones patched in the track headers) and the record point (the timeline In
mark when there is one, otherwise the playhead). Each also takes an optional `clip` (a project item
id) so a clip picked in the Media Pool can be edited in without opening it first.

### What each one does

**[Resolve-style]** · each is one undo step

- `edit.insert` (F9) and `edit.overwrite` (F10) are the three-point Insert and Overwrite.
- `edit.replace` (F11) swaps the clip under the playhead on the destination track for the source,
  lining up the Source monitor frame with the timeline playhead. The new clip is as long as the
  old one, or shorter if the media runs out.
- `edit.placeOnTop` (F12) overwrites onto the track above the highest one that has a clip at the
  record point, adding a track when needed.
- `edit.append` (Shift+F12) puts the source at the end of the timeline.
- `edit.rippleOverwrite` (Shift+F10) swaps the clip under the playhead for the whole source and
  moves everything after it by the difference in length.
- `edit.fitToFill` (Shift+F11) needs a timeline In and Out and changes the clip's speed so the
  whole marked source fills them.
- `edit.smartInsert` (Cut page) inserts at the clip boundary nearest the playhead.
- `edit.closeUp` (Cut page) puts the source on the track above at the playhead, zoomed in to 150%.
- `edit.sourceOverwrite` (Cut page) puts the source on the track above so its source timecode meets
  the timecode of the clip under the playhead; it stops with a message when they do not overlap.

In the Resolve key preset F11 is Replace, so full screen there is Ctrl+`.
