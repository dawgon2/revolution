# Nodes

**[Resolve-style]** · a clip's grade is a chain of nodes, applied first to last

## Why

> id: color.nodes.why | does: why a grade is split into nodes, one job per node | resolve: serial nodes | premiere: — (one Lumetri effect, or several stacked) | when: a grade gets hard to follow; is the grade on the clip or the whole sequence | see: color.nodes.panel

**[Resolve-style]**

One big correction, all on one node, is hard to reason about. Splitting a grade into several
nodes, **one job per node**
(balance, exposure, contrast, a look), lets you switch any step off, see what each did, and copy
only the steps you want to other clips. A grade starts out **serial**: node 1 feeds node 2 feeds
node 3. The grade belongs to the clip, not to the sequence.

Branched nodes (parallel, layer, outside, splitter, key mixer) are covered further down.

## The Nodes panel

> id: color.nodes.panel | does: the Nodes panel: the clip's chain of nodes, the selected node, on / off | ui: grade.addNode, grade.toggleNode, grade.bypass | cmd: grade.selectNode, grade.toggleNode, grade.bypass | resolve: Node editor | premiere: — | when: finding and switching nodes; the maximum number of nodes per clip | see: color.nodes.three-node

**[Resolve-style]** **[Fork]**

Select a video clip. The **Nodes** panel (in the Color Grading workspace) shows "Nodes · clip
name", then buttons, then the node canvas ({ui:grade.canvas}): an Input box, your nodes as boxes
numbered 01, 02 … with wires between them (a serial grade is one row). The selected node is
outlined; the Primaries panel edits it.

| Button | Id | Command | Does |
|---|---|---|---|
| **+ Serial** | {ui:grade.addNode} | `grade.addNode` | new node after the selected one |
| **+ Before** | {ui:grade.insertNode} | `grade.insertNode` | new node before the selected one |
| **Delete** | {ui:grade.removeNode} | `grade.removeNode` | removes the selected node |
| **Disable / Enable** | {ui:grade.toggleNode} | `grade.toggleNode` | switches that one node off (turns it off) without losing settings (the box is struck through) |
| **Reset** | {ui:grade.resetNode} | `grade.resetNode` | puts all of its controls back to neutral |
| **Bypass** | {ui:grade.bypass} | `grade.bypass` | whole grade off; press again to restore |

**Delete**, **Disable / Enable** and **Reset** need a selected node; **Bypass** needs a grade.
With no nodes the strip reads "No nodes: + Serial adds one"; while bypassed the header shows
"bypassed".

Each node box has its own id, `grade.node.<id>` (the node's stable number). Click a box to select
it (`grade.selectNode`); to disable or enable it, press **Disable / Enable** or right-click it and
choose **Bypass** (`grade.toggleNode`). A node without a name is
labelled by what it holds ("Primaries", "Curves", "Grain", joined with "+" when it holds several,
or "Empty"). A grade holds at most 64 nodes; a damaged file with more shows "+N more (not
rendered)".

## Steps: build a three-node grade

> id: color.nodes.three-node | does: steps to build a balance, contrast and look grade in three nodes | ui: grade.addFirst, grade.addNode | cmd: grade.addNode, grade.labelNode | resolve: serial nodes (Alt+S) | premiere: — | when: your first multi-node grade | see: color.nodes.starter-tree

**[Resolve-style]**

1. Select a clip. In **Primaries** press **Add Node** {ui:grade.addFirst} (or **+ Serial** in
   Nodes). The strip shows one box, 01.
2. Press **+ Serial** {ui:grade.addNode}: box 02 appears after 01 and is selected.
3. Press **+ Serial** again: box 03.
4. Click box 01 and change **Gain** in Primaries. Click box 03 and raise **Grain strength**.
5. Select box 02 and press **Disable** {ui:grade.toggleNode}: the picture changes by exactly
   that node's work. Press **Enable** to restore it.
6. Press **Bypass** {ui:grade.bypass}: the clip looks ungraded. Press it again.

## Delete a node

> id: color.nodes.delete | does: removes a node and its correction from the chain | ui: grade.removeNode | cmd: grade.removeNode | resolve: Delete node | when: a node that no longer does anything useful | see: color.nodes.undo

**[Resolve-style]**

