# Primaries

**[Resolve-style]** · the broad corrections of a node: lift, gamma, gain, offset, contrast, saturation, white balance

## Why

> id: color.primaries.why | does: why primaries come first: they change the whole picture or whole brightness ranges | resolve: Primaries | premiere: Lumetri Basic Correction, Color Wheels | when: starting a grade | see: color.primaries.panel

**[Resolve-style]**

Primaries change the whole picture, or whole brightness ranges of it. Do these whole-picture
fixes first, before the skin or any one colour; fine work on
one colour (secondaries, G2) comes after.

## The panel

> id: color.primaries.panel | does: Lift, Gamma, Gain and Offset wheels, contrast, pivot, saturation, temperature and tint | ui: grade.wheel.*, grade.param.*, grade.tab.primaries | cmd: grade.setParam | resolve: Primaries Color Wheels | premiere: Lumetri Color Wheels | when: balancing and shaping a shot; making it black and white | see: color.primaries.white-balance

**[Resolve-style]**

Select a clip and a node. The **Primaries** panel has two tabs: **Primaries**
{ui:grade.tab.primaries} and **Curves** {ui:grade.tab.curves}. At the right of the tab row it
names the node you edit ("Node 02 · name"). With no grade it shows **Add Node**
{ui:grade.addFirst}.

### The four wheels

Each wheel has a puck (colour balance) and a ring (brightness of that range, the "master"):

| Wheel | Id | Moves | Use it to |
|---|---|---|---|
| **Lift** | {ui:grade.wheel.lift} | darkest tones | set the black level, tint shadows |
| **Gamma** | {ui:grade.wheel.gamma} | midtones | brighten or darken the middle, tint it |
| **Gain** | {ui:grade.wheel.gain} | brightest tones | set the white level, tint highlights |
| **Offset** | {ui:grade.wheel.offset} | everything evenly | overall brightness or colour cast |

The palette is laid out like Resolve's Primaries: a strip of numbers along the top, the four
wheels, and a second strip under them. Each wheel has a reset (↺) beside its title, id like
{ui:grade.wheel.lift.reset}. Under each wheel four boxes show the master (Y, white band), red,
green and blue offsets; drag a box sideways or click it and type. Under the boxes the ridged
**jog bar** is the master, the brightness of that range (id like {ui:grade.wheel.gamma.master}):
drag it left or right, double-click it to return to 0. Drag the puck toward a colour to add it;
double-click the wheel to centre the puck. Neutral is all zeros.

There is no single right order. Many colourists balance with Offset first, then shape with Lift /
Gamma / Gain; others set the black and white points with Lift and Gain first and balance after.
Try both on the same shot and keep the one that gets you there faster.

### The number strips

| Label | Id | Range (neutral) | Does |
|---|---|---|---|
| **Temp** | {ui:grade.param.temperature} | -100 to 100 (0) | cool to warm (blue → orange band) |
| **Tint** | {ui:grade.param.tint} | -100 to 100 (0) | green to magenta |
| **Contrast** | {ui:grade.param.contrast} | 0 to 4 (1) | stretches brightness around the pivot |
| **Pivot** | {ui:grade.param.pivot} | 0 to 1 (0.336, mid grey) | the brightness contrast leaves alone |
| **Saturation** | {ui:grade.param.saturation} | 0 to 4 (1) | 0 is black and white |
| **Grain**, **Grain size**, **Grain softness** | {ui:grade.param.grain.strength} | see [finishing.md](finishing.md#film-grain) | film grain |

Drag a box sideways or type a value. **Double-click a label** to reset that control (the label's
id is like {ui:grade.param.contrast.reset}). The node's **Reset** {ui:grade.resetNode} returns
every control of the node to neutral. The node's LUT menu sits under the bottom strip.

## White balance picker and Auto balance

> id: color.primaries.white-balance | does: removes a colour cast by picking something neutral, or automatically | ui: grade.wb.pick, grade.wb.auto | cmd: grade.wb.pick, grade.wb.auto | resolve: Highlight / white balance picker, Auto Balance | premiere: Lumetri WB Selector | when: orange, blue, green or magenta casts; nothing grey or white to pick, balance it by itself | see: color.primaries.panel

**[Resolve-style]**

**Why:** a colour cast (orange tungsten light, blue shade, a green fluorescent tube, a magenta
LED panel) is easiest to see on something that *should* have no colour: a grey card, a white wall,
a shirt, a road sign's white. Finding the right Temperature and Tint by eye takes a while and two
sliders interact; the picker solves both at once from that one neutral spot.

1. Select the clip and the node to balance (usually the first one), open **Primaries**.
2. Click the pen icon at the start of the top strip, **WB picker** {ui:grade.wb.pick}. The
   Program monitor shows a crosshair.
3. Click on something that should be grey or white, or drag a box over a larger neutral area
   (a box averages out noise and texture). Esc cancels.
4. **Temperature** and **Tint** move so that spot comes out neutral; the status line shows the
   values. The sliders stay adjustable: warm it back up a little by hand if the scene should
   feel warm.
5. No obvious neutral? Click the round **A** before it, **Auto balance** {ui:grade.wb.auto}: it averages the near-grey,
   mid-tone parts of the whole frame (skipping clipped highlights, deep shadows and strongly
   coloured areas) and balances on that.

Each pick or Auto balance is one undo step. The picker reads the picture **as it enters this
node**, so picking the same spot again gives the same values instead of over-correcting. It
refuses, and says why, a spot that is clipped (its colour is lost), too dark (noise decides) or
too colourful to be neutral. "Clipped" is judged on the clip's own footage, before any node or
Input LUT: a channel that reached the top of the camera's signal (code value 1.0 of SDR, HLG or
log footage such as F-Log2, or the content peak of PQ footage) counts as blown out in every
sequence, including DaVinci Wide Gamut and HDR ones, where values above 1.0 are otherwise real
highlights. When a cast is stronger than the sliders reach (±100), they stop at
the limit and the status line says the cast is only partly removed: balance the rest with the
**Gain** or **Offset** wheel. Keyframed Temperature / Tint get a keyframe at the playhead.

