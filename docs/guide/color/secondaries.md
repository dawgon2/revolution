# Secondaries: qualifier, windows, tracking, colour warper

**[Resolve-style]** · limiting a node to part of the picture (Phase G2; this page grows with it)

A *primary* correction changes the whole picture. A *secondary* changes only part of it: the
skin, the sky, a face, one corner. In the node grader a secondary is simply a node whose
correction is limited by a **qualifier** (a colour selection) and, later in G2, by **windows**
(shapes) that can be **tracked**.

## The qualifier: select by colour

> id: color.secondaries.qualifier | does: limits a node's correction to a range of hue, saturation and brightness | ui: grade.qual.*, grade.param.qual.*, grade.tab.qualifier | cmd: grade.pickQualifier, grade.setParam | resolve: Qualifier (HSL) | premiere: Lumetri HSL Secondary | when: skin, sky, one colour to change or protect | see: color.secondaries.windows

**[Resolve-style]**

### Why

Use a qualifier when the part you want to change has its own colour: skin, a blue sky, green
grass, a red jacket. The qualifier builds a **matte** (white where the node acts, black where it
doesn't) from the node's *input* picture, and the node's corrections (wheels, sliders, curves,
grain) then apply only inside that matte.

### Steps

1. Select the clip and add a node for the secondary (`grade.addNode`, **Add Node**). One job per
   node: keep the secondary on its own node, after your balance and exposure nodes.
2. In **Primaries**, open the **Qualifier** tab {ui:grade.tab.qualifier}.
3. Click **Pick** {ui:grade.qual.pick}, then click the colour you want in the Program picture.
   The command `grade.pickQualifier` samples the node's input there and switches the qualifier
   on with a hue, saturation and luminance range around it.
4. Click **Add** {ui:grade.qual.add} and click another part of the same object to widen the
   selection (`grade.pickQualifier` with `"mode": "add"`). Esc cancels an armed pick.
5. To see what is selected, set **View** to **Selection** {ui:grade.qual.show.1} (the selected
   area in colour, the rest grey) or **Matte** {ui:grade.qual.show.2} (white = selected).
   Switch back to **Normal** {ui:grade.qual.show.0} before you judge the grade.