A node you no longer need only makes the grade harder to read. Deleting (removing) it takes its
work out of the picture; the nodes after it move up and keep their settings. To keep its
settings for later, use **Disable** {ui:grade.toggleNode} instead.

1. Select the clip, then click the node's box in the Nodes panel (`grade.selectNode`).
2. Press **Delete** {ui:grade.removeNode} (`grade.removeNode`). The box disappears and the
   picture changes by that node's work.
3. Changed your mind? **Ctrl+Z** brings the node back with its settings.

## Reorder and name nodes

> id: color.nodes.reorder | does: moves nodes earlier or later and names them | ui: grade.insertNode, grade.renameNode | cmd: grade.moveNode, grade.labelNode, grade.insertNode | resolve: node labels, dragging nodes | when: making a grade readable; fixing the order | see: color.nodes.orders

**[Fork]**

Naming nodes ("Balance", "Skin", "Look") makes a grade readable at a glance, as in Resolve.

1. Select the node, then click **Rename** {ui:grade.renameNode} in the Nodes panel, or right-click
   the node and choose **Rename**.
2. Type the name in the field on the node and press **Enter** (or click elsewhere). **Esc**
   leaves the old name. An empty name clears it, and the node shows what it changes instead.
3. **Ctrl+Z** undoes a rename (`grade.labelNode`, params `node`, `label`).

Right-clicking a node also offers **Disable** / **Enable**, **Reset** and **Delete**.
`grade.moveNode` (params `node`, `to`) moves a node to a position in the order; dragging only
moves the box, so use the command (control channel, MCP or command list).

## A starter tree for F-Log2

> id: color.nodes.starter-tree | does: a node order to start from for F-Log2 footage, and why each node sits where it does | resolve: fixed node tree for log footage | premiere: Lumetri section order | when: grading X-M5 F-Log2 clips | see: color.nodes.orders

**[Resolve-style] [Fork]**

There is no single correct node order. What every order has in common: **each node sees the
picture the node before it produced**, so the order decides what each later correction works
on. Below is one simple, common starting point; the next sections show other orders and *why*
a colourist would choose them.

A short serial tree that works for most F-Log2 shots (one job per node, so you can switch one
off to see what it did):

1. **Balance** (first, right after the automatic input transform): Temperature / Tint and
   Offset, so whites are white and greys are grey. It goes first because every later node then works on a neutral picture: a brightness change
   on a colour cast pushes the cast further.
2. **Exposure**: Gain (brights) and Gamma (mids), until face and sky sit where you want.
3. **Contrast**: Contrast and Pivot, or a gentle S in the Curves tab.
4. **Saturation**: one slider, after step 3, because a steeper tone curve also makes colours
   look more saturated.
5. **Secondaries** (optional): a qualifier or window node for skin, sky or one object
   ([secondaries.md](secondaries.md)). It comes after the primaries so it selects colours that
   already look right; selecting from the flat log picture is harder.
6. **Look** (last or near last): anything creative, plus film grain on the very last node. This
   is one common order, not the only right one: the next sections give others and their
   trade-offs.

The input conversion before node 1 and the Rec. 709 conversion after the last node are
automatic ([color-management.md](color-management.md)). The app does not build this tree for
you; add nodes with **+ Serial**.

## Other node orders, and why you'd choose them

> id: color.nodes.orders | does: other node orders in use, with their trade-offs | resolve: node tree habits | when: your footage or habits don't fit the starter tree | see: color.nodes.starter-tree

**[Resolve-style] [Standard]**

These are all in use; pick by how you work and what the footage needs.

- **Beginner: three nodes.** Balance → Exposure / contrast → Look. Fewer nodes, less to keep
  track of; you lose the ability to switch off exposure and contrast separately. Good for a
  first edit or when shots need little work.
- **Typical: the six-step tree above.** One job per node, so a client note ("less contrast")
  touches one node, and copying a grade to the next shot copies a readable structure.
- **Noise reduction first.** Spatial NR on node 1 (Noise tab) cleans the picture before any
  contrast or saturation boost makes the noise more visible, and qualifiers key more cleanly on
  a clean picture. Some colourists put it last instead so it only runs once the look is set
  (NR is slow; on a slow machine they disable it while grading). Temporal NR isn't a node here:
  it is a clip setting that always runs before the grade ([noise.md](noise.md)).
