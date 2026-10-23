# The Compositor (node comps)

**[Fork] [Resolve-style]**

Why: some shots are easier to build as a small flow chart than as a stack of tracks: a keyed
subject over a new background with its own colour, a title that sits behind a person, the same
logo treatment reused in many places. A **comp** is that flow chart. It is a project item like a
nested sequence, so one comp can be on the timeline many times, and changing it changes every
copy.

The Compositor is in progress (COMPOSITOR-PLAN C1). What is listed here works today; the Color
editor in the inspector and drawing masks on the viewer come later in C1.

## Comps, nodes and connections

> id: compositor.basics | does: explains comps, the node kinds and how pictures flow through them | cmd: comp.new, comp.addNode, comp.connect, comp.params | resolve: Fusion page node graph | premiere: — (closest: nested sequence) | when: before building a first comp | see: compositor.make, compositor.recipes

What is a comp? A **comp** (short for composite) is a project item that holds a node graph; on the
timeline you use it as a comp clip. Like a nested sequence, one comp can be on the timeline in
many places: change it once and every comp clip that uses it changes too. A comp has a size, a frame rate and a length, and a graph of **nodes**. Each node takes pictures
in on the left, does one job, and passes a picture out on the right. The comp shows what reaches
its **Media Out** node; every new comp starts with one.