6. Refine with the sliders: **Hue centre** / **Hue width** {ui:grade.param.qual.hueWidth},
   **Saturation low / high**, **Luminance low / high**, **Qualifier softness**
   {ui:grade.param.qual.soft} (how gradual the matte's edges are) and **Matte blur**
   {ui:grade.param.qual.blur} (softens a noisy, speckled matte). Edges that flicker or chatter
   on movement need more softness or blur, not a wider range; if the matte still catches other
   things, limit it with a window ([Windows](#windows-select-by-shape)).
7. Tick **Invert** {ui:grade.qual.invert} to act on everything *except* the selection.
8. Go back to the **Primaries** tab and make the correction; it lands only inside the matte.

### Tips

- Pick on the node's *input*: the picker sees the picture before this node and before any
  Lumetri / effects, the same picture the qualifier keys.
- On F-Log2 footage, a qualifier on a node *before* your normalising node works on the flat log
  picture (low saturation): it still works, but the ranges are narrower. Most people qualify
  after the image is normalised.
- A selection that flickers or crawls usually needs more **Qualifier softness** or a little
  **Matte blur**, not a wider range.
- Skin gone grey or blotchy after a secondary (on the sky, say) means the node's selection
  reaches into the face. Set **View** to **Matte** {ui:grade.qual.show.2}: the face should be
  black. Narrow **Hue width** {ui:grade.param.qual.hueWidth} or raise **Qualifier softness**
  until it is.

### Parameters (for scripts)

**[Fork]**

All are node parameters set with `grade.setParam` (0…1 unless noted): `qual.on` (switch, ≥ ½ is
on), `qual.hue`, `qual.hueWidth` (1 = every hue), `qual.satLow`, `qual.satHigh`, `qual.lumLow`,
`qual.lumHigh` (ranges on the node's input as HSL of the grading signal), `qual.soft`
(0.001…0.5, edge width), `qual.blur` (σ up to 20 pixels of the sequence frame), `qual.invert` (switch),
`qual.show` (0 normal, 1 selection, 2 matte). A range that reaches 0 or 1 has no soft edge at that
end, so the full default range selects everything, black and white included.

### If you know Resolve

**[Resolve-style]**

- Like Resolve's HSL qualifier: hue / saturation / luminance ranges, softness, matte blur,
  invert, a highlight view, and picker / add tools on the viewer.
- **Revolution:** no "subtract" picker and no RGB / luma-only qualifier mode yet; the 3D
  qualifier has its own section ([3D qualifier](#3d-qualifier-select-by-painting-over-colours)).
- Differences: the softness is one value for every edge; **View ▸ Selection / Matte is saved with the node**
  and shows on the Program monitor *and in an export* (unlike Resolve's viewer-only highlight),
  and nodes after the one being viewed are skipped while it is on. Turn it off before exporting.
- The picker doesn't work on a grade on an adjustment layer yet (set the ranges by hand there).
- The matte is not shown as a separate node output yet (key output / key mixer come with
  COMPOSITOR-PLAN C2).

### If you know Premiere

**[Revolution]**

Lumetri Color's **HSL Secondary** is the same idea inside one effect. In the node grader every
node can carry its own qualifier, and a node's whole correction (wheels, curves, grain) is
limited by it, not only the secondary's own hue / saturation sliders.

## 3D qualifier: select by painting over colours

> id: color.secondaries.qualifier-3d | does: selects the colours you paint strokes over (a soft volume in colour space), with exclude strokes, softness and despill | ui: grade.qual3d.*, grade.tab.qualifier | cmd: grade.qual3d.addStroke, grade.qual3d.set, grade.qual3d.clear | resolve: 3D qualifier | when: a colour the ranges can't isolate cleanly, green or blue screen spill, skin next to similar wood or sand | see: color.secondaries.qualifier

**[Resolve-style]**

### Why

The qualifier above selects a box of hue, saturation and luminance ranges. Some colours don't fit
in a box: a jacket whose lit and shaded parts differ in hue and brightness, skin standing in
front of a similar-coloured wall. The **3D qualifier** lets you show it the colours instead:
you paint strokes over the colours you want, and it selects those colours and the ones close to
them, wherever they are in the picture. Strokes over colours to leave out (the wall) take them
back out. It is often quicker than tuning six sliders, and it follows the real spread of the
colours you painted.

### Steps

1. Select the node for the secondary and open the **Qualifier** tab {ui:grade.tab.qualifier}.
2. Set the key to **3D** {ui:grade.qual3d.mode.3d}. (**HSL** {ui:grade.qual3d.mode.hsl} goes back
   to the ranges; the strokes are kept, so you can switch back and forth.)
3. Click **Include** {ui:grade.qual3d.include}, then drag across the colour you want in the
   Program picture: a short line over the lit and the shaded part of it is better than one
   click. Each stroke is one undo step (`grade.qual3d.addStroke`); the first one switches the
   qualifier on. A click without dragging picks one spot. While the Qualifier tab shows the 3D
   key, the strokes you made stay drawn on the Program picture (**[Fork]**): include strokes as
   solid green lines, exclude strokes as dashed red ones, a single spot as a dot (green) or a
   ring (red) {ui:program.qual3d.stroke.0}, so you can see what you have already picked.
4. Set **View** to **Matte** {ui:grade.qual.show.2} or **Selection** {ui:grade.qual.show.1} to
   see what is selected. Paint more **Include** strokes over the parts that are still missing.
5. If it selects something you don't want, click **Exclude** {ui:grade.qual3d.exclude} (or hold
   Alt while you drag an Include stroke) and paint over that colour.
6. **Softness** {ui:grade.qual3d.softness} widens the soft edge around the painted colours:
   more selects more of the nearby shades and gives smoother edges; less is tighter. **Matte
   blur** {ui:grade.param.qual.blur} and **Invert** {ui:grade.qual.invert} work as with the
   ranges.
7. **Despill** {ui:grade.qual3d.despill} pulls the painted colour's tint out of the pixels the
   node corrects. With **Invert** on, the node corrects everything *but* the painted colour, so
   despill removes a green screen's green cast from the subject's edges and hair.
8. Switch **View** back to **Normal** {ui:grade.qual.show.0} and grade on the Primaries tab.
   **Clear Include** {ui:grade.qual3d.clear.include}, **Clear Exclude**
   {ui:grade.qual3d.clear.exclude} or **Clear All** {ui:grade.qual3d.clear} begin again
   (`grade.qual3d.clear`; Clear All goes back to the ranges).

### Options and trade-offs

- **Strokes or ranges.** Strokes are faster for a colour with a natural spread (skin in mixed light, foliage,
  a screen) and when two colours are close in hue but differ in saturation or brightness. The ranges
  are easier to tweak by number and to explain, and keyframes (its ranges are node parameters);
  the 3D strokes are not keyframed, but **Softness** and **Despill** are (**[Fork]**): click the
  diamond after the slider {ui:grade.qual3d.softness.anim} {ui:grade.qual3d.despill.anim}
  (`grade.toggleAnimation` with `param` `q3d.softness` / `q3d.despill`), then move the
  playhead and change the value to add a keyframe there. Beginners often begin with 3D strokes and only reach for
  the range sliders when they need a precise range.
- **Few long strokes, not many clicks.** A stroke samples every pixel it crosses, so one drag
  across the light and shade of an object teaches the key more than ten clicks.
- **Despill or a correction.** Despill only removes the painted colour's tint (brightness and
  other tints stay). To change the colour itself (hue, saturation), grade it on the node.
- For scripts: `grade.qual3d.addStroke` takes `points` (`[[x, y], …]`, 0…1 of the frame) and
  `exclude`; `grade.qual3d.set` takes `mode` (`"hsl"` / `"3d"`), `softness` (0.001…0.5) and
  `despill` (0…1); once a setting is animated, `grade.qual3d.set` (or `grade.setParam`) sets a
  keyframe at `time` (else the playhead). The node keeps at most 64 colour clusters each for include and exclude, and a stroke uses at
  most 4096 samples (a longer stroke is sampled evenly along its length).

### If you know Resolve

**[Resolve-style]**

- Like Resolve's 3D qualifier: strokes on the viewer pick colours, strokes of another colour
  take colours out, a soft edge around the selection, despill, and the same matte blur, invert,
  highlight view and key output as the other qualifiers.
- Differences (**[Fork]**): the strokes are drawn on the viewer while the Qualifier tab shows
  the 3D key, but the node keeps only the newest 32 of them for drawing (each thinned to 64
  points; the selection itself keeps every stroke's colours); you can't select or delete one
  stroke, only clear the include or
  exclude strokes; no "show paths" or colour-space choice (it always works on the node's input as
  the grading signal in a fixed opponent space where brightness counts half as much as
  colour); softness is one value; despill acts inside the node's matte, as the node's first
  correction. The maths is our own: the painted colours are grouped into at most 64 clusters,
  each a small sphere in colour space, and the selection is the union of those spheres with a
  smooth edge, minus the excluded ones.

## Windows: select by shape

> id: color.secondaries.windows | does: limits a node's correction to a shape: ellipse, rectangle or linear gradient | ui: grade.window.*, grade.tab.windows | cmd: masks.* | resolve: Power Windows | premiere: Lumetri masks | when: brightening just a face, vignettes, a sky gradient; combined with a qualifier | see: color.secondaries.qualifier, color.secondaries.tracking

**[Resolve-style]**

### Why

Use a window when the part you want has a *place* rather than a colour: brighten a face,
darken the sky with a graduated edge, add a vignette around the subject. A window is a shape on
the picture; the node acts inside it (or outside, inverted). With a qualifier on the same node,
the node acts only where **both** select (skin *and* inside the circle round the face).

### Steps

1. Select the node and open the **Windows** tab {ui:grade.tab.windows}.
2. Click **Circle** {ui:grade.window.add.ellipse}, **Rectangle** {ui:grade.window.add.rectangle}
   or **Linear** {ui:grade.window.add.linear} (a straight soft edge across the frame, like a
   graduated filter). This runs `masks.add` with `"node"`; the new window is selected.
3. Drag the window's shape and handles on the Program monitor (the same handles as effect masks).
   Click a window's name {ui:grade.window.0} to select it again.
4. Set **Softness** {ui:grade.window.0.softness} (the feather, in source pixels) and tick
   **Invert** {ui:grade.window.0.invert} to act outside the shape.
5. Correct the picture in **Primaries**; the change stays inside the window.
6. **✕** {ui:grade.window.0.delete} deletes a window. A node holds up to 16 windows; several
   windows combine by their mask mode (add, subtract, intersect, …) like effect masks.

### For scripts

**[Fork]**

Windows are masks: every `masks.*` command takes `"node": <node id>` instead of `"effect"`
(`masks.add` with `"shape": "ellipse" | "rectangle" | "polygon" | "bezier" | "linear"`, `masks.set`,
`masks.select`, `masks.remove`, `masks.moveVertex`, `masks.list` lists windows with their
`node`). Paths are in the clip's source pixels.

### If you know Resolve

**[Resolve-style]**

- Like Resolve's Window palette: circle, linear, polygon / curve shapes, softness, invert, and
  the window limiting the node together with its qualifier.
- Differences: shapes are Revolution's mask paths (a circle is a 4-point Bézier ellipse; there is
  no separate inside / outside softness), windows are listed per node rather than in a fixed
  palette, and there is no "outside node" yet (COMPOSITOR-PLAN C2).

## Tracking a window

> id: color.secondaries.tracking | does: makes a window follow its subject from frame to frame | ui: grade.window.0.track.* | cmd: masks.track | resolve: Tracker (Window) | premiere: mask tracking | when: a window on something that moves | see: color.secondaries.windows, tracking.what

**[Resolve-style]** **[Revolution]**

### Why

When the subject moves (a person walking, a car passing), a window has to follow it. The tracker follows the detail inside the
window from frame to frame and keyframes the window's shape.

### Steps

1. Put the playhead on a frame where the window sits well on the subject.
2. In the **Windows** tab, click **▶** (Track forward) {ui:grade.window.0.track.forward} or
   **◀** (Track backward) {ui:grade.window.0.track.backward}. This runs `masks.track` with
   `"node"` in the background; the window gets a keyframe per frame.
3. Scrub through the shot to check it. If the track drifts, move the window back on the frame
   where it went wrong and track again from there (the new keyframes replace the old ones).

### Notes

- Tracking uses Revolution's point tracker (features inside the window, robust fit; method
  Position / Position + Rotation / Position + Scale + Rotation from `masks.set`'s
  `trackMethod`). It needs some detail inside the window: a window on a flat wall will "lose
  track".
- For a flat surface seen in perspective (a sign, a screen, a wall) use **Planar** below: the
  point tracker can follow a window's position, turn and size, but not its tilt.
- **Revolution:** no separate tracker graph to edit by hand yet (unlike Resolve).
- Copying a grade to another clip (`grade.paste`, `grade.append`) copies its windows as they are:
  same position in source pixels and the same tracking keyframes (and planar track). On a clip
  of another size or with other motion, move or re-track the windows after pasting.

## Planar-tracked windows

> id: color.secondaries.planar | does: makes a window stick to a flat surface (sign, screen, wall) as the camera moves, perspective included | ui: grade.window.0.track.planar, grade.window.0.track.clearPlanar | cmd: masks.trackPlanar, masks.clearPlanar | resolve: Tracker (Window), Perspective 3D | premiere: none (mask tracking has no perspective) | when: a window on something flat that the camera moves around or past, so it moves in perspective | see: color.secondaries.tracking, color.secondaries.windows

**[Resolve-style]** · fork G4.4

### Why

A window on a shop sign, a phone screen or a painted wall cannot just slide: as the
camera moves, the surface tilts, so its outline narrows on one side and widens on the other. A
planar track measures that whole-surface motion (a *homography* per frame: position, turn, size
and tilt together), and the window is drawn through it, so its corners stay on the surface's
corners. Because the tracker compares every frame with the frame you started on, small errors
never pile up over a long shot.

### Steps

1. Put the playhead on a frame where you can see the surface well, and draw the window on it
   (**Add window** → **Rectangle** {ui:grade.window.add.rectangle} and drag its corners onto the
   surface's corners, or any shape inside the surface).
2. Make sure the window covers *texture*: print, edges, grain of a wall. A plain white board has
   nothing to follow; a window bigger than the surface also follows what is behind it.
3. Click **Planar** {ui:grade.window.0.track.planar}. This runs `masks.trackPlanar` in the
   background, forward and backward from the playhead, to both ends of the shot. While it runs the
   button reads **Stop**: click it to stop; what was tracked so far is kept. The whole track is
   one undo step.
4. Scrub through the shot. The window's handles on the Program monitor sit where the window is
   seen, and dragging them reshapes it on the surface (the tilt keeps being applied).
5. If it slips on part of the shot, put the playhead on a good frame just before the slip, fix the
   window there if needed and click **Planar** again: only the frames it tracks again are
   replaced, starting from where the window was, so nothing jumps.
6. To stop following the surface, click **✕ planar** {ui:grade.window.0.track.clearPlanar}
   (`masks.clearPlanar`): the window stays where it is seen at the playhead (and at each of its
   keyframes).

### Options and trade-offs

- **Planar or point tracking.** The point tracker (▶ / ◀) is the better choice for things that
  aren't flat (a face, a person, a car turning): it fits only position, turn and size. Planar is
  the better choice for flat things, and it is steadier on long shots because it never drifts.
- **Both ways or one way.** Planar tracks both ways by default, which is what you want when you
  drew the window in the middle of the shot. From scripts, `"direction": "forward"` or
  `"backward"` and `"frames": n` limit it (for a quick test on a long shot).
- **When the surface leaves the frame or is covered**, the tracker keeps the last good position
  for up to 48 frames and picks the surface up again when it comes back; after that it stops
  in that direction.

### For scripts

- `masks.trackPlanar` `{"node": id, "mask": n, "direction": "both"|"forward"|"backward", "frames": n, "wait": true}`
  (any mask works, an effect's too: pass `effect` instead of `node`). `jobs.cancel` stops it.
- **[Fork]** An effect's mask has the same buttons in **Effect Controls**, on its **Mask Path**
  row: **Planar** {ui:effectControls.gaussian_blur.mask0.track.planar} and **✕ planar**
  {ui:effectControls.gaussian_blur.mask0.track.clearPlanar} (here a Gaussian Blur's first mask).
- `masks.list` shows a tracked mask's `planar` (`reference`, `keys`, `from`, `to`) and its
  `shownPath` (where it is at `time`). Its `path` is the shape on the frame it was tracked from;
  `masks.translate`, `masks.moveVertex` and `masks.addVertex` take positions where it is seen.
- The track is saved in the project with the window; projects without one are unchanged.

### If you know Resolve

**[Resolve-style]**

- Like Resolve's window Tracker with its perspective option: the window follows a surface's
  tilt, not just its position, size and turn, and tracking runs forward and backward from the
  playhead.
- Differences: Revolution's planar tracker is a separate, surface-based tracker (it re-checks
  each frame against the frame you started from) rather than a point cloud with a "3D" mode; it
  tracks what is inside the window, not a separate region you draw; there are no per-axis
  check boxes (pan / tilt / zoom / rotate / 3D) — it is always the whole perspective motion, and
  the point tracker (▶ / ◀) is the "fewer axes" option; there is no tracker graph to
  edit by hand yet (fix a slip by tracking again from a good frame).

## Magic Mask

> id: color.secondaries.magic-mask | does: finds the person or object you mark with strokes and tracks it through the shot, so a node acts on just that | ui: grade.tab.magicMask, grade.magicMask.* | cmd: grade.magicMask.* | resolve: Magic Mask | when: neither a qualifier nor a window can isolate it | see: color.secondaries.qualifier, color.secondaries.tracking

**[Resolve-style]** **[Fork]**

### Why

Some subjects have neither their own colour nor a simple shape: someone walking through a
crowd, a car turning, a hand. Magic Mask asks a segmentation model to find "the thing under these
strokes" on one frame, then follows it frame by frame. The node's correction then acts only on
the subject (or, inverted, on everything else).
With a window or qualifier on the same node, the node acts only where all of them select.

### Steps

1. Select the node and open the **Magic Mask** tab {ui:grade.tab.magicMask} (next to Windows).
2. Pick **Object** {ui:grade.magicMask.mode.object} (whatever the strokes cover) or **Person**
   {ui:grade.magicMask.mode.person} (the whole figure the strokes pick), then click
   **Add Magic Mask** {ui:grade.magicMask.add} (`grade.magicMask.add`). The stroke tool is ready.
3. Put the playhead on a frame where the subject is clear. With **Stroke +**
   {ui:grade.magicMask.tool.add}, click the subject in the Program picture, or drag a box round it
   (`grade.magicMask.stroke`). Alt-click, or **Stroke −** {ui:grade.magicMask.tool.subtract},
   marks a part to leave out. The strokes of the frame are drawn on the picture; Esc puts the
   tool down.
4. Pick the **Model** (see the trade-offs below): **Auto** {ui:grade.magicMask.model.auto}
   (its label says what it will use, for example "Auto (Better)"), **Better**
   {ui:grade.magicMask.model.better}, **Faster** {ui:grade.magicMask.model.faster} or
   **MobileSAM** {ui:grade.magicMask.model.mobileSam}. Revolution remembers the choice for the
   next time you open it (preference `color.magicMask`); a `grade.magicMask.track` call can also
   name one with `model` for that track only.
5. Click **Track** {ui:grade.magicMask.track} (both directions), **Track Forward**
   {ui:grade.magicMask.trackForward} or **Track Backward** {ui:grade.magicMask.trackBackward}
   (`grade.magicMask.track`). A bar shows the progress and a line under it says how it went and
   where the subject was lost, plus the model that actually ran (`grade.magicMask.status`). If
   SAM 2.1 can't load or run, a line under it says so and that MobileSAM took over. **Stop**
   {ui:grade.magicMask.cancel} (`grade.magicMask.cancel`) keeps the frames done so far.
6. Tick **Show matte** {ui:grade.magicMask.overlay} to see the tracked matte tinted over the
   picture. Where it goes wrong, add a stroke on that frame and track again: only the frames
   between that frame and its neighbouring stroke frames run again. With SAM 2.1, a fix stroke
   also goes into its memory of the subject, so the frames after it benefit too.
7. Under **Refine**, shape the matte without running the model again
   (`grade.magicMask.setRefine`): **Grow / Shrink** {ui:grade.magicMask.refine.grow}, **Soften**
   {ui:grade.magicMask.refine.soften} (the edge), **Clean Black**
   {ui:grade.magicMask.refine.cleanBlack} and **Clean White** {ui:grade.magicMask.refine.cleanWhite}
   (snap weak parts to out / in), **Invert** {ui:grade.magicMask.refine.invert} (act outside the
   subject). A slider drag is one undo step.
8. Start over with **Clear Frame** {ui:grade.magicMask.clearFrame} (this frame's strokes),
   **Clear All** {ui:grade.magicMask.clearAll} (`grade.magicMask.clearStrokes`) or **Remove**
   {ui:grade.magicMask.remove} (`grade.magicMask.remove`).

### Options and trade-offs

- **Object or Person.** With a box the two behave the same: the box says how much you mean.
  With clicks only, one click on a shirt could mean the shirt or the person wearing it: **Object**
  takes the shape the model is surest of (often just the shirt), **Person** takes the whole
  figure (the largest of the model's answers that it is nearly as sure of; one it is much less
  sure of, which usually spills into the background, is never taken). **[Fork]** Beginners
  marking a person: pick Person and click once on the body. To isolate one part (a face, a
  jacket), pick Object, or draw a box round just that part.
- **Few strokes or many.** A box and a point or two is quickest and often enough; an extra stroke
  on a hard frame (the subject turns, something passes in front) costs a re-track of the frames
  near it only. Beginners: start with a box; add points only where it fails.
- **Which model.** Times are for one frame on an RX 6700 XT, estimated from the parts measured
  separately (not yet timed over a whole track); yours will differ.
  - **Better** (SAM 2.1 Large, GPU): about 0.75 s a frame. It remembers the subject across
    frames, so it holds on through turns, overlaps and similar things touching it. Pick it for
    the final grade or a hard shot.
  - **Faster** (SAM 2.1 Base+, GPU): about 0.5 to 0.6 s a frame, the same memory, a little
    less exact at the edges. A good middle when you track long ranges.
  - **[Fork]** With Better and Faster every part of the model runs on the GPU, including the
    last step that turns what it knows into the mask (about 12 ms there instead of about 100 ms
    on the CPU). If that step ever fails on the GPU it moves to the CPU for the rest of the
    session and the track carries on, just a little slower.
  - **MobileSAM**: about 0.2 to 0.3 s a frame with its image encoder on the GPU (slower on the
    CPU alone). It looks at each frame on its own, so it is weaker when similar objects touch
    (two people side by side, a hand on a face). Good for quick looks, short shots or a machine
    without a discrete GPU.
  - **Auto** picks Better when SAM 2.1 Large is installed and a discrete GPU is there, else
    MobileSAM. On an Intel integrated GPU SAM 2.1 is too slow to be usable, so Auto stays on
    MobileSAM there.
  - Beginners: leave it on Auto. If a shot keeps losing the subject, try Better before adding
    many strokes; if you are just roughing in a look, MobileSAM gives you a matte sooner.
- **Magic Mask or something cheaper.** A qualifier or a tracked window costs no model time; reach
  for Magic Mask when they can't isolate the subject.

### If you know Resolve

**[Resolve-style]** **[Fork]**

- Like Resolve's Magic Mask palette: both modes, + and − strokes, tracking forward,
  backward or both, and refine controls for the matte.
- **[Resolve-style]** Resolve Studio's Magic Mask has a **Better / Faster** quality switch;
  ours has the same two choices (SAM 2.1 Large and Base+), plus **MobileSAM** as a lighter
  option and **Auto**. As in Resolve, Better is slower and holds the subject more firmly, and
  the workflow is the same: strokes on one frame, track, fix with strokes where it slips.
- For people Resolve can also isolate one feature alone (face, arms, clothing…); ours takes
  the whole figure.
- Differences (**[Fork]**): our models are separate files you install (below), not part of the
  app; the selector says which model Auto will use and the status line which one actually ran,
  including when SAM 2.1 fell back to MobileSAM; strokes are points and boxes (not
  painted lines); the mattes are not saved with the project but **regenerated from the strokes**
  (a reopened project needs Track again, and an export uses the frames tracked by then); a frame
  that hasn't been tracked yet shows the node **nowhere** (no correction there) rather than
  everywhere; changing Motion, speed or the media hides the mattes until the clip is tracked again
  (on a nested sequence's clip, so does an edit to the picture inside the nest).
- The models are not bundled; they are files you install in the data folder. SAM 2.1 goes in
  `models/sam2/` (Better: `sam2.1_hiera_large.pt` and `sam2.1_hiera_l.yaml`; Faster:
  `sam2.1_hiera_base_plus.pt` and `sam2.1_hiera_b+.yaml`), MobileSAM in `models/mobilesam/`
  (`mobile_sam.pt`). With a model missing, Track says which files to put where, in plain words,
  and nothing else changes. MobileSAM is also what SAM 2.1 falls back to, so it is worth having.

## Colour warper

> id: color.secondaries.warper | does: moves chosen colours to other hues, saturations or brightness by dragging points on a colour mesh | ui: grade.tab.warper, grade.warper.* | cmd: grade.warper.* | resolve: Color Warper | when: reshaping a few colours together (skin and foliage, a sky's hue, colour density) without keys | see: color.secondaries.qualifier

**[Resolve-style]**

### Why

A qualifier picks one colour and changes it; the colour warper reshapes the colour space itself.
You see your colours laid out on a mesh and drag a point: every colour near that point follows,
smoothly, and colours far away stay exactly as they were. That makes it quick for jobs that
would need several qualifiers: pull greens towards a yellow-green, push a sky a little more
cyan, calm one over-saturated colour, or darken saturated highlights for a denser, filmic look.
There is no matte, so there are no edges to chatter.

It has two meshes, and both apply on the node (hue-sat first):

- **Hue-Sat** {ui:grade.warper.mode.hueSat}: a disc like the colour wheels, hue round the rim,
  saturation from grey in the centre to full at the edge. Moving a point round the circle
  changes the hue of the colours under it; moving it in or out lowers or raises their saturation.
- **Chroma-Luma** {ui:grade.warper.mode.chromaLuma}: a square, saturation across and brightness
  (luma) up, for every hue at once. Moving a point up or down brightens or darkens the colours
  with that saturation and brightness; left or right changes their saturation.

### Steps

1. Select the node (one job per node: a warper node usually comes after balance and contrast)
   and open the **Warper** tab {ui:grade.tab.warper}.
2. Pick the mesh: **Hue-Sat** or **Chroma-Luma**.
3. Drag a point on the plane {ui:grade.warper.plane} (`grade.warper.movePoint`). The picture
   follows as you drag; the whole drag is one undo step. A moved point is drawn in the accent
   colour.
4. Pin points that must not move: right-click a point, or click it and press **Pin**
   {ui:grade.warper.pin} (`grade.warper.pin`). A pinned point is drawn on a red square and can't
   be dragged until you unpin it.
5. **Falloff range** {ui:grade.warper.range} sets how far a move pulls the free points around
   it, in mesh steps: 0 (the default) moves only the colours between the point and its
   neighbours; 2 or 3 moves a wider band of colours more gently. Pinned points never follow.
6. **Strength** {ui:grade.warper.strength} scales every move (dial a look back without
   re-dragging). Strength and Falloff range keyframe (**[Fork]**): click the diamond after the
   slider {ui:grade.warper.strength.anim} {ui:grade.warper.range.anim} to animate it from its
   value (`grade.toggleAnimation`, `param` `warp.strength` / `warp.range`), then move the
   playhead and drag the slider to add a keyframe, for example to fade a look in over a shot.
   **Preserve luma** {ui:grade.warper.preserveLuma} keeps each colour's brightness
   when hue-sat moves change it (a yellow pushed to blue would otherwise get darker).
7. Undo a point with **Reset Point** {ui:grade.warper.resetPoint}, the shown mesh with **Reset
   Mesh** {ui:grade.warper.resetMesh}, everything with **Remove** {ui:grade.warper.remove}
   (`grade.warper.reset`).
8. The grid size is under the settings: **Hue columns** / **Saturation rings** (Hue-Sat) or
   **Chroma columns** / **Luma rows** (Chroma-Luma) {ui:grade.warper.cols} {ui:grade.warper.rows}.
   A new size starts that mesh afresh. Settings and the whole warper can also be set at once with
   `grade.warper.set`.

### Options and trade-offs

- **Coarse or fine grid.** 8 hue columns and 4 rings (the default) are enough for most looks
  and keep moves smooth. More columns (up to 16) let you move one narrow hue without touching
  its neighbours; more points also make it easier to create bumps. Beginners: keep the default.
- **Range 0 or more.** Range 0 is precise and predictable (only the cells next to the point
  change). A larger range is faster for broad changes ("everything warm a bit more orange") but
  reaches further than you may expect; pin the colours you want to protect, such as skin.
- **Warper or qualifier.** The warper is best for smooth, global reshaping of a few colours.
  For one object, or a colour that also appears where you don't want the change, a qualifier or
  a window on the node is more targeted (and they work together: a qualified node can warp).

### If you know Resolve

**[Resolve-style]**

- Like Resolve's Color Warper: a hue-saturation mesh and a chroma-luma mesh of draggable
  points, pins, a falloff for the neighbours, and the warp
  compiled into one smooth colour transform.
- Differences (**[Fork]**): our meshes are drawn on a plain colour background, not on the
  picture's own colours (no vectorscope trace yet); there is no lasso multi-select, no
  "pin all / invert pins" or keyboard nudges yet (drag one point at a time); the chroma-luma
  mesh is saturation × luma for all hues (not per hue); grid sizes are 6–16 × 4–8 (hue-sat)
  and 4–8 × 4–8 (chroma-luma); the mesh points are not keyframed (Strength and Falloff range
  are). The maths is our own: HSV of
  the grading signal (DaVinci Intermediate in a DaVinci Wide Gamut project), the mesh smoothed
  with Catmull-Rom splines into a fine table, and colours outside the moved cells left bit-exact.