- **Exposure before balance.** Some people set exposure first because colour casts are easier
  to judge at the right brightness. It works when the cast is small; with a strong cast,
  balance first is safer (see the starter tree).
- **LUT / colour-space bookends.** In Resolve many colourists start the tree with a colour
  space conversion node and end it with one, so every node in between works in one space. Here
  the input transform (F-Log2 → working space) and the output transform are automatic, so the
  bookends are already there. A creative LUT on a node ([finishing.md](finishing.md)) works on
  the working signal (DaVinci Intermediate in a DaVinci Wide Gamut sequence): use one made for
  that signal, or a Rec. 709 LUT only in a Rec. 709 sequence. Corrections *before* the LUT node
  change what goes into it (the LUT reacts strongly); trims *after* it only adjust its result.
- **Highlight soft clip near the end.** It shapes the brightest values after everything else
  has pushed them, so later nodes can't clip them again.
- **Grain last.** Grain added before other nodes gets graded too (its contrast and colour
  change); last, it looks the same on every shot.
- **Pro trees with parallel and layer nodes** (Resolve, and here): balance and
  exposure in series, then several secondaries side by side in a parallel node so each one
  keys from the same picture, then a look node. More flexible, but more to manage; a qualifier
  node after the primaries does the same job one at a time. See "Parallel nodes" below.

**Footage makes a difference too.** Log footage (F-Log2) needs balance and exposure before any
look; Rec. 709 footage often needs only a small balance and a look. Noisy low-light shots
benefit from NR early. With mixed cameras, balance each camera's shots to match first (a
balance node per shot), then share the look node across all of them.

## The node canvas

> id: color.nodes.canvas | does: the node canvas: boxes, wires, ports, the right-click add menu, connecting and the output | ui: grade.canvas | cmd: grade.addNode, grade.addParallel, grade.addLayer, grade.addOutside, grade.addSplitter, grade.addKeyMixer, grade.connect, grade.disconnect, grade.setOutput, grade.moveNodes, grade.resetLayout | resolve: Node editor, node graph | premiere: — | when: a grade needs more than a straight chain; reading a branched grade | see: color.nodes.which-kind, color.nodes.output, color.nodes.panel

**[Resolve-style] [Fork]**

The Nodes panel's chain is drawn on a **node canvas** ({ui:grade.canvas}). A plain serial grade is
one row: an **Input** box on the left, then your nodes, each wired to the next. A branched grade
(parallel, layer, outside, splitter / combiner, key mixer; the next sections) draws its branches
as columns. Each box is titled with its number and name ("01 Balance", "03 Layer Mixer"). Why a
canvas: once one node can feed two, a single row of boxes can no longer show what feeds what.

**Ports.** A round port carries a picture. A green triangle carries a **key**, a matte (white =
selected). A correction node has two inputs and two outputs: **RGB** (the picture) and **Key**
(the selection its qualifier and windows make). A mixer lists its inputs as 1, 2, 3 … and always
shows one free input. A splitter has outputs **R**, **G**, **B**; a combiner has inputs **R**,
**G**, **B**.

**Add a branch.** Right-click the empty canvas, or a node, and pick one of **Add Serial Node**
(`grade.addNode`), **Add Parallel Node** (`grade.addParallel`), **Add Layer Node**
(`grade.addLayer`), **Add Outside Node** (`grade.addOutside`), **Add Splitter / Combiner Nodes**
(`grade.addSplitter`) or **Add Key Mixer** (`grade.addKeyMixer`). The entry works on the node you
right-clicked, or on the selected node when you right-click the background. A node's menu also has
**Bypass**, **Set as Output**, **View**, **Delete**, **Rename** and **Reset**. Control-channel
ids: `grade.add.<kind>` for the background menu, `grade.nodeMenu.<action>` for a node's menu.