| Node | What it does |
|---|---|
| **Media In** | A picture from the project (a clip, a sequence, another comp) or from a track under the comp clip |
| **Merge** | Puts its second input (foreground) over its first (background), with opacity and a blend mode |
| **Transform** | Moves, scales, rotates (the same controls as a clip's Motion) |
| **Key** | Luma Key or Ultra Key: makes part of the picture transparent |
| **Mask** | Keeps the picture only inside (or outside) drawn shapes |
| **Blur** | Gaussian blur |
| **Color** | One grade node: the same controls as a node on the Color page |
| **Text** | A title layer |
| **Solid** / **Gradient** | A flat colour, or a linear / radial blend of two colours |

Every edit to a comp is one undo step, like any other edit. `comp.params` lists the controls a
kind of node has.

If you know Resolve: the idea is the Fusion page's node graph, much smaller: no 3D, particles or
scripting (a flat clip can still be tilted in perspective with the 3D Rotate effect,
[effects.md](effects.md)). If you know Premiere: there is no node compositing there; a comp clip is closest to a
nested sequence whose layers you wire yourself.

## Making a comp

> id: compositor.make | does: opens the Compositor panel and makes a comp clip on the timeline | ui: comp.newClip | cmd: comp.newClip, comp.open | resolve: New Fusion Clip, Open in Fusion page | premiere: — | when: starting a composite from nothing; wiring nodes together and unplugging them | see: compositor.basics, compositor.convert

1. Open the panel: **Window ▸ Compositor** (`window.panel.Compositor`), or the **Compositing**
   workspace (**Window ▸ Workspaces ▸ Compositing**, `window.workspace.compositing`), which
   puts it beside the Program monitor. In the Resolve workflow the **Fusion** page is in the page bar: click **Fusion** {ui:page.fusion}
   (`page.fusion`), between Edit and Color.
2. With no comp clip selected, the panel offers **New Comp Clip** {ui:comp.newClip}
   (`comp.newClip`, also **Clip ▸ Compositor ▸ New Comp Clip**). It makes a comp the size of
   the sequence and places it on V1 at the playhead.
3. Select any comp clip and the panel shows its graph. To show one without selecting it, use
   **Clip ▸ Compositor ▸ Open in Compositor** (`comp.open`).
4. Add a node: right-click the empty canvas and pick its kind (`comp.addNode`).
5. Connect: drag from a node's output dot to another node's input dot (`comp.connect`).
   Dragging onto an input that is already used replaces that connection.
6. Select a node and press **Delete** to remove it (`comp.removeNode`); **Ctrl+B** (or **Bypass**
   in its right-click menu) shows the picture without it (`comp.bypass`).
7. Disconnect: click an input dot that has a connection (`comp.disconnect`); the node's
   input is empty again.
8. Tidy the graph: drag a node by its body to move it (`comp.moveNode`). Where a node sits
   doesn't change the picture.
9. Pick the finished picture: right-click a node ▸ **Set as Output**
   (`comp.setOutput`). The comp clip shows what that node outputs (the Media Out node).
   This differs from **View**, which only changes what the Program monitor shows while you work.

On the timeline a comp clip shows **Comp** and its number of nodes in its header.

## Changing a node's settings

> id: compositor.inspector | does: shows the selected node's settings under the canvas and changes them | cmd: comp.setParam, comp.toggleAnimation | resolve: Inspector on the Fusion page | premiere: Effect Controls rows | when: adjusting a Merge, Transform, Key, Blur, Color, Solid, Gradient or Text node | see: compositor.basics

1. Click one node on the canvas. Its settings appear as rows under the canvas, like Effect
   Controls rows: numbers you drag or type, colour swatches, menus.
2. Change a value; the comp clip updates. Each change is an undo step (one drag is one step).
3. To animate a setting, click the stopwatch at the start of its row (`comp.toggleAnimation`):
   a keyframe is set at the playhead, and later changes at other playhead positions add
   keyframes (`comp.setParam` with a time). Clicking the stopwatch again removes them.

A **Text** node shows the title's controls (text, font, size, colour and the rest): type in the
text field and the title changes as you type. A **Color** node shows its Lift, Gamma, Gain and Offset wheels (drag the puck to move the
colour balance, as on the Color page) above its numeric grade settings (Contrast, Pivot,
Saturation, Temperature, Tint, grain), and a curve editor with Y / R / G / B buttons (drag on
the curve to add a point, double-click a point to remove it; scroll the settings to reach it). Selecting several
nodes hides the rows.

## Expressions: a setting worked out by a formula [Fork]

> id: compositor.expressions | does: drives a comp setting with a small formula of time, frame, size or another setting, instead of or on top of keyframes | cmd: comp.setExpression, comp.clearExpression, comp.explain | resolve: Fusion's simple expressions (the = field on a control) | premiere: none (After Effects has expressions; Premiere has none) | when: a setting should follow time steadily (a spin, a pulse, a wiggle), follow a setting elsewhere in the comp, or depend on the comp's size | see: compositor.inspector, compositor.explain

Keyframes are best for exact values at exact moments. Some motion is easier to describe than to
keyframe: "turn 10 degrees a second", "shake a little", "stay as opaque as that Merge". An
expression is a one-line formula worked out at every frame. Use either, or both: a formula can
start from the keyframed value (`value`), so keyframes set the broad move and the formula
changes it.

1. Click a node on the canvas so its settings show under the canvas.
2. Click **=** after a setting's label (`comp.inspector.<param>.expr.toggle`, for example
   `comp.inspector.rotation.expr.toggle`). A one-line field opens under the row
   (`comp.inspector.<param>.expr`).
3. Type a formula, for example `time * 10`, and press **Enter** (or click elsewhere). It is
   stored with `comp.setExpression` (**Ctrl+Z** takes it back), and the row shows its value at
   the playhead.
4. A typing mistake stays in the field with what is wrong under it, and its column (for example
   ``expected `)` after `sin(time` at column 9``); nothing is stored. A stored formula that fails
   while it runs (a division by zero, a deleted node) shows its error there too, and the setting
   uses its keyframed value meanwhile: a render never fails because of a formula.
5. To remove it, empty the field and press **Enter**, or send `comp.clearExpression`. Clicking
   **=** again only hides the field; the formula stays.

From the control channel or MCP, `comp.setExpression` takes `node`, `param` and `expression`
(`comp` is optional: the selected comp clip). It answers with the value at `time` and any error,
and refuses a formula that can't be read unless `"allowError": true`. `comp.explain` lists every
formula with its value and error.

What a formula can use:

- **Built in:** `time` (seconds), `frame` (exact at 23.976 and 29.97), `fps`, `width`, `height`,
  `value` (this setting's keyframed value now), `pi`, `e`.
- **Settings:** `self.scale` (the same node) or `node("Blur 1").blurriness` and `node(3).opacity`
  (by label or number).
- **Maths:** `+ - * / %`, `^`, comparisons, `&&`, `||`, `!`, `test ? a : b`; points as `[x, y]`
  and RGBA values as `[r, g, b, a]`, read with `v[0]`, `.x .y` or `.r .g .b .a`.
- **Functions:** `sin cos tan asin acos atan atan2 sqrt abs sign min max clamp floor ceil round
  fract exp ln log10 pow lerp smoothstep step`, `noise(x [, seed])` (smooth repeatable
  randomness, −1 to 1), `random(seed)` (0 to 1, repeatable) and `wiggle(freq, amp [, seed])`
  (`value` shaken by `amp`, `freq` times a second).

Starting points, each with a trade-off:

- A steady spin on Rotation: `time * 45`. Easy to read, but it never eases; keyframes suit a
  spin that starts and stops.
- A shake on Position: `wiggle(2, 5)`. The same on every playback; a seed (`wiggle(2, 5, 7)`)
  gives a different shake.
- Two Solids kept in step: `node("Back").color` on the second. One place to change, but
  a web of references is harder to follow (`comp.explain` helps).

Formulas work on a node's own numbers, points, RGBA swatches, checkboxes and menus (a menu takes the
rounded number, kept within its choices); text, curves, mask shapes and the settings inside
Color and Text nodes take none yet.

- **Revolution:** a small formula language of its own, close to Fusion's simple expressions and
  unlike After Effects' JavaScript: no scripting, no loops, no variables, no files. So a formula
  from any project is safe to open: one over 4 096 bytes, nested too deeply or reading
  itself in a loop gets a message instead of slowing or stopping the app.

## Comp size, Media In clips and node names (commands only for now) [Fork]

> id: compositor.settings | does: changes a comp's size, frame rate and length, which clip a Media In node reads, and a node's name | cmd: comp.setSize, comp.setSource, comp.labelNode, comp.get | resolve: Fusion composition settings, MediaIn node source, renaming a node | premiere: — | when: a comp must be another size or length, a Media In node should read a different clip, or the graph needs clearer names | see: compositor.make, compositor.basics

These have no button in the Compositor panel yet. Run them from the command palette, the
control channel or the MCP tools; each is one undo step. `comp` can be left out to use the
selected comp clip.

1. See the comp first (all of these are commands only, with no button yet): `comp.get` lists
   its nodes, connections, output node, size, frame rate and length, plus the order the nodes
   run in and, if the graph can't run, why.
2. Size, frame rate, length: `comp.setSize` with any of `width`, `height`, `fps`, `duration`
   (in ticks). A new comp starts at the sequence's size.
3. Which clip a **Media In** node reads: `comp.setSource` with `node` and either `item` (a
   project item) or `track` (the clip under the comp on that track), plus an optional
   `offset` (ticks into the source).
4. Name a node: `comp.labelNode` with `node` and `label` (long names are cut). The name shows
   on the canvas; it doesn't change the picture.

- **Revolution:** no composition settings dialog and no rename field on the node yet (unlike
  Resolve).

## Looking at one node's output [Resolve-style]

> id: compositor.view | does: shows one node's output in the Program monitor | cmd: comp.view | resolve: viewing a node on the Fusion page's viewer | premiere: none (Premiere has no node graph) | when: checking what a node does before it is merged | see: compositor.basics

Like the Fusion page, you can look at any node's picture, not only the finished comp: this is how you
see what one node outputs.

1. Put the playhead over the comp clip and open its comp in the Compositor.
2. Double-click a node (or right-click it ▸ **View**, or pick it in the **Show** menu at the top
   right of the canvas, `comp.viewer`). The Program monitor now shows that node's output, at the
   comp's size. Viewing a node changes only what the monitor shows, not the finished comp.
