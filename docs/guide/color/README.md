# Colour in this fork: start here

**[Resolve-style] [Standard] [Fork]** · status: written for G0 + G1 (what exists today)

This fork adds a **node grader**: a list of small correction steps (nodes) on each clip, in the
spirit of DaVinci Resolve's Color page. It sits next to Revolution's Premiere-style **Lumetri
Color** panel; you can use either, or both.

## The idea in one minute

> id: color.readme.idea | does: why F-Log2 looks flat and how a colour-managed grade turns it into a finished picture | resolve: colour-managed grading | premiere: log footage with Lumetri | when: why log footage looks washed out, grey and flat out of the camera; starting to grade it | see: color.color-management.why

**[Standard]** **[Fork]**

Footage from the Fujifilm X-M5 in F-Log2 looks flat and grey on purpose: it keeps as much picture
as the sensor saw. Colour work has three jobs (the big stages), always in this order:

1. **Translate** the footage into a common working space, so every clip starts from the same
   ground. Done for you by colour management ([color-management.md](color-management.md)).
2. **Grade** it: balance, exposure, contrast, saturation, look. This is what nodes are for
   ([nodes.md](nodes.md), [primaries.md](primaries.md), [curves.md](curves.md)).
3. **Deliver** it: the working space is converted to Rec. 709 for normal screens. Also automatic.

For every clip the picture flows like this (**[Standard]** spaces, **[Fork]** order):

```
clip → input transform (F-Log2/F-Gamut → working space)
     → your grade nodes, first to last
     → the clip's effects (Lumetri is one of them)
     → output transform (→ Rec. 709)
```

## Where things are

> id: color.readme.where | does: a map of the colour tools and the page that explains each | when: looking for a colour tool | see: color.readme.layout

**[Fork]**

| Where | What | Page |
|---|---|---|
| Workspace **Color Grading** (Window ▸ Workspaces ▸ Color Grading, `window.workspace.colorgrading`; the **Color** page in the Resolve workflow) | Gallery, viewer, **Nodes**, **Clips** (the full Timeline as a tab beside it), **Primaries** (Lumetri Color as a tab beside it), **Lumetri Scopes** | this page |
| **Clips** panel | pick the clip to grade, mini timeline, split screen | [below](#the-color-page-layout) |
| **Nodes** panel | add, select, bypass nodes | [nodes.md](nodes.md) |
| **Primaries** panel (tabs *Primaries* and *Curves*) | wheels, sliders, curves, film grain | [primaries.md](primaries.md), [curves.md](curves.md) |
| **Gallery** panel | stills, wipe, split | [matching.md](matching.md) |
| Clip ▸ Grade menu | copy, paste, append grades | [matching.md](matching.md) |
| Settings ▸ General ▸ Workflow | Premiere or Resolve layout and keys | [../getting-started.md](../getting-started.md) |

## The Color page layout

> id: color.readme.layout | does: the Color page: gallery, viewer, clips strip, node graph and tool panels | ui: clips.*, grade.* | cmd: window.workspace.colorgrading | resolve: Color page | premiere: Color workspace | when: finding your way around the Color page; seeing the previous and next shots side by side | see: color.readme.first-grade

**[Resolve-style]**

The page is laid out like Resolve's Color page: **Gallery** on the left, the viewer in the
middle, **Nodes** on the right; the **Clips** strip across the whole width under them; the
**Primaries** panel and **Lumetri Scopes** at the bottom.

1. **Pick a clip** in the **Clips** strip {ui:clips.strip}: every video clip of the sequence in
   timeline order, numbered, with its track and start timecode (a dot means it has a grade).
   Clicking a clip selects it for grading and moves the playhead into it. The thin bar under the
   thumbnails {ui:clips.mini} is a mini timeline: click or drag in it to move the playhead.
2. **See the whole timeline** with the **Timeline** tab beside **Clips**.
3. **Resize anything** by dragging the gaps between panels; drag a panel's tab to move it
   elsewhere. **Window ▸ Workspaces ▸ Reset to Saved Layout** puts the page back.
4. **The viewer fits the picture to the panel** (zoom **Fit**) and keeps fitting it when the
   panels are resized. Scrolling the mouse wheel over the picture zooms in; scrolling back out
   stops at Fit. The zoom menu under the picture still offers fixed sizes.
5. **Several clips at once, side by side (split screen):** choose **Adjacent Clips** (the shots
   before and after the one you grade: previous, current, next) or **Selected Clips** (up to 16,
   say 8 interview shots to compare while you match them; select them in the timeline first) in the split-screen menu
   {ui:clips.split} of the Clips panel, or **View ▸ Split Screen**. **Ctrl+Alt+W** turns it on
   and off (Selected Clips when several clips are selected). Each tile shows that clip with its
   grade; the one you are grading is outlined; click a tile to grade that clip. While playing,
   the viewer shows the normal picture.
6. For a fixed reference frame beside the live one, use the **Reference Monitor** tab, a still's
   **Wipe** / **Split** in the Gallery ([matching.md](matching.md)), or **View ▸ Display Mode ▸
   Comparison View**.

The thumbnails in the Clips strip show each clip's source at its middle frame; the split screen
shows the graded frames.

## First grade in six steps

> id: color.readme.first-grade | does: a first grade on a log clip in six steps | ui: grade.addFirst, grade.bypass | cmd: grade.addNode | resolve: first node, primaries | premiere: Lumetri Basic Correction | when: your first grade in Revolution; how undo works while dragging | see: color.primaries.flog2-steps

**[Resolve-style]**

1. Open the workspace **Color Grading** and click a video clip in the **Clips** strip (or the
   timeline).
2. In the **Primaries** panel press **Add Node** {ui:grade.addFirst} (command `grade.addNode`).
3. Set the sequence's colour management and the clip's colour space first if the picture is flat
   ([color-management.md](color-management.md)).
4. Drag the **Gain** wheel's ring up until the brightest part looks right.
5. Raise **Contrast** a little, then **Saturation**.
6. Press **Bypass** {ui:grade.bypass} to compare before and after (graded and ungraded). Press it again to
   restore.

Every step is one undo step (Ctrl+Z); dragging a control counts as one step however long you drag.

## Not here yet

> id: color.readme.not-yet | does: colour tools that are planned but not built yet | when: a Resolve tool you can't find | see: color.readme.where

**[Fork]**

Not built yet: sliders for the mixer nodes' settings (parallel, layer and key mixer use commands
for now, see [nodes](nodes.md)). Each page says what is missing.

Already here (they were listed as coming before): the qualifier (HSL and 3D), windows,
tracking incl. planar tracking, Magic Mask and the colour warper ([secondaries](secondaries.md)),
HDR zone wheels ([primaries](primaries.md)), noise reduction ([noise](noise.md)), LUTs on a node
([finishing](finishing.md)), "same grade?", Shot Match and the node explainer
([matching](matching.md)) and the Grading Coach ([coach](coach.md)).