**Connect and disconnect.** Drag from an output port to an input port to wire them
(`grade.connect`: params `from`, `fromPort`, `to`, `toPort`; node 0 is the grade's input). A wire
that would make a loop is refused. Click a connected input port to cut its wire
(`grade.disconnect`). Automation ids: `grade.port.<node>.<in|out>.<port>`.

**Layout, selection, view.** Until you move anything, the canvas arranges the nodes itself (left
to right by how far they are from the input, new branches below). **Drag a node's box** to put it
where you like: it stays there after other edits and is saved with the project (one undo step per
drag, `grade.moveNodes`). The first drag pins every box where it is, so nothing else jumps; from
then on a node you add appears next to the node it was added from, and a box that would land on
another moves down. Dragging changes only the drawing, never what feeds what or the order the
nodes work in. To go back to the automatic layout, right-click the empty canvas and choose
**Reset Layout** (`grade.resetLayout`, undoable; offered once a node was moved; automation id
`grade.menu.resetLayout`). A grade nobody dragged saves exactly as before. Pan with the middle
mouse button or Space + drag, zoom with the wheel, drag on the background to box-select,
**Delete** removes the selection and **Ctrl+B** bypasses it (one undo step).

**Differences from Resolve.** You place nodes by dragging, as in Resolve; Reset Layout returns to
the automatic layout instead of Resolve's Clean Up Node Graph, and moving a node never rewires it.
A new branch is added through the menu and wired for you, as with Resolve's
Alt+ keys, but there is no "add a mixer by itself" entry: a mixer comes with the branch it mixes.

**Mixer settings.** Click a mixer and the grading palette (the Primaries panel) shows its
settings instead of the correction tabs: one row per connected input, named after the node that
feeds it ("Input 2: Node 03", or the node's name). A parallel mixer has a **Weight** per input, a
layer mixer a **Blend Mode** and an **Opacity**, a key mixer a **Mode** and an **Invert** (the
sections below). Unconnected inputs have no row. The sliders work like the other node sliders: a
drag is one undo step, ↺ resets, the diamond keyframes it. Automation ids:
`grade.mixer.<i>.weight`, `grade.mixer.<i>.mode` (its entries `grade.mixer.<i>.mode.<m>`),
`grade.mixer.<i>.opacity`, `grade.mixer.<i>.invert`, with `<i>` counting inputs from 0 (input 1 is
`0`); each slider also has `.reset` and `.anim`. Every change is `grade.setParam` on the mixer
node, so agents and scripts can still set them directly.

## Key input and key output

> id: color.nodes.key-ports | does: where a node's key output and key input are and what they carry | ui: grade.canvas | cmd: grade.connect | resolve: key input, key output (green triangles) | premiere: — | when: you want one node's selection to limit another node | see: color.nodes.outside, color.nodes.key-mixer

**[Resolve-style]**

Every correction node has a **key output**: its second output, the green triangle labelled **Key**
below the RGB output. It carries the selection that node's qualifier and windows make (white =
selected), not a picture. Its **key input** is the second input, also a green triangle: whatever
selection reaches it limits that node's correction. A node with nothing wired to its key input
works on the whole frame.

1. Find the node's **Key** output on the canvas {ui:grade.canvas}: the lower triangle on its right.
2. Drag from it to another node's **Key** input (`grade.connect` with `fromPort` 1 and `toPort`
   1). That node now works only inside the first node's selection.
3. Select the receiving node and open the **Qualifier** tab {ui:grade.tab.qualifier}: a **Key
   input** block at the top names the node the key comes from. Tick **Invert key input**
   {ui:grade.key.invert} to flip the selection (this is what **Add Outside Node** does for you);
   **Key gain** {ui:grade.key.gain} and **Key offset** (`grade.key.offset`) scale and shift it.
   They are the node parameters `key.invert`, `key.gain` and `key.offset` (`grade.setParam`).
4. To combine several selections first, use a **Key Mixer** (below).

## The output node

> id: color.nodes.output | does: which node's picture is the grade's result, and Set as Output | ui: grade.canvas | cmd: grade.setOutput | resolve: Set as Output (node graph) | premiere: — | when: the grade says several nodes could be the output; a branch should end elsewhere | see: color.nodes.canvas

**[Resolve-style] [Fork]**

**Why.** A branched grade can end in several places, but the clip gets one picture. The **output
node** is the node whose picture becomes the grade's result; its box has a highlighted header.

By default it is the last node of a chain, or the one node whose picture feeds nothing else. If
two nodes qualify, the grade cannot render until you connect them or choose one, and the box says
why.

