# Practice notes: node trees and advanced grading

**[Standard]** · techniques that rely on node structure and the more specialised grading tools,
one per note, with the Resolve way, the Premiere way where there is one, and what Revolution offers
(several are "not yet": said in place). Written in our own words from common practice (AI-PLAN
AI3 item 0a, 0c).

## A fixed node tree for every shot

> id: practice.node-trees.fixed-tree | does: uses the same node layout on every shot so any node does the same job everywhere | resolve: a saved node tree applied to every clip | premiere: — (one Lumetri per clip) | when: long projects, many shots, handing a grade to someone else | see: color.nodes.starter-tree, practice.grading.look-node

**[Standard]**

- **Common practice:** the same labelled nodes in the same order on every clip (for example
  Balance → Contrast → Skin → Sky → Look → Grain), many of them empty on simple shots.
- **Revolution:** build it once, label nodes (`grade.labelNode`), save a still of it and apply it
  to other clips (`stills.apply`) or copy / append (`grade.copy`, `grade.paste`).
- **Why:** you always know where a correction lives; matching and revisions are faster.
- **Pitfalls:** empty nodes cost nothing, but a tree too big to read defeats the purpose.

## Parallel nodes: corrections side by side

> id: practice.node-trees.parallel | does: feeds two or more nodes the same input and mixes their outputs | resolve: Parallel node / Parallel Mixer | premiere: — | when: two secondaries that must not affect each other's selection | see: color.nodes.parallel

**[Standard]**

- **Resolve:** the parallel mixer adds the nodes' changes together; each node keys from the same
  untouched picture.
- **Revolution:** **Add Parallel Node** (`grade.addParallel`) on the canvas; the mixer blends the
  branches by weight (50 / 50 for two) instead of adding their changes together, so check the
  picture when you rebuild a Resolve tree. See [color/nodes.md](../color/nodes.md) ▸ Parallel nodes.
  A qualifier placed earlier in a serial chain still protects it in simple cases.
- **Pitfalls:** two parallel nodes changing the same area add up and overshoot.

## Layer nodes: the lower one wins

> id: practice.node-trees.layer | does: stacks node outputs so the bottom one replaces the top one where its key allows | resolve: Layer Mixer node, composite modes | premiere: — | when: a secondary that must override the look, blend modes between grades | see: color.nodes.layer

**[Standard]**

- **Revolution:** **Add Layer Node** (`grade.addLayer`): each input above the first has its own
  opacity and blend mode (`layer.opacity.<n>`, `layer.mode.<n>` through `grade.setParam`; no
  sliders yet). See [color/nodes.md](../color/nodes.md) ▸ Layer nodes for looks. A late node with
  a qualifier or window does the same job in simple cases.

## The outside node: everything but the selection

> id: practice.node-trees.outside | does: grades the inverse of a previous node's key, such as the background around a face | resolve: Add Outside Node | premiere: Invert Mask in a second Lumetri | when: face and background graded separately | see: color.secondaries.windows

**[Standard]**

- **Revolution:** **Add Outside Node** (`grade.addOutside`) takes the first node's own selection,
  inverted ([color/nodes.md](../color/nodes.md) ▸ Outside nodes). Inverting the window or
  qualifier on a second node (`grade.window.0.invert`, `grade.qual.invert`) also works when the
  shapes are the same.
- **Pitfalls:** two separately drawn shapes drift apart; copy the node and invert it instead of
  redrawing.

## Group (pre- and post-clip) grades

> id: practice.node-trees.groups | does: applies one grade before or after every clip of a group, such as one camera or one scene | resolve: Group Pre-Clip / Post-Clip | premiere: adjustment layer over a scene | when: one correction per camera, one look per scene | see: practice.grading.look-node, media.generated

**[Standard]**

- **Revolution:** no grade groups yet; for a look over a scene, an **Adjustment Layer** above the
  clips (`file.newAdjustmentLayer`) with Lumetri on it; for per-camera fixes, append the same node
  (`grade.append`).
- **Pitfalls:** an adjustment layer's grade sits after each clip's own nodes and their output
  transform.

## Shared nodes

> id: practice.node-trees.shared | does: one node used in many clips' trees, so changing it changes all of them | resolve: Shared Node | premiere: — | when: a look or a fix that must stay identical across shots | see: practice.node-trees.groups

**[Standard]**

- **Revolution:** not yet; re-apply with `grade.append` after changing the source node.

## Hue curves for single-colour tweaks

> id: practice.node-trees.hue-curves | does: changes the hue, saturation or brightness of one colour range with curves | resolve: Hue vs Hue / Sat / Lum, Lum vs Sat | premiere: Lumetri Hue Saturation Curves | when: a green that's too yellow, saturated highlights to tame | see: color.curves.limits, color.lumetri.panel

**[Standard]**

- **Revolution:** the node grader's hue curves are not yet built; Lumetri's **Hue Saturation
  Curves** do it ([color/lumetri.md](../color/lumetri.md)), or a qualifier on that colour.
