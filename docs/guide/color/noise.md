# Noise reduction

**[Resolve-style]** · spatial noise reduction on a node (Phase G2.4) and temporal noise
reduction on the clip (fork Phases 6.7 and 6.7b)

## Why

> id: color.noise.why | does: why log footage gets noisy when shadows are lifted, and where to fix it | resolve: Spatial / Temporal NR | premiere: Median / Noise reduction effects | when: grainy shadows after a grade | see: color.noise.steps

**[Resolve-style]**

Log footage such as F-Log2 is noisy in the shadows, and lifting the shadows in a grade makes the
noise more visible. Noise reduction smooths small random speckle while keeping edges sharp, so do
it *before* the correction that lifts the shadows.

There are two kinds. **Spatial** noise reduction looks at one frame: it averages each pixel with
similar pixels around it, which also softens fine detail. **Temporal** noise reduction looks at
the frames before and after: noise changes from frame to frame while the picture doesn't, so
averaging the same point over several frames removes noise and keeps detail. It is the better
tool for static or slowly moving shots; spatial is the fallback where things move fast
(a handheld running shot, lots of motion).

## Steps

> id: color.noise.steps | does: spatial noise reduction on a node at the start of the chain | ui: grade.param.nr.*, grade.tab.noise | cmd: grade.insertNode, grade.setParam | resolve: Spatial NR | premiere: — (third-party plug-ins) | when: noise in one shot | see: color.noise.temporal

**[Resolve-style]**

1. Add a node at the **start** of the chain (`grade.insertNode`, **Insert Node Before** on the
   first node): noise reduction works best on the picture before contrast and saturation,
   so it goes before the grade rather than after it.
2. Open the **Noise** tab {ui:grade.tab.noise}.
3. Raise **NR luma** {ui:grade.param.nr.luma} for grainy brightness noise and **NR chroma**
   {ui:grade.param.nr.chroma} for coloured blotches. Start around 0.3 and compare with the node
   switched off (`grade.toggleNode`).
4. **NR radius** {ui:grade.param.nr.size} sets how far each pixel looks for similar neighbours
   (0.25 … 3.5 source pixels): larger is smoother and slower.
5. To clean only part of the picture (say only the dark background, without softening the face), add a window to the
   same node
   ([secondaries.md](secondaries.md)): the noise reduction stays inside it. (A qualifier on the
   node does *not* limit the noise reduction, see Limits.)

## Temporal noise reduction

> id: color.noise.temporal | does: cleans noise using neighbouring frames, set per clip | ui: tnr.* | cmd: tnr.set, tnr.toggle, tnr.copy, tnr.paste, tnr.reset | resolve: Temporal NR | premiere: — | when: heavy noise on static or slow shots | see: color.noise.how

**[Fork]**

Temporal noise reduction is a setting of the **clip**, not of a grade node: it cleans the
clip's picture right after the input colour transform, before every node and effect. That way
it always looks at the plain camera frames on either side, and changing your grade never
changes what it does. (Until schema v17 it lived on the first grade node; projects saved then
move it to the clip when they open, and a message says how many clips moved.)

It is too heavy to run while playing, so it renders only where there is time for it: at
**export**, with **Export Frame**, in the **render cache** ([render-cache.md](../render-cache.md))
and on the **paused** Program monitor frame. While playing or scrubbing the monitor shows the
clip without it (from the render cache once that part is rendered).

1. Select the clip on the timeline (temporal NR is per clip and uses the neighbouring frames).
2. Find the **Temporal NR** controls. They are the same controls in two places, so use
   whichever is open:
   - **Effect Controls** (Premiere workflow; called **Keyframes** in the Resolve workflow): the
     **Temporal NR** section. In the Resolve workflow the **Inspector**'s Video tab has the same
     **Temporal NR** section (pages.inspector).
   - The Color page **Noise** tab {ui:grade.tab.noise}: the **Temporal NR (clip)** block under
     the node's spatial rows. It is headed with the clip's name because it belongs to the clip,
     whichever node is selected.
3. Make sure **Enabled** {ui:tnr.enabled} is on.
4. Raise **Luma** {ui:tnr.luma} (brightness noise) and **Chroma**
   {ui:tnr.chroma} (colour noise) in that section. 0.5 is a good start; 1.0 is strong.
5. **Frames** {ui:tnr.frames} is how many frames on each side are averaged: 1
   (three frames in all) or 2 (five frames, cleaner and slower).
6. **Motion** {ui:tnr.motion} (0 … 1, default 0.5) is how much disagreement in the
   motion estimate is accepted. Lower it if moving edges leave a faint ghost; raise it for more
   cleaning on difficult but slow motion.
7. Pause the Program monitor on the shot and wait a moment: the paused frame renders with
   temporal noise reduction (the frame without it shows meanwhile). Each control has a reset
   button ({ui:tnr.luma.reset} and the like) and can be keyframed. In Effect Controls
   (Keyframes in the Resolve workflow) the keyframes show as diamonds in the keyframe area on the control's row: click
   one to move the playhead there, drag one to move it in time (it snaps to a frame; one undo
   step, `tnr.moveKeyframe`). Change a keyframe's value at the playhead with the row's controls.
8. The render bar marks the clip for rendering; let the render cache build it, or export.