1. Right-click the node you want on the canvas {ui:grade.canvas}.
2. Choose **Set as Output** (`grade.setOutput`; param `node`). The header moves to that node.
3. Check: the clip shows that node's picture. **Ctrl+Z** undoes it.

A splitter and a key mixer cannot be the output (the splitter makes three grey pictures, the key
mixer makes a selection, not a picture).

## Serial, parallel, layer, outside, splitter, key mixer: which one?

> id: color.nodes.which-kind | does: compares the node kinds and says when to use each | cmd: grade.addNode, grade.addParallel, grade.addLayer, grade.addOutside, grade.addSplitter, grade.addKeyMixer | resolve: serial, parallel, layer, outside nodes; splitter / combiner; key mixer | premiere: — | when: choosing how to add the next correction | see: color.nodes.canvas, color.nodes.parallel

**[Resolve-style]**

Choose by one question: *what should this correction see, and what should it be allowed to
change?*

| Kind | It works on | Reach for it when | Command | Resolve key |
|---|---|---|---|---|
| **Serial** | the picture the node before it made | one correction builds on the last (balance, then exposure) | `grade.addNode` | Alt+S |
| **Parallel** | the same picture as its twin; a mixer blends the results | two corrections must not affect each other (skin and background) | `grade.addParallel` | Alt+P |
| **Layer** | the same picture; a mixer stacks the results with opacity and a blend mode | a look you want to dial in at part strength | `grade.addLayer` | Alt+L |
| **Outside** | everything outside another node's selection | grade the background without a second qualifier | `grade.addOutside` | Alt+O |
| **Splitter / combiner** | one colour channel at a time | a fault in one channel (blue clipping) | `grade.addSplitter` | none |
| **Key mixer** | selections, not pictures: adds, subtracts or overlaps them | one selection built from several qualifiers | `grade.addKeyMixer` | none |

Start serial. Branch only when a serial chain gets in the way: a second qualifier that keys on
an already-graded picture, a look you cannot dial down, a background you keep re-selecting.
A big branched tree is harder to read than a short serial chain, which is why the explainer
(below) describes branches in words.

## Parallel nodes

> id: color.nodes.parallel | does: adds a second correction node beside one so both work on the same picture and a parallel mixer blends the results | ui: grade.canvas | cmd: grade.addParallel, grade.setParam | resolve: Add Parallel Node, Parallel Mixer | premiere: — | when: skin and background corrections that must not affect each other; two secondaries keyed from the same picture | see: color.nodes.which-kind, color.nodes.explain-branches

**[Resolve-style] [Fork]**

**Why.** In a serial chain a qualifier on node 3 keys on the picture nodes 1 and 2 already
changed. If node 2 warmed the skin, node 3's skin key sees a different colour. Two parallel nodes
both start from the **same** picture, so each key sees the original and neither disturbs the
other.

1. Select a clip and a node (the shared starting point). In the Color Grading workspace press
   **Add Node** {ui:grade.addFirst} if the clip has none.
2. Right-click the canvas {ui:grade.canvas} and choose **Add Parallel Node** (`grade.addParallel`;
   Alt+P with the Resolve keys). Two things appear: a new correction node beside the selected one,
   and a **Parallel Mixer** after both.
3. The new node is selected. Give it its own job, for example lower **Saturation**
   {ui:grade.param.saturation}. Click the first node and give it a different job.
4. Check: select one node and press **Disable** {ui:grade.toggleNode}. The picture changes by that
   branch only.
5. Optional: change the blend. By default each input counts equally (50 / 50 for two). Click the
   **Parallel Mixer** on the canvas: the palette shows a **Weight** slider (0 to 2) for each
   connected input, with the input's share of the mix beside it. Drag input 2's weight
   {ui:grade.mixer.1.weight} down to let that branch count less. Each input's share is its weight
   divided by the sum of the weights (parameters `mix.gain.0` for input 1, `mix.gain.1` for
   input 2 …; `grade.setParam` on the mixer node).
6. To add a third branch, right-click the new node and choose **Add Parallel Node** again: it
   joins the same mixer (up to 16 inputs).

