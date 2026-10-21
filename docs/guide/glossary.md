# Glossary

**[Standard] [Resolve-style] [Revolution] [Fork]** · plain-language meanings of the words this
guide uses, with what Resolve and Premiere call the same thing

Each entry says what the word means, why it matters to you, and where to find it in this fork.
The "Resolve" and "Premiere" columns give the nearest name in each program, so a habit from
either transfers; "—" means that program has no direct equivalent. Every entry carries a tag
saying where the idea comes from (see [README](README.md#where-each-feature-comes-from-provenance)).

## At a glance

| This guide | Resolve calls it | Premiere calls it |
|---|---|---|
| log footage | log / camera log | log footage |
| gamut | gamut / colour space | colour space |
| working space | timeline colour space | working colour space (sequence) |
| node | node (corrector node) | — (closest: one Lumetri Color effect) |
| serial node | serial node | — (closest: stacking effects) |
| wheel | colour wheel | colour wheel (Lumetri) |
| curve | curve | curve (Lumetri RGB / Hue Saturation curves) |
| still | still (gallery) | — (closest: comparison view reference frame) |
| wipe | wipe / split screen | comparison view (split) |
| key | key / qualifier | HSL Secondary key |
| matte | matte / key output | mask (track matte for layers) |
| LUT | LUT | LUT (Lumetri Input / Creative LUT) |
| grain | film grain | grain (noise effect) |
| proxy | proxy media / optimized media | proxy |
| render cache | render cache | render files / preview files |
| timeline (Resolve workflow wording; see [getting-started](getting-started.md#terminology-follows-the-workflow)) | timeline | sequence |

## Log

> id: glossary.log | does: a way of storing brightness that keeps very dark and very bright parts in the file | resolve: log (e.g. F-Log2) | premiere: log | when: footage looks flat and grey | see: color.color-management.why

**[Standard]**

A way a camera stores brightness so that very dark and very bright parts both fit in the file.
It squeezes highlights and shadows together, so straight out of the camera a log clip looks
grey and flat. That is why a camera records a picture that looks worse at first: it is raw material, not a finished picture. Revolution turns it
back into a normal-looking picture once it knows which log curve the camera used (for the
X-M5, **F-Log2**). See [colour management](color/color-management.md).

- Resolve: log, or the camera's name for it. Premiere: log footage.

## Gamut

> id: glossary.gamut | does: the range of colours a signal can describe | resolve: color gamut | premiere: color gamut | when: reading colour-space names | see: glossary.working-space

**[Standard]**

The range of colours a signal can describe. A camera gamut such as **F-Gamut** holds more
saturated colours than a normal screen (**Rec. 709**) can show. When a wide gamut is shown on a
smaller one, the colours outside have to be brought inside somehow; this fork folds them in
gently instead of cutting them off.

- Resolve: gamut / colour space. Premiere: colour space.

## Working space

> id: glossary.working-space | does: the colour space every clip is translated into before you grade | resolve: timeline color space | premiere: working color space | when: setting up colour management | see: color.color-management.working

**[Standard] [Fork]**

The one colour space every clip is translated into before you grade, so the same slider move
does the same thing on every clip. This fork's recommended working space is **DaVinci Wide
Gamut / DaVinci Intermediate** (from Blackmagic's published specification). You choose it per
sequence in Sequence ▸ **Color Management…** (`sequence.colorSettings`). At the end the working
space is converted to the output (normally Rec. 709).

- Resolve: timeline colour space (with colour management on). Premiere: the sequence's working
  colour space.

## Node

> id: glossary.node | does: one step of a clip's grade, holding a set of corrections | ui: grade.addNode | cmd: grade.addNode | resolve: node | premiere: — (a Lumetri effect instance, closest) | when: learning the node grader | see: color.nodes.why

**[Resolve-style]**

One step of a clip's grade. A node holds a set of corrections (wheels, curves, a key, a window,
noise reduction, a LUT, grain) and can be switched off on its own. The idea is **one job per
node**, so you can see and undo each step separately. Nodes appear as numbered boxes in the
**Nodes** panel; **+ Serial** {ui:grade.addNode} (`grade.addNode`) adds one. See
[nodes](color/nodes.md).

- Resolve: node (corrector node). Premiere: no direct equivalent; the nearest is one Lumetri
  Color effect on a clip.

## Serial node

> id: glossary.serial-node | does: a node that passes its result to the next one | resolve: serial node | premiere: — | when: reading a node chain | see: glossary.node

**[Resolve-style]**

A node that takes the picture from the node before it and hands its result to the next one:
node 1 → node 2 → node 3. Order matters, because each node sees what the earlier ones already
did. A grade starts out serial; parallel, layer, outside, splitter / combiner and key-mixer nodes
are added when needed ([nodes](color/nodes.md)).

- Resolve: serial node. Premiere: no direct equivalent; stacking several effects on a clip
  behaves similarly.

## Wheel

> id: glossary.wheel | does: a round control that pushes colour towards a hue and sets brightness | resolve: color wheel | premiere: Lumetri color wheel | when: balancing shadows, midtones, highlights | see: color.primaries.panel

**[Resolve-style] [Revolution]**

A round control that pushes the picture's colour towards a hue (drag the dot) and its
brightness up or down (the slider under it). There are four: **Lift** (shadows), **Gamma**
(mid-tones), **Gain** (highlights) and **Offset** (everything at once). See
[primaries](color/primaries.md).

- Resolve: colour wheels (Lift / Gamma / Gain / Offset). Premiere: Lumetri's colour wheels
  (Shadows / Midtones / Highlights).

## Curve

> id: glossary.curve | does: a graph that maps input to output brightness, hue or saturation | resolve: curves | premiere: Lumetri curves | when: shaping contrast or one colour | see: color.curves.why

**[Resolve-style] [Revolution]**

A graph that maps input to output: drag a point up and that brightness (or hue, or saturation)
comes out higher. The custom curve shapes contrast; the hue and saturation curves change one
colour range at a time ("make only the blues less saturated"). See [curves](color/curves.md).

- Resolve: curves (custom, hue vs hue, hue vs sat, …). Premiere: Lumetri RGB curves and Hue
  Saturation curves.

## Still

> id: glossary.still | does: a saved frame with the grade that was on its clip | ui: stills.grab | cmd: stills.grab | resolve: still (Gallery) | premiere: — (reference frame) | when: keeping a reference look | see: color.matching.stills

**[Resolve-style]**

A saved frame plus the grade that was on its clip when you saved it. You keep stills as
references: to compare a new shot against, or to give the same grade to other clips. **Grab
Still** {ui:stills.grab} (`stills.grab`) saves a still of the shot under the playhead into the
gallery. See
[matching](color/matching.md).

- Resolve: still (in the gallery). Premiere: no direct equivalent; the comparison view's
  reference frame is the nearest.

## Wipe

> id: glossary.wipe | does: shows a still and the current picture at once, divided by a line | cmd: compare.set | resolve: wipe | premiere: Comparison View | when: matching shots by eye | see: color.matching.compare

**[Resolve-style]**

A way to see a still and the current picture at once, divided by a line you can drag, so you
can judge whether two shots match. **Split** shows them side by side instead. Both are set with
`compare.set`.

- Resolve: wipe / split screen. Premiere: comparison view (split mode).

## Key

> id: glossary.key | does: a selection of part of the picture by colour or brightness | ui: grade.tab.qualifier | resolve: key (qualifier) | premiere: HSL Secondary key | when: what a key is in grading; changing one colour | see: color.secondaries.qualifier

**[Standard] [Resolve-style]**

A key, in grading, is a selection of part of the picture made **by its colour or brightness**, for example "the skin"
or "the sky", so a node changes only that part. This fork makes keys with the **qualifier**
(the Qualifier tab {ui:grade.tab.qualifier}). The difference from a window: a key selects by
colour, a window by a shape you draw; the two can be combined. See [secondaries](color/secondaries.md).

- Resolve: key, made with a qualifier. Premiere: HSL Secondary key in Lumetri.

## Matte

> id: glossary.matte | does: the black-and-white picture of where a key or window acts | ui: grade.qual.show.2 | resolve: matte / highlight | premiere: mask / show mask | when: checking a key | see: glossary.key

**[Standard]**

The black-and-white picture a key or window produces: white where the node acts, black where
it doesn't, grey for partly. Looking at the matte (Qualifier ▸ View ▸ **Matte**
{ui:grade.qual.show.2}) is the quickest way to see whether a selection is clean.

- Resolve: matte / key output (highlight mode). Premiere: the HSL Secondary "mask" view; for
  layers, a track matte.

## Window

> id: glossary.window | does: a selection of part of the picture by shape | ui: grade.tab.windows | resolve: Power Window | premiere: mask | when: a vignette, a face, a sky | see: color.secondaries.windows

**[Resolve-style]**

A selection of part of the picture made **by shape** (circle, rectangle, gradient) instead of by
colour. Windows can follow a moving object with tracking. The Windows tab is
{ui:grade.tab.windows}. See [secondaries](color/secondaries.md#windows-select-by-shape).

- Resolve: window (power window). Premiere: mask on an effect.

## LUT

> id: glossary.lut | does: a file that maps every input colour to an output colour | ui: grade.lut | cmd: lut.import | resolve: LUT | premiere: LUT (Input LUT, Look) | when: camera conversions, ready-made looks | see: color.finishing.lut

**[Standard]**

A look-up table: a file (`.cube` or `.3dl`) that maps every input colour to an output colour.
Used to convert between colour spaces or to apply a ready-made look. A LUT can't adapt to the
shot, so it is best used as one step in a node, not as the whole grade. **Import LUT…**
(`lut.import`) adds one to the project; the **LUT** menu {ui:grade.lut} puts it on a node. See
[finishing](color/finishing.md#a-lut-on-a-node).

- Resolve: LUT. Premiere: Lumetri's Input LUT and Creative Look.

## Grain

> id: glossary.grain | does: fine random texture added on purpose, like film grain | ui: grade.param.grain.strength | resolve: Film Grain | premiere: grain / noise | when: hiding banding, matching sources | see: color.finishing.grain

**[Standard] [Fork]**

Fine random texture added on purpose, like the grain of photographic film. It hides banding,
makes a heavily graded shot feel less "digital" and ties shots from different cameras together.
Set on a node with **Grain strength** {ui:grade.param.grain.strength}. See
[finishing](color/finishing.md#film-grain).

- Resolve: film grain. Premiere: no built-in film grain in Lumetri; a noise effect is the
  nearest.

## Proxy

> id: glossary.proxy | does: a smaller copy of a clip used while editing | cmd: media.createProxies, media.toggleProxies | resolve: proxy media | premiere: proxy | when: heavy footage | see: proxies.what

**[Revolution]**

A smaller, easier-to-decode copy of a clip used while editing, so playback stays smooth; the
export still uses the original. Clip ▸ Proxy ▸ **Create Proxies…** (`media.createProxies`)
makes them and View ▸ **Toggle Proxies** (`media.toggleProxies`) switches between proxies and
originals. Revolution also makes them on its own, in the background, for 4K and larger long-GOP
clips ([automatic proxies](proxies.md#automatic-proxies)).

- Resolve: proxy media / optimized media. Premiere: proxy.

## Render cache

> id: glossary.render-cache | does: finished frames of part of a sequence saved to disk for smooth playback | cmd: sequence.renderInToOut, sequence.deleteRenderFiles | resolve: render cache | premiere: preview files | when: heavy sections that stutter | see: render-cache.why

**[Revolution]**

Finished frames of part of a sequence saved to disk, so a heavy section (many effects, a big
grade) plays back without being worked out again every time. Sequence ▸ **Render In to Out**
(`sequence.renderInToOut`) fills it for the marked range and **Delete Render Files**
(`sequence.deleteRenderFiles`) throws it away. **[Fork]** The automatic render cache also does
this on its own: while you are idle it renders heavy stretches in the background and stops the
moment you play, scrub or export ([render-cache.md](render-cache.md)).

- Resolve: render cache. Premiere: rendering previews (render files).
