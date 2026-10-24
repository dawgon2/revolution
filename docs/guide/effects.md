# Effects, keyframes, transitions and presets

**[Revolution]** · applying and arranging clip effects, animating them with keyframes, a clip's
opacity, blend mode and masks, transitions between clips, and effect presets; modelled on Premiere Pro's Effects panel and
Effect Controls; written for AI-PLAN AI3-PREP (feature pages)

## Applying effects

> id: effects.apply | does: adds a video or audio effect to the selected clips and arranges, switches off or removes it | cmd: effects.list, effects.apply, effects.move, effects.toggleEnabled, effects.remove, effects.reset | resolve: Effects Library ▸ OpenFX / Audio FX, Inspector | premiere: Effects panel, Effect Controls | when: blur, sharpen, distort, keying, audio repair on a clip; changing the order of effects | see: effects.keyframes, effects.presets

**[Revolution]**

1. Find the effect in the Effects panel (Window ▸ Effects; `effects.list` lists them by kind and
   folder) and drag it onto a clip, or select clips and run `effects.apply` with the effect's
   name.
2. Its controls appear in **Effect Controls** (**Keyframes** in the Resolve workflow; the **Inspector**'s Effects tab lists them too) under
   the clip's built-in Motion, Opacity and Time Remapping (video) or Volume and Panner (audio).
3. Effects run top to bottom: drag one up or down to change the order (`effects.move`). The
   switch next to its name bypasses it (`effects.toggleEnabled`); `effects.reset` puts its
   parameters back to their defaults; `effects.remove` deletes it.

Text and shape layers aren't effects you apply: make them with Graphics ▸ New Layer
([graphics.md](graphics.md)). Grading has its own tools: the node grader and Lumetri
([color/README.md](color/README.md)).

## When an effect seems missing (wrong clip, video or audio)

> id: effects.wrong-clip | does: finds an effect you dragged that doesn't show, and the panel that shows it | cmd: effects.list, effects.remove | resolve: Inspector, Keyframes | premiere: Effect Controls | when: you dragged an effect and nothing changed, or Inspector says "No effects applied" | see: effects.apply, audio.voice-isolation

**[Fork]**

- **Where it went:** a clip with effects shows an **fx** badge on the Timeline. If the badge is on
  the clip next to the one you meant, the drop landed there: drop on the middle of the clip, not
  near its edge. Dragging again adds another copy each time; remove the extras on the wrong clip
  (select it, then select the effect in Effect Controls and press Delete, or `effects.remove`).
- **Video or audio:** an audio effect (DeNoise, EQ…) goes on the clip's **audio** part, a video
  effect on its picture. With linked video and audio selected, Effect Controls (**Keyframes** in
  the Resolve workflow) shows the video settings first and the audio ones **below** them: scroll
  down, or click the audio part alone.
- **Inspector:** its **Effects** tab lists the selected clip's *video* effects only; audio effects
  are on its **Audio** tab. "No video effects applied" with an audio effect on the clip is expected.
- **Effect Controls isn't on the right-click menu as a setting:** it is a panel. Window ▸ Effect
  Controls (Shift+5; called **Keyframes** in the Resolve workflow), or right-click a clip ▸
  **Effect Controls** to open it for that clip.

## Changing values and keyframes

> id: effects.keyframes | does: sets effect parameters and animates them over time with keyframes and their interpolation | cmd: effects.setParam, effects.resetParam, effects.toggleAnimation, effects.addKeyframe, effects.setKeyframe, effects.moveKeyframe, effects.deleteKeyframe, effects.setInterpolation | resolve: Inspector keyframes, Keyframe editor | premiere: Effect Controls keyframes | when: a move, a fade, a blur that grows; anything that changes slowly over a few seconds (a colour draining, a push-in, a sound fading) | see: effects.apply

**[Revolution]**

1. Drag a value or type it (`effects.setParam`); the reset arrow puts it back
   (`effects.resetParam`).
2. Click the stopwatch to animate the parameter (`effects.toggleAnimation`): a keyframe is added
   at the playhead, and from now on every change at another time adds or changes a keyframe there
   (`effects.addKeyframe`, `effects.setKeyframe`).
3. Move keyframes in time (`effects.moveKeyframe`) or delete them (`effects.deleteKeyframe`);
   clicking the stopwatch again removes all of them and keeps the value at the playhead.
4. Right-click a keyframe for its interpolation (`effects.setInterpolation`): **Linear**,
   **Bezier**, **Auto Bezier**, **Continuous Bezier**, **Hold**, **Ease In**, **Ease Out**.

| Interpolation | Feels like | Use it for |
|---|---|---|
| **Linear** | Mechanical, constant speed | Fades, wipes, anything that should feel exact |
| **Ease In / Ease Out**, **Bezier** | Natural: speeds up and slows down | Moves, zooms, titles flying in |
| **Hold** | A jump, no motion between keyframes | Steps, cuts inside an effect, flashing |

## Opacity, blend modes and masks

> id: effects.opacity | does: makes a clip see-through, mixes it with the tracks below by a blend mode, and limits it to a drawn mask | cmd: effects.setParam, masks.add, masks.set, masks.track | resolve: Inspector ▸ Composite (Composite Mode, Opacity) and Color page windows | premiere: Effect Controls ▸ Opacity (Blend Mode, masks) | when: a ghost or double exposure, a fade to the track below, a light leak or texture laid over a shot, showing only part of a clip | see: effects.keyframes, tracking.what

**[Revolution]**

Every video clip has a built-in **Opacity** effect in Effect Controls (**Keyframes** in the
Resolve workflow; the **Inspector**'s Composite section shows the same settings). It decides how the clip mixes with the tracks under it.

1. Select the clip on the upper track. Under **Opacity**, lower **Opacity** (0–100 %) to let the
   track below show through (`effects.setParam` with effect `opacity`, param `opacity`).
2. Choose a **Blend Mode** (param `blend`): **Normal** is a plain mix; **Screen** and
   **Linear Dodge (Add)** only brighten (light leaks, glows, a ghostly figure); **Multiply** only
   darkens (textures, shadows); **Overlay** and **Soft Light** add contrast. There are 27 modes,
   the same list as Premiere's.
3. To keep only part of the clip, draw a mask on Opacity (the ellipse, rectangle or pen buttons;
   `masks.add`): outside the mask the clip is transparent. **Mask Feather** softens the edge,
   **Mask Expansion** grows or shrinks it, **Inverted** keeps the outside instead (`masks.set`).
4. If the part moves, track the mask (Mask Path's track buttons, `masks.track`;
   [tracking.md](tracking.md)).
5. Animate any of these with keyframes ([Changing values and keyframes](#changing-values-and-keyframes)),
   e.g. Opacity from 100 % to 0 % for a fade to the track below.

| You want | Try | Trade-off |
|---|---|---|
| Two pictures mixed evenly | **Normal** at 30–60 % | Both look washed out; dark parts of the top clip show |
| Something bright and see-through (ghost, light leak, fire) | **Screen** or **Linear Dodge (Add)** | Black disappears for free, but the top clip can never darken the picture |
| A texture, dirt or shadow | **Multiply** | White disappears, but the picture can only get darker |

Blend modes other than Normal work on display-encoded values, as in other editors; Normal mixes
in linear light. For more than two layers, or a mix you want to see as a graph, use a comp's
**Merge** node instead (opacity and blend mode per input; [compositor.md](compositor.md)).
Differs from Resolve: Resolve calls the blend mode **Composite Mode** and has fewer of them;
masks there live on the Color page (windows) or in Fusion. Revolution can't remove an object and
fill in behind it; the closest workaround is a clean plate (a frame without the object, from a
locked-off shot) on the track below, shown through a mask.

## Stylised effects: glows, light leaks, warps, shake and time

> id: effects.stylize | does: built-in effects for glows, light leaks, warps, shake, reframing, stutter, trails and one colour | cmd: effects.apply, effects.setParam | resolve: OpenFX Glow / Light Rays, Fusion warps | premiere: the same effect names in Video Effects | when: a dreamy glow, a light leak, a heat-haze or underwater wobble, a choppy low frame rate, motion trails, one colour in a black-and-white shot | see: effects.apply, shader-effects.use

**[Revolution]**

Apply them like any effect (`effects.apply`; search the name in the Effects panel) and set them
in Effect Controls.

| Effect (Video Effects ▸ …) | Does | Good for |
|---|---|---|
| **Lights & Glows ▸ Wonder Glow** | blooms everything brighter than **Threshold**, with Radius, Intensity and an optional Glow Color | dreamy looks, glowing lamps, a soft halo |
| **Lights & Glows ▸ Edge Glow**, **Echo Glow** | glows along edges, or in repeated rings | stylised titles and graphics |
| **Lights & Glows ▸ Light Leaks** | warm leaks of light drifting over the picture | film / retro looks, transitions by hand |
| **Distort ▸ Turbulent Displace** | warps the picture with moving noise (Turbulent / Smoother modes) | heat haze, underwater, a dream wobble |
| **Transform ▸ Camera Shake** | smooth random position, rotation and zoom moves (Amount, Frequency, Complexity, Motion Blur) | an earthquake, a hit, a handheld feel |
| **Transform ▸ Auto Reframe** | follows the main subject and zooms to a Target Aspect (9:16, 1:1, 4:5) | vertical and square versions of a wide clip |
| **Time ▸ Posterize Time** | plays the clip at a lower **Frame Rate** inside the sequence | CCTV / stop-motion choppiness |
| **Obsolete ▸ Echo** | mixes in earlier frames (Echo Time, number of echoes) | motion trails, a ghostly smear |
| **Obsolete ▸ Leave Color** | removes colour except the **Color To Leave** (Tolerance, Edge Softness) | the one-red-coat look |

The **Obsolete** folder holds effects Premiere no longer lists; Revolution keeps them working.
For one colour kept in a graded shot, a qualifier on a node with Saturation down outside the key
gives more control ([color/secondaries.md](color/secondaries.md)). The bundled shader effects
(VHS, Glitch, Chromatic Aberration, Light Leak) are in Video Effects ▸ User
([shader-effects.md](shader-effects.md)).

## Lens and light effects: flares, rays, bokeh and focus

> id: effects.lens-light | does: built-in lens flares, light rays, bokeh and focus blur, lens distortion, a magnifier and a mirror | cmd: effects.apply, effects.setParam | resolve: OpenFX Lens Flare, Light Rays, Lens Blur, Tilt-Shift Blur | premiere: the same effect names | when: a sun flare, god rays, a miniature (tilt-shift) look, a fisheye bend, a magnifying glass, a mirrored half | see: effects.stylize, effects.apply

**[Revolution]**

| Effect (Video Effects ▸ …) | Main controls | Good for |
|---|---|---|
| **Lights & Glows ▸ Lens Flare** | Flare Center, Flare Brightness, Lens Type, Flare Color, Size, Ghosts | a sun or lamp flare; keyframe Flare Center to follow the light |
| **Lights & Glows ▸ Volumetric Rays** | Source Point, Threshold, Ray Length, Intensity, Ray Color | god rays through windows, trees or dust |
| **Blur & Sharpen ▸ Bokeh Blur** | Blur Amount, Iris Shape, Highlight Brightness / Threshold | soft background lights turning into discs |
| **Blur & Sharpen ▸ Focus Blur** | Focus Shape, Focus Point, Angle, Focus Size, Feather, Blur Amount | a tilt-shift miniature look, or faking shallow focus |
| **Distort ▸ Lens Distortion** | Curvature, Decentering | a fisheye / action-cam bend, or straightening one |
| **Distort ▸ Magnify** | Shape, Center, Magnification, Size, Feather | a magnifying glass or zoomed detail |
| **Distort ▸ Mirror** | Reflection Center, Reflection Angle | a mirrored half, a symmetric frame |

## Moving, framing and outlines: 3D Rotate, Wiggle, Track Matte, Stroke

> id: effects.shape-move | does: tilts a layer in 3-D, wiggles it, flips it, cuts it through another layer, outlines, shadows and rounds its frame | cmd: effects.apply, effects.setParam | resolve: Inspector 3D transform, Fusion, Track Matte in Composite | premiere: the same effect names | when: a flat picture tilted in perspective, straightening a crooked horizon, an old TV switch-off squeeze, a jittery wiggle, text filled with video, a cut-out outline, a rounded picture-in-picture | see: effects.opacity, practice.editing.split-screen

**[Revolution]**

| Effect (Video Effects ▸ …) | Main controls | Good for |
|---|---|---|
| **Transform ▸ 3D Rotate** | X / Y / Z Rotation, Perspective, Z Position, Hide Back Face | a flat card tilted or spun in perspective (a phone screen, a photo flying in) |
| **Transform ▸ Wiggle** | Frequency, Position / Rotation / Scale Amount, Random Seed | a nervous hand-held jitter or a playful wobble on a graphic |
| **Transform ▸ Horizontal Flip / Vertical Flip** | none | mirroring a shot so a look or move faces the other way |
| **Keying ▸ Track Matte Key** | Matte (the track to cut with), Composite Using (alpha or luma), Reverse | video inside text, a picture shown only through a shape |
| **Utility ▸ Stroke** | Color, Width, Position, Stroke Only | an outline around a cut-out person or a logo |
| **Perspective ▸ Drop Shadow** | Shadow Color, Opacity, Direction, Distance, Softness | lifting a picture-in-picture or a title off the background |
| **Image Control ▸ Rounded Crop** | Left / Top / Right / Bottom, Roundness, Border Width / Color | rounded picture-in-picture windows with a border |

With **Track Matte Key**, put the matte (the text or shape) on its own track above the video,
apply the effect to the video clip and pick that track as **Matte**. Then hide the matte so it
doesn't show on top:
1. Turn off the matte **track's** output (the track's eye toggle; command `timeline.setTrack`
   with `enabled: false`). The matte still cuts the video.
2. Don't disable the matte **clip** instead (`clip.enable`): a disabled clip is no longer read as
   a matte, and the video goes black.
Premiere works the same way (eye toggle on the matte track); Resolve's track mattes work
differently, so its steps don't carry over.

**Motion** is built into Effect Controls (no need to apply it): **Position**,
**Scale**, **Rotation** (degrees), **Anchor Point** and **Uniform Scale**. Two everyday uses:
1. Straighten a crooked horizon: set **Rotation** a degree or two against the tilt, then raise
   **Scale** until the corners that rotation uncovered are filled (watch the frame edges).
2. Squeeze or stretch one way only (an old TV switching off, a squashed-to-a-line exit): turn
   **Uniform Scale** off; **Scale** then sets the height and **Scale Width** the width.
   Keyframe **Scale** down to almost 0 over a few frames for the switch-off. Premiere calls these
   Scale Height / Scale Width; Resolve has Zoom X / Zoom Y in the Inspector.
3. Spin, swing or squash about a chosen point (a logo spinning on its own centre, a sign
   swinging from a hinge at its top edge, a bar growing up from the floor): **Anchor Point** is
   the pivot that Scale and Rotation work around, in the clip's own pixels; it starts at the
   centre, so a plain Rotation spins on the centre. To pivot somewhere else, move **Anchor
   Point** to that spot (for a 1920×1080 graphic, 960, 0 is the middle of the top edge), then
   move **Position** by the same amount (−540 in Y here) so the picture doesn't jump; now
   keyframe Rotation or Scale. Premiere works the same way; Resolve calls it Anchor Point in the
   Inspector too.

3D Rotate tilts a flat layer; Revolution has no 3-D scenes, cameras or extruded text (the
Compositor is 2-D too).

## Painterly and graphic looks

> id: effects.painterly | does: turns a shot into brush strokes, outlines or flat colour bands | cmd: effects.apply, effects.setParam | resolve: OpenFX stylize effects | premiere: Brush Strokes, Find Edges, Posterize | when: a painted or sketched look, a comic-book style, a cartoon with flat colours | see: effects.stylize

**[Revolution]**

- **Stylize ▸ Brush Strokes** (Stroke Angle, Brush Size, Length, Density, Randomness, Blend With
  Original): a painted look; blend a little of the original back for detail.
- **Stylize ▸ Find Edges** (Invert, Blend With Original): outlines only, like a sketch; on a copy
  of the clip above the original with the **Multiply** blend mode it draws ink lines on the shot.
- **Stylize ▸ Posterize** (Level): flat colour bands; a few levels plus Find Edges on top gives a
  comic-book style.

## Transitions

> id: effects.transitions | does: puts a dissolve, wipe or audio crossfade on an edit point and sets the default one | cmd: sequence.applyVideoTransition, sequence.applyAudioTransition, sequence.applyDefaultTransitionsToSelection, sequence.setTransition, sequence.removeTransition, effects.setDefaultTransition | resolve: Video and Audio Transitions | premiere: Video Transitions, Audio Transitions, Apply Default Transitions | when: softening a cut, a passage of time, smoothing an audio edit; fading to black or white at the end, a flash between clips | see: effects.apply

**[Revolution]**

1. Park the playhead on an edit (or select clips) and apply the default video transition
   (`sequence.applyVideoTransition`) or audio crossfade (`sequence.applyAudioTransition`);
   **Apply Default Transitions to Selection** (`sequence.applyDefaultTransitionsToSelection`)
   does every edit of the selected clips at once.
2. Or drag a transition from Effects ▸ Video / Audio Transitions onto the edit.
3. Select it to change its length and alignment (`sequence.setTransition`) or delete it
   (`sequence.removeTransition`). Which transition is the default is set with
   `effects.setDefaultTransition`; the default lengths are preferences
   (`timeline.videoTransitionDuration`, `timeline.audioTransitionDuration`).

Video Transitions has ten folders. **Dissolve** ▸ **Dip to Black** / **Dip to White** fade out
and in through a colour. Besides the
dissolves and wipes, a few that creative edits reach for: **Slide** ▸ **Whip** (a whip pan with blur), **Push** and **Slide**; **Grunge &
Distort** ▸ **Earthquake** (a shake across the cut), **Glitch** and **VHS Damage**; **Lights &
Blurs** ▸ **Cross Zoom** and **Light Leak**; and the ones that move the picture like a card:
**Transformers** ▸ **Page Peel**, **3D Spin** / **3D Spinback** and **Louver**, **Slide** ▸
**3D Roll** and **Film Roll**, **Animation** ▸ **Flip Motion** and **Spin Motion**. Each has its own settings (direction, blur, amount,
colour) in Effect Controls once it is on the edit.

A transition needs media beyond the edit (handles) on both clips; with too little, it gets
shorter. A **Dip to Black / White** on a sequence's first or last edit (nothing beyond it)
holds the colour for the first half of its length and fades during the second: a 2-second dip
at the start is black for 1 second, then fades up. For a fade that runs the whole length, keyframe
**Opacity** from 0 to 100 % instead (`effects.keyframes`).

Audio crossfades: **Constant Power** keeps the loudness steady across the edit and is
the usual choice; **Constant Gain** is a straight linear fade, which can sound like a small dip in
the middle; **Exponential Fade** suits fades to and from silence.

## Effect presets

> id: effects.presets | does: saves configured effects with their keyframes and masks under a name and applies them to other clips | cmd: presets.list, presets.save, presets.apply, presets.rename, presets.delete, presets.export, presets.import | resolve: Effects favourites, PowerGrades (for grades) | premiere: Effects ▸ Presets bin | when: the same blur, zoom or audio chain on many clips | see: packs.what, effects.keyframes

**[Revolution]**

1. Select a clip, choose its effects in Effect Controls and **Save Preset…** (`presets.save`)
   with a name and how to treat keyframes:
   - **Scale**: keyframes stretch over the length of the clip they are applied to;
   - **Anchor to In Point** / **Anchor to Out Point**: they keep their distance from the
     clip's start or end (an intro or outro animation);
   - **none**: only the values at the playhead are saved.
2. Apply it from the Presets bin (Effects ▸ Presets) or with `presets.apply`: positions and mask
   shapes are scaled to the clip's frame size, so a preset made on 4K lands in the same place on
   HD. Motion, Opacity and Time Remapping in a preset replace the clip's own.
3. Rename, delete, export or import presets (`presets.rename`, `presets.delete`,
   `presets.export`, `presets.import`); to share presets with LUTs and graphics templates, use a
   look pack ([packs.md](packs.md)).