**Where we differ from Resolve.** The mixer here blends by weight, so the shares always add up to
100 % and two branches that both brighten do not double the brightness. Resolve's parallel mixer
is described as adding the nodes' changes together (see
[practice/node-trees.md](../practice/node-trees.md)), so a grade built in Resolve can need its
branches turned down when rebuilt here. Check the picture and the scopes.

The Grading Coach lesson **Two corrections on the same picture (parallel nodes)** walks through
this.

## Layer nodes for looks

> id: color.nodes.layer | does: adds a layer node on top of a base node so a look can be mixed in at part opacity with a blend mode | ui: grade.canvas | cmd: grade.addLayer, grade.setParam | resolve: Add Layer Node, Layer Mixer | premiere: opacity and blend mode of an adjustment layer | when: a creative look you want to dial between 0 and 100 %; a correction that should override the base | see: color.nodes.which-kind, color.nodes.parallel

**[Resolve-style] [Fork]**

**Why.** A look folded into the base grade is all or nothing. On its own layer you can set how
strongly it shows, choose how it blends, switch it off to compare, and copy it to other shots
without their base grades.

1. Make the base grade on a node, then leave that node selected.
2. Right-click the canvas {ui:grade.canvas} and choose **Add Layer Node** (`grade.addLayer`;
   Alt+L with the Resolve keys). The new node and a **Layer Mixer** appear. The first node is the
   bottom layer (mixer input 1), the new node sits on top (input 2).
3. Build the look on the new node: Saturation {ui:grade.param.saturation}, the Gamma wheel
   {ui:grade.wheel.gamma}, a LUT.
4. Click the **Layer Mixer** on the canvas and set the look's strength with input 2's **Opacity**
   {ui:grade.mixer.1.opacity} (0 to 100 %; parameter `layer.opacity.1`, 0 to 1, through
   `grade.setParam`).
5. Set how it blends with input 2's **Blend Mode** {ui:grade.mixer.1.mode}: the same list as a
   clip's Blend Mode (parameter `layer.mode.1`, the mode's number: 0 Normal, 3 Multiply, 8 Screen,
   12 Overlay, 13 Soft Light). Normal replaces what is below; Soft Light and Overlay add the look's
   contrast and colour to it.
6. Check: **Disable** {ui:grade.toggleNode} the look node to see the base alone.

More layers: add another layer node from the look node; each higher input sits above the ones
below it, with its own opacity and mode rows (`layer.opacity.<n>`, `layer.mode.<n>`; n counts
from 0).

**Where we differ from Resolve.** The newest layer lands on top, which matches where Resolve stacks the node lowest in the graph.

The Grading Coach lesson **A look on a layer, mixed in at part strength** walks through this.

## Outside nodes

> id: color.nodes.outside | does: adds a node that works on everything outside another node's selection | ui: grade.canvas | cmd: grade.addOutside | resolve: Add Outside Node | premiere: an inverted mask on a second Lumetri | when: grading the background separately from a face, sky or object you already selected | see: color.nodes.which-kind, color.secondaries.qualifier

**[Resolve-style] [Fork]**

**Why.** To grade the background and leave the face alone you could build a second qualifier and
invert it, but the two selections then drift apart at the edges. An outside node takes the first
node's own selection and flips it, so what the first node picks and what the outside node picks
always add up to the whole frame, with no gap or overlap.

1. On a node, make a selection: a qualifier ({ui:grade.tab.qualifier}, then **Pick** on the
   skin {ui:grade.qual.pick}) or a window ([secondaries.md](secondaries.md)).
2. Right-click the canvas {ui:grade.canvas} and choose **Add Outside Node** (`grade.addOutside`;
   Alt+O with the Resolve keys). The new node follows the first one in the chain, and its **Key**
   input is wired to the first node's **Key** output with the key inverted.
3. Grade the new node with the usual controls, for example lower Saturation.
4. Change the first node's qualifier later and the outside node follows.

If the first node selects the whole frame (no qualifier or window) there is nothing outside it,
so the outside node changes nothing. The outside node's **Qualifier** tab shows its **Key input**:
**Invert key input** {ui:grade.key.invert} (ticked), **Key gain** and **Key offset**
(`key.invert`, `key.gain`, `key.offset` through `grade.setParam`) invert, scale and shift whatever
key reaches it: an outside node
is a normal node whose key is inverted, so you can also wire any key output into any node's
**Key** input yourself.