Difference from Resolve: Revolution's picker and Auto balance only move this node's Temperature
and Tint (two controls in linear light, see the sliders above); Resolve's tools may spread the
correction differently, so the numbers won't match between the two applications.

## Steps: fix a flat F-Log2 clip

> id: color.primaries.flog2-steps | does: steps to turn a flat F-Log2 clip into a balanced picture | ui: grade.addFirst, grade.param.contrast, grade.param.pivot | cmd: grade.setParam | resolve: first primary correction on log | premiere: Lumetri Basic Correction on log | when: the first node of an X-M5 clip | see: color.color-management.input

**[Fork]**

1. Confirm the clip is interpreted as F-Log2 ([color-management.md](color-management.md)).
2. Select the clip, add a node, open **Primaries**.
3. Drag the **Gain** jog bar right until highlights sit where you want, the **Lift** jog bar until blacks
   are black.
4. Raise **Contrast** in small steps; keep **Pivot** at 0.336 unless faces get too dark.
5. Raise **Saturation** until colours look right.
6. Fix a colour cast with **WB picker** on something neutral (or **Auto balance**), then adjust
   **Temperature** / **Tint** by taste.
7. Check with the **Lumetri Scopes** panel, then press **Bypass** {ui:grade.bypass} to compare.

Dragging a control is one undo step no matter how long you drag.

## HDR zone wheels

> id: color.primaries.hdr | does: exposure, saturation and balance per tonal zone in stops from mid grey, with zone ranges and falloff | ui: grade.tab.hdr, grade.hdr.wheel.*, grade.hdr.graph, grade.hdr.reset | cmd: grade.setParam, grade.hdr.reset | resolve: HDR palette zone wheels | premiere: none | when: taming a bright sky or lifting deep shadows without moving the mid-tones | see: color.primaries.hdr-steps

**[Resolve-style]**

**Why:** Lift / Gamma / Gain cover three wide, overlapping ranges, so pulling a bright sky down
also dims faces a little. The HDR wheels work like a camera: each zone is a band of brightness
measured in **stops** (doublings of light) from mid grey, and each wheel's exposure is in stops
too. A sky three stops over grey can come down a stop while a face at grey stays put. On
wide-range camera material in a DaVinci Wide Gamut sequence this is often the more natural tool;
for a quick SDR fix the four wheels may be faster. Both sit in the same node.

Open the **HDR** tab {ui:grade.tab.hdr} next to **Primaries**. The graph at the top
{ui:grade.hdr.graph} shows where each zone acts (shadows on the left, highlights on the right,
the middle line is mid grey; a zone you changed is drawn in the accent colour). Then seven wheels:

| Wheel | Acts on | Default edge |
|---|---|---|
| **Global** {ui:grade.hdr.wheel.global} | everything | none |
| **Black** | the deepest blacks, below its edge | −4 stops |
| **Dark** | below its edge | −2.5 stops |
| **Shadow** | below its edge | −1 stop |
| **Light** | above its edge | +1 stop |
| **Highlight** | above its edge | +2.5 stops |
| **Specular** | glints, lamps, the sun | +4.5 stops |

Under each wheel: **Exposure** (stops, ±4) and **Saturation** (1 = unchanged); the six zones also
have **Range** (where the edge sits) and **Falloff** (how soft the edge is; never 0, so no
banding). The puck tints the zone without changing its brightness. **Reset All**
{ui:grade.hdr.reset} clears every wheel.

