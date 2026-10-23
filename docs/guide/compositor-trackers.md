# Tracking in the Compositor

**[Fork] [Resolve-style]**

Tracker and Planar Tracker nodes in a comp (fork C3.5a). For tracking on the timeline (masks,
Attach to Track, Corner Pin's Track Surface) see [tracking.md](tracking.md); for comps in general
see [compositor.md](compositor.md).

## Tracker and Planar Tracker nodes [Fork]

> id: compositor-trackers.nodes | does: tracks points or a flat surface through a comp node's input and uses the motion to match move, stabilise or corner pin | cmd: comp.track.analyse | resolve: the Tracker and Planar Tracker tools on the Fusion page | premiere: Corner Pin with Track Surface, Warp Stabilizer | when: a logo that follows something in the shot, steadying a shaky shot, replacing a phone or TV screen | see: tracking.screen, compositor.masks

A **Tracker** node follows one or more points; a **Planar Tracker** node follows a flat area (a
screen, a sign, a wall), which also catches perspective. Both read the picture on their first
input. Their second input is optional: a **Foreground** (Tracker) or **Insert** (Planar Tracker)
that is moved and laid over the first input.

1. Add the node and connect the shot to its first input (and, if you want, a logo or a
   replacement picture to the second).
2. Tracker: **Add point** (`comp.track.addPoint`) and type where it sits in comp pixels
   (`comp.track.point<k>.x` / `.y`). Pick spots with detail (corners, texture, text); flat areas
   have nothing to follow. Planar Tracker: type the four **Plane corners**
   (`comp.planar.corner<k>.x` / `.y`); the area inside them is tracked unless you set another
   region (`comp.planar.setRegion`).
3. Choose the **Operation** (`comp.track.operation`): **Match Move** (the foreground follows the
   motion), **Stabilise** (the shot moves so the points stay still) or **Corner Pin** (the
   foreground's corners sit on four tracked points, or on the plane's corners). Tracker also has
   **Method** (`comp.track.method`: position only, with rotation, or with scale too) and, for
   Stabilise, **Smooth radius** (`comp.track.smoothRadius`): 0 locks the shot in place, a larger
   radius only removes the shake and keeps a pan.
4. **Set to playhead** (`comp.track.reference`) picks the reference frame the motion is measured
   from.
5. **Analyse forward** or **Analyse backward** (`comp.track.analyse.forward` /
   `.backward`, the command `comp.track.analyse`) tracks from the playhead; **Stop** (`comp.track.cancel`) ends it early and keeps
   what was tracked. The whole run is one undo step. **Clear track** (`comp.track.clear`)
   removes the analysed keyframes.

Points and corners are ordinary keyframes after analysis, so a bad frame can be fixed by typing a
value at that frame. Agents can set the same things with `comp.track.setPoints`,
`comp.track.setOptions`, `comp.planar.setRegion` and `comp.track.clear`. If you add, remove or
resize points, redraw the region or resize the comp while an analysis runs, its result is not
written (it belongs to the old setup): analyse again. A Tracker's match move and stabilise run on
the GPU; a corner pin and anything a Planar Tracker draws (it is in perspective) render on the
CPU for now, so playback of such a comp is slower.

## Placing tracker points and the plane on the viewer [Fork] [Resolve-style]

> id: compositor-trackers.viewer | does: places and sizes Tracker points and draws a Planar Tracker's region and corners on the Program monitor | cmd: comp.track.setPoints, comp.planar.setRegion | resolve: the Tracker and Planar Tracker widgets on the Fusion page's viewer | premiere: placing the Track Surface corners of Corner Pin | when: picking what to track before analysing, or fixing a bad frame by hand | see: compositor-trackers.nodes

With the Compositor open and a Tracker or Planar Tracker node selected, its points or its plane
are drawn over the Program monitor's picture (`compViewer.overlay`). This works the way the
on-viewer tracker widgets of Resolve's Fusion page do. The comp clip has to be under the
playhead. Typing values in the inspector still works; use whichever is quicker for the shot.

Tracker:

- **Ctrl-click** (Cmd-click on macOS) on the picture adds a point there. A plain click does
  nothing, so you can't add one by accident.
- Each point shows its centre (a white cross, `compViewer.track.point<k>`), its pattern box (the
  solid box: the detail that is followed) and its search box (dashed: how far that detail may move
  from one frame to the next). Drag the centre to move the point at the playhead.
- Drag the pattern box's lower-right corner (`compViewer.track.pattern<k>`) to resize it. A box
  that is too small loses the detail; one that is too big takes in background that moves
  differently.
- Drag the search box's corner (`compViewer.track.search<k>`) to widen the search for fast moves.
  A wider search is slower to analyse and more likely to jump to a similar spot.
- After analysis, the path the point took over the comp clip's frames is drawn as a green line.
  To fix a bad frame, go to it and drag the point: that frame's keyframe moves.

Planar Tracker:

- With no region yet, **Draw region** (`compViewer.planar.draw`) is on. Click around the flat
  area, then click the first point (or press Enter) to close it (`comp.planar.setRegion`). Keep
  the region inside the surface, and leave out anything that passes in front of it.
- **Edit** (`compViewer.planar.edit`) shows the region's points
  (`compViewer.planar.region<i>`, green) and the plane's four corners at the reference frame
  (`compViewer.planar.corner<k>`, orange). Drag them into place. Put the corners on the edges
  of what you are replacing (a screen, a sign), because the Insert is pinned to them.
- Once analysed, the plane's position at the playhead is shown as a dashed white outline.

Each drag is one undo step (`comp.track.setPoints` or `comp.planar.setRegion`).