**Where we differ from Resolve.** The same idea as Resolve's outside node. The key output is the
second output of a node (the green triangle labelled **Key**); the Resolve key output behaves the
same way. The qualifier's **View** set to **Matte** {ui:grade.qual.show.2} shows a node's own matte.

The Grading Coach lesson **Grade everything except the selection (outside node)** walks through
this.

## Splitter and combiner

> id: color.nodes.splitter | does: splits a picture into red, green and blue channel nodes and combines them again | ui: grade.canvas | cmd: grade.addSplitter | resolve: Splitter / Combiner nodes, Add Splitter-Combiner | premiere: — | when: a fault in one colour channel; per-channel curves or levels | see: color.nodes.which-kind

**[Resolve-style] [Fork]**

**Why.** Most corrections move all three channels together. A splitter lets you work on one
channel alone: a clipped blue channel, a noisy red one, or a per-channel curve. Each channel
arrives as a grey picture; the combiner puts the three back into one colour picture.

1. Select the node the channels should come from.
2. Right-click the canvas {ui:grade.canvas} and choose **Add Splitter / Combiner Nodes**
   (`grade.addSplitter`). Five nodes appear: the splitter, three correction nodes named **Red**,
   **Green**, **Blue**, and the combiner. The Red node is selected and the grade's output moves to
   the combiner.
3. Click the **Blue** node and move a control, for example the Gain wheel {ui:grade.wheel.gain}.
   Only the blue channel changes, so the colour balance shifts.
4. Check: **Disable** {ui:grade.toggleNode} the Blue node to see the picture without it.

Do ordinary colour work before the splitter or after the combiner. Channel nodes see grey
pictures, so colour controls such as Saturation do nothing useful on them; use brightness and
contrast controls and curves.

**Where we differ from Resolve.** One command adds the whole splitter, three channel nodes and
combiner already wired; you can cut and rewire them like any other nodes.

The Grading Coach lesson **Grade the red, green and blue channels separately** walks through this.

## Key mixer

> id: color.nodes.key-mixer | does: combines several selections (keys) into one by adding, subtracting or intersecting them | ui: grade.canvas | cmd: grade.addKeyMixer, grade.connect, grade.setParam | resolve: Add Key Mixer, Key Mixer | premiere: — | when: a selection built from several qualifiers or windows; sky minus clouds; skin plus lips | see: color.nodes.outside, color.nodes.canvas

**[Resolve-style] [Fork]**

**Why.** Real selections are rarely one colour range or one shape. A key mixer takes selections (the **Key**
outputs of nodes, the green triangles), combines them and outputs one **Key**, which you wire into
the **Key** input of any node to limit it. The selections stay in their own nodes, so you can
retune one without rebuilding the rest.

1. Build the selections on separate nodes, each with a qualifier or window (for example skin on
   node 1, lips on node 2).
2. Right-click node 1 on the canvas {ui:grade.canvas} and choose **Add Key Mixer**
   (`grade.addKeyMixer`). A **Key Mixer** appears with node 1's Key output wired to its input 1.
3. Drag from node 2's **Key** output (the green triangle at its right) to the key mixer's input 2
   (`grade.connect` with `fromPort` 1).
4. Drag from the key mixer's output to the **Key** input of the node that should use the combined
   selection (`grade.connect` with `toPort` 1).
5. Select that node and grade it: only the combined selection is affected.
6. Optional: choose how each input combines. Click the **Key Mixer** on the canvas: each
   connected input has a **Mode** (`grade.mixer.<i>.mode`): **Add**, **Subtract**, or
   **Intersect** (keep only what overlaps the inputs before it), and an **Invert** box that flips
   that input first ({ui:grade.mixer.0.invert} for input 1). They are the parameters
   `kmix.mode.<n>` (0, 1, 2) and `kmix.invert.<n>` (n counting from 0) through `grade.setParam`.

A key mixer only makes a selection and is never the picture's output. **View** set to
**Matte** {ui:grade.qual.show.2} shows a node's own matte.

**Where we differ from Resolve.** Each input picks its own mode in a list rather than through the
key mixer's input menu.

The Grading Coach lesson **Combine two selections (key mixer)** walks through this.

## What the explainer says about branches