3. To go back to the finished picture, pick **Show: Media Out** (or send `comp.view` with no
   node).

The Program monitor shows the node only while the Compositor panel is open and the playhead is
over a clip of that comp; otherwise it shows the sequence as usual. Unlike Resolve there is one
viewer, the Program monitor, rather than two viewers side by side.

## What a comp does, node by node [Fork]

> id: compositor.explain | does: describes what a comp does, node by node, in the order the pictures flow | cmd: comp.explain | resolve: none (Fusion has no built-in explainer) | premiere: none | when: opening a comp someone else built, or checking why a node seems to do nothing | see: compositor.basics, compositor.view

A comp you made last month, or one from a template, can be hard to read from the canvas alone.
`comp.explain` reads the graph and says what each node does, starting from the sources and ending
at Media Out, using the nodes' own names and the on-screen names of any setting you changed. It
only describes what is in the graph, so it can't invent a node or a control.

1. Send `comp.explain` (from the control channel or MCP; `{"comp": <id>}`, or nothing for the comp
   open in the Compositor). Add `"time"` in ticks to describe animated settings at that moment.
2. Read `summary` first: how many nodes build the picture and from how many sources.
3. Read `lines` in order; each names one node and what it does, for example "puts Key 3
   (foreground) over Background (background)".
4. Check `unused`: nodes that don't reach Media Out change nothing, which is the usual reason a
   node "does nothing": a Blur (or any other) node that isn't connected to Media Out changes nothing.

A graph that can't be evaluated (a loop) gets a summary that says so, and no lines. There is no
button for it in the Compositor yet.

## A Mask node's masks [Fork]

> id: compositor.masks | does: adds, inverts, reshapes (points, Bézier handles, added / deleted points) and deletes the shapes of a Mask node | cmd: comp.setMasks | resolve: Polygon / Ellipse / Rectangle mask nodes on the Fusion page | premiere: the mask buttons in Effect Controls | when: keeping only part of the picture in a comp; a person in front of text, a clone of yourself, a split screen | see: compositor.inspector

A Mask node keeps its input only inside its shapes (or outside, when a shape is inverted).
Unlike Resolve's Fusion page, where each shape is its own node, one Revolution Mask node holds a
list of up to sixteen shapes, like the masks of a Premiere effect.

1. Select the Mask node. Under the canvas is a **Masks** row with **Add ellipse**
   (`comp.mask.ellipse`) and **Add rectangle** (`comp.mask.rect`).
2. Click one: a shape a fifth of the comp's short side is added in the middle of the comp,
   named *Mask 1*, *Mask 2*, …
3. Tick **Inverted** (`comp.mask<k>.inverted`, k counts from 0) to keep the picture outside the
   shape instead.
4. Under each shape, **Mask Feather**, **Mask Opacity** and **Mask Expansion**
   (`comp.mask<k>.feather` / `.opacity` / `.expansion`) soften the edge, fade the shape and grow
   or shrink it, as in Effect Controls. If the shape is keyframed, the change is a keyframe at the
   playhead.
5. **Delete** (`comp.mask<k>.delete`) removes the shape.

Each click is one undo step (`comp.setMasks` with the whole list).

To move or reshape a shape, keep the Mask node selected and work on the Program monitor
(`comp.maskOverlay`): its shapes are drawn over the picture.