- **Pitfalls:** narrow hue-curve bumps tear gradients in compressed footage.

## Desaturate highlights and shadows

> id: practice.node-trees.sat-by-luma | does: lowers saturation in the very bright and very dark parts for a cleaner, filmic look | resolve: Lum vs Sat curve, Highlight / Shadow saturation | premiere: Lumetri Saturation curves (Luma vs Sat) | when: colourful noise in shadows, neon highlights | see: practice.node-trees.hue-curves

**[Standard]**

- **Revolution:** Lumetri's Luma vs Sat curve, or a qualifier on luma only with saturation down on
  that node.

## Printer-light style balancing with Offset

> id: practice.node-trees.printer-lights | does: balances and exposes with Offset in fixed steps per channel, like film printer lights | resolve: Offset wheel, Printer Lights | premiere: — | when: quick, repeatable balance changes on log | see: color.primaries.panel

**[Standard]**

- **Common practice:** printer lights move red, green and blue (or all three) in fixed small steps
  of Offset, one channel at a time, like a film lab's printer.
- **Revolution:** the Offset wheel on the balance node (`grade.wheel.offset`); no printer-light
  step keys yet.
- **Why:** in a log working space Offset moves everything evenly, like changing exposure.

## Gamut compression for neon and LED colours

> id: practice.node-trees.gamut-compress | does: pulls colours that fall outside the delivery gamut back in smoothly instead of clipping | resolve: Gamut Compression / Gamut Mapping OFX | premiere: — | when: LED stage lights, neon signs, saturated sunsets; blue or red lights that look clipped, flat or turn purple | see: color.color-management.output

**[Standard]**

- **Revolution:** the output transform and highlight roll-off handle brightness; a dedicated gamut
  compressor isn't offered yet. Lower saturation on a qualifier for the offending colour.
- **Pitfalls:** colours outside the delivery gamut: clipped blues turning purple, reds going flat.

## Glow and halation

> id: practice.node-trees.glow | does: adds a soft glow or a film-style red halo around bright edges | resolve: Glow, Halation (Studio OFX) | premiere: a blurred, blended duplicate layer | when: dreamy or film looks, highlights that look too clean | see: practice.grading.grain-finish

**[Standard]**

- **Glow:** a clip effect, not a node: **Wonder Glow** (Effects ▸ Video Effects ▸ Lights &
  Glows; Threshold, Radius, Intensity, an optional Glow Color, Add or Screen) blooms everything
  above the threshold; **Edge Glow** and **Echo Glow** are in the same folder
  ([effects.md](../effects.md) ▸ stylised effects).
- **Revolution:** no halation effect yet; for a film-style red halo use Wonder Glow with a
  red-orange Glow Color, a high Threshold and a small Radius, or a duplicate of the clip above
  with a high-luma key, Gaussian Blur and a Screen blend. Glows are clip effects, not nodes.

## Skin softening

> id: practice.node-trees.skin-soften | does: softens skin texture inside a skin key while keeping eyes and edges sharp | resolve: Beauty OFX, or blur in a skin qualifier | premiere: — (third-party plugins) | when: close-ups, interviews | see: practice.grading.qualify-skin

**[Standard]**

- **Revolution:** a skin qualifier node with Spatial noise reduction at a low strength
  ([color/noise.md](../color/noise.md)) softens texture inside the key; no dedicated beauty tool yet.
- **Pitfalls:** plastic-looking skin; keep it subtle and keep eyes, lips and hair sharp (outside
  the key).

## Sharpening at the end, sparingly

> id: practice.node-trees.sharpen | does: adds a little sharpening after the grade, mainly for downscaled or soft footage | resolve: Blur / Sharpen palette on a last node | premiere: Unsharp Mask, Lumetri Sharpen | when: soft lenses, heavy downscales | see: practice.grading.grain-finish

**[Standard]**

- **Revolution:** the Unsharp Mask or Sharpen effects on the clip ([effects.md](../effects.md)).
- **Pitfalls:** halos on edges and noise; sharpen before grain, not after.

## Window tracking on moving subjects

> id: practice.node-trees.track-window | does: tracks a window to a moving face or object so the correction stays on it | resolve: Tracker palette | premiere: mask tracking | when: any window on a subject that moves | see: color.secondaries.tracking

**[Standard]**

- **Revolution:** the window's track buttons (`grade.window.0.track.forward`); check the track in
  playback and fix the few frames where it slips.
- **Pitfalls:** tracking a soft window on a featureless area; track something with texture.

## Versions of a grade for review

> id: practice.node-trees.versions | does: keeps alternative grades of a shot to compare or show a client | resolve: Local / Remote grade versions | premiere: — | when: client reviews, A / B looks | see: color.matching.stills

**[Standard]**

- **Revolution:** no grade versions yet; save each alternative as a still (`stills.grab`) and apply
  the chosen one (`stills.apply`).