> id: color.nodes.explain-branches | does: the node explainer describes branched grades in plain language | ui: grade.match.explainGrade | cmd: grade.explainGrade | resolve: — | premiere: — | when: reading a branched grade you or someone else built; checking what a mixer does | see: color.nodes.canvas, color.matching.explainer

**[Fork]**

**Why.** A branched grade is hard to read from boxes alone. **Explain grade**
{ui:grade.match.explainGrade} (on the Match tab, `grade.explainGrade`) still measures what each
correction node did, and now adds one plain sentence per node that branches or selects. They are
read from the wiring and the settings, not measured, so they are exact.

1. Select a clip with a branched grade and open the **Match** tab ({ui:grade.tab.match}).
2. Press **Explain grade**. The first part is the usual per-node text (mixers and splitters do no
   grading themselves, so they are left out of it); the branch sentences follow.

What the sentences look like (names are the nodes' names, or "node N" by position):

- Parallel: "Node 3: 'Skin' and 'Background' each work on the same image; their results are blended
  50 / 50."
- Layer: "Node 3: node 2's look sits on top of node 1 at 60 % opacity, Soft Light."
- Outside: "Node 2: works on everything outside the 'Sky' node's selection."
- Splitter: "Node 3 splits the picture into its red, green and blue channels, each a grey picture: red
  goes to …"; combiner: "Node 4 puts the channels back into one colour picture (red from …)."
- Key mixer: "Node 5 builds one selection: it starts with … and takes away …."

A serial grade has no branch sentences and reads exactly as before.

## Older grades open unchanged

> id: color.nodes.older-grades | does: grades saved before branches existed open as the same serial chain | resolve: — | premiere: — | when: opening an old project; copying a grade between clips | see: color.nodes.canvas

**[Fork]**

Grades saved before the node canvas open unchanged: a clip with no stored wiring is the serial
chain it always was, and renders the same. A grade becomes branched the first time you add a
parallel, layer, outside, splitter or key-mixer node; copy, paste and append (see
[matching.md](matching.md)) carry the whole branched grade. A grade holds at most 64 nodes, and
a mixer takes at most 16 inputs.

## Undo

> id: color.nodes.undo | does: every node command is one undo step | cmd: grade.resetNode | resolve: undo, reset node | when: a node change went wrong | see: color.nodes.delete

**[Revolution]**

Every node command is one undo step. Delete a node, undo, and it comes back with its settings.
To undo a node change, press Ctrl+Z (Edit ▸ Undo) once per step; to put one node back to its
defaults instead, without undoing anything else, use Reset Node (`grade.resetNode`).

## If you know Resolve

> id: color.nodes.resolve | does: how this node graph compares with Resolve's node graph, keys and differences | resolve: Node editor | when: you come from Resolve; the Resolve-style shortcuts for nodes | see: color.nodes.canvas, color.nodes.which-kind

**[Resolve-style]**

Same idea as the node graph: serial, parallel, layer and outside nodes, splitter / combiner and
key mixer, with key ports drawn as green triangles. With Workflow set to Resolve the keys follow
Resolve's: add serial node **Alt+S**, insert before **Shift+S**, enable / disable **Ctrl+D**,
bypass **Shift+D** (Ctrl where Resolve uses Cmd on macOS), and add parallel **Alt+P**, layer
**Alt+L**, outside **Alt+O**. In the default Premiere workflow these commands have no shortcut.
If Alt+S (or Alt+P, Alt+L, Alt+O) does nothing, the Workflow is not set to Resolve: Settings ▸
General ▸ Workflow.

Where we differ:

- **Alt+P and Alt+O replace two inherited bindings, in the Resolve preset only** (Premiere's
  Clear Poster Frame and Clear Out); the Premiere preset keeps them. Splitter / combiner and key
  mixer have no key.
- **Reset Layout instead of Clean Up Node Graph**: it lays the nodes out automatically again.
- **Mixer settings are rows in the palette**, not the mixer node's right-click menu.
- **The parallel mixer blends by weight** instead of adding the branches' changes together.
- **One command adds a whole branch** (the node, its twin or mixer, and the wiring).
- Resolve's other graph features (shared and grouped nodes, node cache) are not built; see
  [practice/node-trees.md](../practice/node-trees.md).