1. Drag a square point to move it, or drag inside a shape to move all of it.
2. A smooth point (an ellipse's four points) shows two round Bézier handles on short lines.
   Drag one to bend the curve; the other handle turns with it so the curve stays smooth. Hold
   **Alt** while dragging to move one handle alone (a sharp bend, a cusp).
3. **Alt-click** a point to switch it between smooth and corner: a rectangle's corners get
   handles this way, and a smooth point loses them.
4. **Ctrl-click** (Cmd-click on macOS) on an edge to add a point there, or on a point to delete
   it (a shape keeps at least 3 points). On a keyframed shape the point is added to every
   keyframe, so the shape still animates smoothly.

Each drag or click is one undo step. The keys are the same as for Effect Controls masks, in both
workflows (not the Fusion page's own shortcuts). An agent can also send `comp.setMasks` a full
mask list.

## Roto and Paint nodes [Fork]

> id: compositor.roto | does: cuts a picture out along animated roto shapes, or paints brush strokes over it | cmd: comp.roto.addShape, comp.roto.setShapeKeyframe, comp.paint.addStroke | resolve: Polygon / B-Spline and Paint nodes on the Fusion page | premiere: animated masks in Effect Controls (no paint) | when: isolating a moving subject, or painting out a mark, a wire or a logo | see: compositor.masks

A **Roto** node holds up to 256 shapes. Each one is a Bézier or B-spline outline with its own
feather at every point, and its outline can be keyframed. **Output** picks what the node gives:

- **Alpha** keeps its input only inside the shapes. With no shape switched on, the input passes
  through unchanged.
- **Matte** shows the shapes as white on black, ready to feed a Merge or a key.

Select the node to see its shape list under the canvas. **Add shape** (`comp.roto.add`) adds an
ellipse in the middle of the comp (`comp.roto.addShape`). Each shape `k` (counting from 0) has **Enabled**
(`comp.roto<k>.enabled`), **Inverted** (`comp.roto<k>.inverted`), **Mode**
(`comp.roto<k>.mode`: Add, Subtract, Intersect or Replace, combined with the shapes above it),
**Feather** (`comp.roto<k>.feather`) and **Delete** (`comp.roto<k>.delete`). Shapes are drawn
and reshaped on the Program monitor (below); an agent can also send an outline with
`comp.roto.setShapeKeyframe`, one keyframe at a time.

A **Paint** node draws strokes over its first input. Each stroke uses one tool:

- **Color** paints a colour.
- **Clone** copies the picture from a nearby spot. With a clone time offset it copies from
  another frame instead, for example the frame before, to paint out something that moves away.
  One Paint node clones from at most 4 other frames at a time; strokes past that copy from the
  current frame, and `comp.explain` says so.
- **Reveal** shows the node's second input, for example a clean plate.
- **Erase** brings back the unpainted picture.

A stroke lasts one frame or holds over a range of frames. The stroke list shows each stroke's
tool, **Size** and **Opacity** (`comp.paint<k>.size` / `.opacity`) and how long it lasts, with
**Move up** / **Move down** (`comp.paint<k>.up` / `.down`) and **Delete**
(`comp.paint<k>.delete`). Strokes are painted on the Program monitor (below); an agent adds them
with `comp.paint.addStroke`.

Rotoscoping takes time whichever way you do it, so use the method that suits the shot:

- A few shapes keyframed every few frames is quick, and fine for slow movement.
- One keyframe per frame takes longer but stays exact on fast motion.
- Magic Mask is often quicker for whole people or objects. Roto gives you control of every edge.

Agents can also replace the whole list (`comp.setRoto`, `comp.setPaint`) or remove and
reorder entries (`comp.roto.removeShape`, `comp.paint.removeStroke`, `comp.paint.reorder`).
Roto shapes and paint strokes are saved with the project.

## Drawing roto shapes and painting on the viewer [Fork] [Resolve-style]

> id: compositor.viewer-tools | does: draws and reshapes roto shapes (points, Bézier handles, per-point feather) and paints strokes on the Program monitor | cmd: comp.roto.addShape, comp.roto.setShapeKeyframe, comp.paint.addStroke | resolve: the Polygon / B-Spline and Paint tools on the Fusion page's viewer | premiere: drawing a pen mask on the Program monitor | when: cutting out a moving subject by hand, or painting out a mark frame by frame | see: compositor.roto, compositor.masks

Keep the Compositor open with the Roto or Paint node selected. Its shapes or the brush show over
the Program monitor's picture (`compViewer.overlay`), with a small tool bar at its top left and a
short hint at its bottom left. The comp clip has to be under the playhead. The tools work the
same in both workflows.

Drawing a roto shape, as with Resolve's Polygon and B-Spline tools:

1. Pick **Bézier** (`compViewer.roto.bezier`) or **B-Spline** (`compViewer.roto.bSpline`). A
   Roto node with no shapes starts in Bézier.
2. Bézier: click to add a corner point, or press and drag to add a smooth point; the drag pulls
   out its handles. B-Spline: click each control point. The curve stays inside the points, so a
   B-spline is quick for soft, organic edges, while Bézier gives exact corners and curves.
3. Click the first point (`compViewer.draft.first`) or press **Enter** to close the shape (at least
   3 points). **Backspace** removes the last point and **Escape** drops the drawing.

The new shape is keyframed at the playhead (`comp.roto.addShape`), and the tool goes back to
**Select** (`compViewer.roto.select`).

Reshaping, in Select:

- Drag a square point (`compViewer.roto<k>.point<i>`) to move it, or drag inside a shape to move
  all of it. **Alt-drag** a point also moves its whole shape.
- A click selects a point. The selected shape's smooth points show round Bézier handles
  (`compViewer.roto<k>.point<i>.in` / `.out`). Drag one to bend the curve; the other turns with
  it. Hold **Alt** while dragging to move one handle alone (a cusp).
- **Alt-click** a point of a Bézier shape to switch it between smooth and corner.
- The selected point shows two orange diamonds on a dashed line: the outer feather
  (`compViewer.roto.featherOut`) and the inner feather (`compViewer.roto.featherIn`). Drag one
  away from the point to soften the edge outside or inside the outline there. This is per point,
  on top of the shape's own **Feather**: use it where only part of the edge is blurred, such as
  hair or a moving hand.

Every change is a keyframe at the playhead (`comp.roto.setShapeKeyframe`). To animate, move the playhead and adjust the shape again. You can work every few
frames and let the shape move smoothly between them, or key every frame for fast motion.

Painting:

1. Pick the tool on the bar: **Color**, **Clone**, **Reveal** or **Erase**
   (`compViewer.paint.color` / `.clone` / `.reveal` / `.erase`). Set **Size** (pixels),
   **Hardness** and **Opacity** (`compViewer.paint.size` / `.hardness` / `.opacity`), and for
   Color the colour swatch (`compViewer.paint.swatch`). The circle under the pointer is the brush
   size.
2. Drag on the picture. Each drag is one stroke (`comp.paint.addStroke`). A mouse paints at
   full pressure.
3. Clone: **Alt-click** where to copy from, then paint. The offset from that spot to where you
   start painting is kept for the next strokes, and a small cross shows where the brush copies
   from.
4. **This frame only** (`compViewer.paint.singleFrame`) makes the next strokes last only for the
   playhead's frame. Use it for frame-by-frame fixes such as dust, a flash or a passing wire.
   Leave it off for a fix that holds through the whole comp, such as a logo on a still wall.

Each reshaping drag or stroke is one undo step. The brush settings are interface state (`ui.panels.compViewer.brush`), so an agent can read or
set them with `ui.inspect` / `ui.set`.

## Roto shapes that follow the object [Fork]

> id: compositor.roto-track | does: makes a roto shape drawn once follow a moving object, by linking it to a tracker or by tracking it with Magic Mask | cmd: comp.roto.linkTracker, comp.roto.unlinkTracker, comp.roto.trackShape, comp.roto.stopTrack | resolve: connecting a Tracker to a Polygon's position, or Magic Mask on the Color page | premiere: Track Selected Mask in Effect Controls | when: a moving subject you would otherwise keyframe by hand | see: compositor.roto

Draw a shape once and let it follow what it outlines. Each shape `k` of a Roto node has a second
row with two ways to do that.

**Follow a tracker.** Add a Tracker or Planar Tracker node, analyse it, and pick it in the
shape's **Follow tracker** list (`comp.roto<k>.follow`, `comp.roto.linkTracker`). The shape
moves the way the tracker measured, whatever the tracker node itself does with its picture.
The shape stays where it is on screen at the playhead when you pick the tracker, and keyframes
you already set stay where you drew them. Its own keyframes still work on top of the track, for a
hand that also opens and closes. Pick **None** to stop following (`comp.roto.unlinkTracker`); the
shape again stays where it is at the playhead. If the tracker is deleted, the list shows **Missing tracker** and the
shape stays where it was drawn. An agent can also follow one point of a Tracker
(`"output"`: the point's name), which moves the shape without turning it.

**Track the shape with Magic Mask.** Draw the shape round the subject at the playhead, then
click **Track forward** or **Track backward** (`comp.roto<k>.track.forward` / `.backward`,
`comp.roto.trackShape`). Magic Mask (SAM 2.1 when it is installed, otherwise MobileSAM) follows
the subject frame by frame through the Roto node's input. Each frame becomes a shape keyframe
with the same number of points as your shape and the same per-point feather. An agent can space
the keyframes out (`"every"`: frames) and pick the model (`"model"`). It runs in the background;
**Stop** (`comp.roto<k>.track.stop`, `comp.roto.stopTrack`) keeps what was tracked so far. The
whole track is one undo step. If you change the shape while it runs, nothing is written, so you
can try again.

Which to use depends on the shot:

- A tracker suits rigid things and planes (a sign, a screen, a car door). The shape keeps its
  exact outline and only moves.
- Magic Mask suits things that change shape (a person, an animal). It redraws the outline every
  frame, so check the edges and fix a keyframe where it slips.
- For fast or blurred motion, a beginner may find Magic Mask quicker. A finishing artist may
  prefer a tracker plus a few hand keyframes for full control.

## Grouping nodes into a macro [Fork] [Resolve-style]

> id: compositor.groups | does: packs connected nodes into one Group node (a macro) and puts chosen inner settings on it | cmd: comp.group, comp.ungroup, comp.group.expose, comp.group.unexpose | resolve: Group and Macro nodes on the Fusion page | premiere: none | when: a graph is too big to read, or the same nodes are reused | see: compositor.basics

1. Select nodes that flow into one node (Media Out stays outside) and click **Group** over the
   canvas (or right-click ▸ Group; `comp.group`). The picture doesn't change.
2. Double-click the group (or **Open Group**) to edit its inside like the comp itself (the
   comp commands take a `group` path); right-click ▸ Group Input adds an input. Select an
   inner node and click **Expose** on a setting to put it on the group (`comp.group.expose`;
   `comp.group.unexpose` takes it off).
   The breadcrumb at the top goes back out.
3. **Ungroup** (`comp.ungroup`) puts the nodes back. Groups are saved in the project.

Grouping is optional: a flat graph is easier to follow while building, a group easier to reuse.

## Turning stacked clips into a comp

> id: compositor.convert | does: replaces clips stacked on several tracks with one comp clip that looks the same | cmd: comp.convertToCompClip | resolve: New Fusion Clip from a selection | premiere: — (closest: Nest) | when: a layered shot you want to keep as one reusable piece | see: compositor.make

1. Select the clips: one per video track, all starting and ending at the same time.
2. Choose **Clip ▸ Compositor ▸ Convert to Comp Clip** (`comp.convertToCompClip`).
3. The clips are replaced, on the lowest of their tracks, by one comp clip. Inside it, each clip
   is a **Media In** followed by a **Transform** holding its Motion settings; the clips are laid
   over each other bottom track first by **Merge** nodes holding their Opacity and blend mode.
   The frame looks the same as before. **Edit ▸ Undo** brings the clips back.

For now this only takes plain clips and refuses, with the reason, anything it can't carry over
yet: effects, a grade, noise reduction, speed changes, reverse, frame holds, time remapping,
transitions, sound linked to the picture, keyframed or masked Motion / Opacity, clips with other
pixel shapes than the sequence, a lowest clip that isn't at 100 % Normal opacity, clips that
don't line up in time, and two clips on one track. Remove or flatten those first (or keep the
clips as they are).

## Two comps to start from

> id: compositor.recipes | does: two small comps that show the usual shapes: a keyed subject over a background, a title over footage | cmd: comp.addNode, comp.connect, comp.setParam | resolve: Fusion basic composite | premiere: — | when: learning the node flow, or as a template | see: compositor.basics

**Keyed subject over a background** (comp A; also how you key a green screen over a background): Media In (your footage) → Key (Luma Key) → into
the foreground of a Merge whose background is a Solid → Color → Media Out. The Color node grades
the finished composite as one picture. For a green or blue screen, use **Ultra Key** in the Key node
instead of Luma Key (a new Key node starts as Ultra Key; its settings are the Ultra Key effect's,
see the keying page), and for a background plate replace the Solid with a second Media In of that clip.

**Title over footage** (comp B): Text → Transform → into the foreground of a Merge whose
background is a Media In of the footage → Media Out.

Both can be built in one go with the command batch kept in the repository as
`tools/comp-accept.json` (an agent or script runs it through the control channel's
`command_batch`): it puts comp A on V2 and comp B on V3, both reading the footage on V1.

## Saving part of a comp as a template [Fork]

> id: compositor.templates | does: saves a piece of a comp graph as a named template file and adds it to another comp later | ui: comp.canvas.saveTemplate, comp.canvas.insertTemplate | cmd: comp.template.save, comp.template.list, comp.template.insert, comp.template.delete | resolve: saving a Fusion macro or setting | premiere: — (closest: an effect preset) | when: a setup you build again and again, such as a glow or title look | see: compositor.recipes

On the canvas: select nodes, right-click empty space, **Save Selection as Template…**
{ui:comp.canvas.saveTemplate}, type a name. **Insert Template ▸** {ui:comp.canvas.insertTemplate}
lists saved ones and places the chosen one where you clicked;
**Delete Template…** inside it asks before it removes one.

The same as commands:

1. `comp.template.save` with `nodes` (ids from `comp.get`) and a `name`. Only links between
   those ids are kept. A Media In that reads a project item is left out, as is Media Out (the
   reply says so). An existing name needs `replace: true`.
2. `comp.template.list`: names, sizes and save times. Damaged files are skipped.
3. `comp.template.insert` with `name` and optionally `at: [x, y]` (canvas place of its top-left
   corner). The copies get fresh ids; wire them in afterwards.
4. `comp.template.delete` with `name` removes the file only.

Files live in the data folder under `templates/comp`; names can't hold `/` or `..`. Small
pieces combine more freely; whole looks are quicker to reuse. Pick what suits you.

## What makes a comp fast or slow

> id: compositor.speed | does: says what a comp redraws every frame and what makes it fall back to the processor | cmd: comp.addNode | resolve: Fusion render cache | premiere: — | when: a comp plays slowly | see: compositor.basics

- Parts of a graph that don't change over time (a Text, Solid, Gradient, Blur, Key or Mask with
  no keyframes and no footage feeding it) are drawn once and remembered (up to 256 MB), so a
  title comp costs about one picture per frame.
- Anything fed by a Media In, a Color node, or anything keyframed is redrawn every frame.
- Comps are drawn on the graphics card, at the size they appear (a comp clip scaled to 50 % is
  built at half size). A Media In that reads a track under the comp clip takes that track's
  frames straight from the decoder, as the timeline does, so 1080p comps and 4K comps at half
  resolution play in real time (measured on synthetic footage).
- These run on the processor instead, and play slower:
  - a comp clip with effects, a grade or opacity masks of its own (move those into the comp as
    nodes to keep it on the graphics card);
  - a Media In that reads a project clip rather than a track (for now);
  - a comp wider than the graphics card's largest picture, or one whose pictures need more than
    2 GB of graphics memory;
  - a Blur, Key or Color setting the graphics card can't run, or a comp clip inside a track that
    another comp's Media In reads.

Resolve's Fusion page renders on the graphics card too; where its render cache keeps whole
frames, Revolution keeps only the unchanging parts of a comp and redraws the rest.