To use the same settings on other clips: **Clip ▸ Temporal NR ▸ Copy Temporal NR** (`tnr.copy`),
select the other clips, then **Paste Temporal NR** (`tnr.paste`). **Paste Attributes**
(`edit.pasteAttributes`) also copies it (its `temporalNr` parameter, on by default). Copying or pasting a *grade* doesn't carry
temporal NR any more, because it isn't part of the grade.

Spatial and temporal noise reduction combine: the clip's temporal NR runs first, then each
node's spatial NR, then that node's correction. With temporal NR on, keep spatial NR low (0.1–0.2) or off.

## How it works

> id: color.noise.how | does: how the spatial (bilateral) and temporal filters decide what is noise | resolve: Spatial / Temporal NR internals | when: understanding artefacts and settings | see: color.noise.limits

**[Standard]** **[Fork]**

Spatial (set on a grade node): an edge-preserving *bilateral* filter: each pixel is averaged with its neighbours,
weighted by distance and by how similar they are, so a real edge (a big difference) is left alone
while small random differences are averaged out. Brightness (luma) and colour (two colour
differences) are filtered separately with their own strengths. The filter size follows the
picture size, so a reduced-resolution preview looks like the full-size export.

Temporal (own code from published methods): the motion between the frame and each neighbour is
estimated with dense pyramidal Lucas–Kanade optical flow (Lucas & Kanade 1981; Bouguet 2001) on a
small brightness copy (at most 640 pixels wide), both ways. Each neighbour is warped onto the
frame along that motion and blended in with a weight that falls as it differs from the frame — a
motion-compensated temporal filter (Dubois & Sabri 1984) with per-pixel range weights, i.e. a
temporal bilateral filter (Bennett & McMillan 2005). Where the forward and backward motion
disagree (occlusions, fast motion) the neighbour counts less (forward–backward consistency,
Kalal et al. 2010). The neighbours are the clip's own frames: never across a cut, following its
speed, reverse and holds (a hold has no other frame to average, so nothing changes there).
Motion between neighbouring frames is computed once and reused.

## Limits

> id: color.noise.limits | does: where temporal NR renders and what it costs | ui: program.resolution | when: temporal NR doesn't show while playing | see: color.noise.temporal

**[Fork]**

- Temporal noise reduction renders only at export, in the render cache and on the paused Program
  frame (and Export Frame from the Program monitor), never while playing or scrubbing. A frame
  exported from the Source monitor has no temporal NR (it isn't a clip on a timeline).
- The paused frame matches the export exactly only at playback resolution **Full**
  {ui:program.resolution}: at 1/2 or 1/4 the motion and the blend work on the smaller picture,
  so the cleaning is close but not identical.
- On a retimed clip with **Frame Blending** or **Optical Flow**, frames made between two source
  frames get no temporal NR, and such frames are not used as neighbours (the motion is measured
  between real source frames). Frame Sampling (the default) is not affected.
- During a transition the clips keep their temporal NR: the frames shown from their handles have
  neighbours within the transition.
- Temporal noise reduction applies to the whole picture of the clip: windows and qualifiers
  can't limit it (they belong to grade nodes). Bypassing the grade doesn't switch it off; use its
  **Enabled** switch.
- It works on media clips (and subclips); on nested sequences, graphics and adjustment layers it
  does nothing.
- It is slow: at 6.2K expect an export several times slower with it on (motion estimation on the
  CPU, the blend on the GPU). Use 1 frame each side unless 2 is visibly cleaner.
- Spatial: at full strength, faint colour edges soften (a property of this kind of filter); keep
  NR chroma moderate.
- Spatial: the node's qualifier doesn't limit the noise reduction (it keys the cleaned picture);
  windows do.
- Spatial costs GPU time: on the RX 6700 XT about 3–20 ms per 1080p frame depending on the
  radius, much more on the Intel GPU. Use lower playback resolution while grading.

## If you know Resolve

> id: color.noise.resolve | does: how this compares with Resolve's NR palette | resolve: Spatial / Temporal NR | when: you come from Resolve | see: color.noise.steps

**[Resolve-style]**

Resolve's Spatial NR palette has more modes (faster / better, mode per channel, blend); here it
is one filter with two strengths and a radius. Resolve's Temporal NR sits on a node (its Motion
Effects palette), so it can be limited by that node's window or qualifier; here it is a setting
of the clip, before the grade (owner decision, question Q9), so it never re-reads graded frames
and a window can't limit it. Resolve's also has more controls (motion estimation type, motion range, frame count up to 5, thresholds per channel,
blend); here it has two strengths, 1 or 2 frames each side and one motion tolerance, and it runs
only at export, in the render cache and on the paused frame (Resolve renders it live when the
system keeps up). Parameters for scripts: `nr.luma`, `nr.chroma` (0…1), `nr.size` (σ in source
pixels), set with `grade.setParam`; temporal: `luma`, `chroma` (0…1), `frames` (1 or 2), `motion`
(0…1), set with `tnr.set` (for example `{"param": "luma", "value": 0.5}`), plus `tnr.toggle`,
`tnr.reset`, `tnr.toggleAnimation`, `tnr.moveKeyframe`, `tnr.get`. `grade.setParam` refuses the old `tnr.*` keys.
