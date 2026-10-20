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