In an SDR (Rec. 709) sequence display white sits only about 2.5 stops above mid grey, so
**Specular** rarely has anything to act on and **Highlight** reaches only the brightest part of
the picture; move their **Range** down if you want them to do more there. Wide-range sequences
(DaVinci Wide Gamut, PQ, HLG) keep many more stops above grey.

## Steps: bring a bright sky down with the HDR wheels

> id: color.primaries.hdr-steps | does: steps to lower a bright sky with the Highlight zone and refine its range | ui: grade.tab.hdr, grade.hdr.wheel.*, grade.hdr.reset | cmd: grade.setParam | resolve: HDR palette | when: a sky or window is too bright but faces are fine | see: color.primaries.hdr

**[Resolve-style]**

1. Select the node, open **HDR**.
2. Drag **Highlight** ▸ **Exposure** left (try −0.5 to −1). The sky comes down; faces near mid
   grey stay.
3. If bright skin dims too, move **Highlight** ▸ **Range** right (2.5 → 3); if the transition
   looks abrupt, raise **Falloff**.
4. Optional: tint the highlights towards blue with the **Highlight** wheel, or lower its
   **Saturation**.
5. For lamps or the sun only, use **Specular**: its edge sits higher.

A beginner can go far with **Global** plus one zone; a colourist may set Shadow and Light first,
then trim Black and Specular. Neither order is wrong: try it, watch the scopes, undo.

## HDR wheels compared with Resolve

> id: color.primaries.hdr-resolve | does: how the HDR zone wheels combine and differ from Resolve's HDR palette; their parameter keys | cmd: grade.setParam, grade.hdr.reset | resolve: HDR palette | when: you come from Resolve or script the wheels | see: color.primaries.hdr

**[Resolve-style]**

Zones nest (a deep shadow is in Black and Shadow at once, and in the zone between): exposures add in stops,
saturations multiply, balances add. Exposure is a gain on linear light, measured on the working
gamut's luminance; the zones run after the node's primaries and saturation,
before its colour warper and curves. The zone layout follows Resolve's HDR palette as its manual describes it; the
maths is Revolution's own (our default edges and falloffs, mid grey fixed at 18 %), and there is
no Black Offset, pivot or nit-based zone setting. Equal numbers won't look equal in both
applications.

Keys for `grade.setParam`: `hdr.<zone>.exposure` / `sat` / `balance` (a wheel `[r, g, b]`) /
`range` / `falloff`, `<zone>` being `global` (no range or falloff) or a zone's name in lower case
(`black` … `specular`). `grade.hdr.reset` with `zone` resets one wheel, without it all.
Automation ids: `grade.hdr.wheel.<zone>` (and `.reset`), `grade.hdr.<zone>.exposure` / `.sat` /
`.range` / `.falloff`.

## Commands

> id: color.primaries.commands | does: the grade commands for scripts and agents | cmd: grade.setParam, grade.params, grade.get, grade.toggleAnimation | when: scripting a grade | see: color.primaries.panel

**[Fork]**

From a script or an agent, `grade.setParam` sets one parameter of a node (`param`, `value`; a
node's contrast is `contrast`); `grade.params` lists every parameter with
its range; `grade.get` returns the whole grade. `grade.toggleAnimation` turns keyframes on for a
parameter at a time; the panel has no keyframe button yet and shows the value at the playhead.
Values read from a file are clamped to the ranges above. `grade.wb.pick` runs the white balance picker for an agent or script on a
point or a box of the frame (`x`, `y`, optionally `x2`, `y2`: 0…1 of the frame; a point samples a 5 × 5 pixel spot, two corners a box) and `grade.wb.auto` solve the
node's Temperature and Tint and return them, with `clamped` and a `message` when the sliders hit
their limit.

## If you know Resolve

> id: color.primaries.resolve | does: how these wheels compare with Resolve's primaries | resolve: Lift / Gamma / Gain / Offset | when: you come from Resolve | see: color.primaries.panel

**[Resolve-style]**

Lift / Gamma / Gain / Offset do the same jobs, but they act on the sequence's grading signal
(DaVinci Intermediate in a DaVinci Wide Gamut sequence) and the numbers shown are the wheel
offsets, not Resolve's internal values. There are no separate shadows / highlights /
colour-boost / mid-detail sliders yet.

## If you know Premiere

> id: color.primaries.premiere | does: how the node grader relates to Lumetri's wheels and sliders | premiere: Lumetri Color | when: you come from Premiere | see: color.lumetri.what

**[Revolution]**

Lumetri's wheels and sliders are a separate tool (**[Revolution]**) that lives in the clip's
effect stack. Nodes run before the effect stack, so a node grade and Lumetri can be combined.
See [lumetri.md](lumetri.md) for when to use which.
