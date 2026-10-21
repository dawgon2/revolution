# Practice notes: looks

**[Standard]** · common creative looks and how they are usually built, one per note, with the
Resolve / Premiere approach and the Revolution nodes. Written in our own words from common practice
(AI-PLAN AI3 item 0a, 0c). Build every look on a balanced shot, on its own node
([grading.md](grading.md) ▸ look node), and mix it to taste.

## Black and white with control

> id: practice.looks.black-white | does: turns colour into black and white while choosing how each colour becomes grey | resolve: Saturation 0 after an RGB Mixer / channel weighting | premiere: Lumetri Saturation 0, Black & White effect | when: drama, timelessness, mixed light that won't balance | see: practice.looks.high-contrast-bw

**[Standard]**

- **Resolve:** weight the channels (more red lightens skin and darkens blue sky) before taking
  saturation to zero.
- **Revolution:** on the look node, shift colours with the wheels or a qualifier first (blue darker,
  skin lighter), then Saturation to 0 on a later node.
- **Pitfalls:** plain desaturation makes red and green the same grey; skies go flat.

## High-contrast black and white

> id: practice.looks.high-contrast-bw | does: a punchy monochrome with deep blacks and bright highlights | resolve: B&W + S-curve | premiere: Lumetri B&W + Contrast | when: street, music, stylised portraits | see: practice.looks.black-white, practice.grading.contrast-pivot

**[Standard]**

- **Revolution:** black and white as above, then Contrast up with the pivot on the subject and a
  little grain.
- **Pitfalls:** crushed blacks lose hair and clothing detail.

## Bleach bypass

> id: practice.looks.bleach-bypass | does: a gritty, low-saturation, high-contrast look like skipping film's bleach step | resolve: blend a desaturated, contrasty copy (layer mixer, overlay) | premiere: duplicate layer, desaturate, overlay blend | when: war, thrillers, harsh drama | see: practice.node-trees.layer

**[Standard]**

- **Resolve:** a layer mixer blending the shot with a black-and-white, high-contrast version.
- **Revolution:** **Add Layer Node** (`grade.addLayer`) and make the layer node low in
  saturation and high in contrast, then set its strength with `layer.opacity.1`
  ([color/nodes.md](../color/nodes.md) ▸ Layer nodes for looks); or on the look node: Saturation
  about 40–60 %, Contrast up, slight highlight shoulder; or a desaturated duplicate on V2 with the
  **Overlay** blend mode.
- **Pitfalls:** skin goes grey; protect it a little with a skin qualifier.

## Faded, lifted blacks (vintage)

> id: practice.looks.faded-blacks | does: lifts the bottom of the curve so blacks turn soft grey, like old prints | resolve: custom curve, Lift | premiere: Lumetri Faded Film, RGB curve | when: nostalgic, lifestyle, music videos | see: practice.grading.curve-shoulder

**[Standard]**

- **Revolution:** raise the bottom point of the master curve (`grade.curve.master`), maybe tint the
  lifted blacks slightly warm or green with the Lift wheel; grain on top.
- **Pitfalls:** banding in dark areas on 8-bit delivery; deliver 10-bit or add grain.

## Warm golden hour, made stronger

> id: practice.looks.golden-hour | does: enhances warm, low sun light: warm highlights, deeper contrast, gentle glow | resolve: Gain warm, Temperature, soft highlight roll-off | premiere: Lumetri Temperature, Highlight Tint | when: sunsets, lifestyle, weddings | see: practice.grading.split-tone

**[Standard]**

- **Revolution:** Gain wheel towards orange, Temperature a little warmer, a soft highlight shoulder;
  keep shadows neutral or slightly cool for depth.
- **Pitfalls:** orange skin; check the skin line on the vectorscope.

## Cool, desaturated thriller

> id: practice.looks.cool-thriller | does: a cold, muted look with green-blue shadows and restrained skin | resolve: cool Offset / Lift, saturation down, skin protected | premiere: Lumetri Temperature cool, Saturation down | when: crime, tension, corporate dystopia | see: practice.grading.qualify-skin

**[Standard]**

- **Revolution:** Temperature cool, Lift towards teal-green, Saturation 70–80 %, a skin qualifier to
  keep faces from turning blue.
- **Pitfalls:** dead, blue skin; viewers read it as illness.

## Clean commercial look

> id: practice.looks.commercial | does: bright, clean, neutral picture with true colours and pure whites | resolve: careful balance, gentle S-curve, vibrance | premiere: Lumetri Basic + Vibrance | when: products, corporate, tutorials | see: practice.grading.balance-first

**[Standard]**

- **Revolution:** precise balance on the first node, white point near the top, a gentle contrast
  curve, Saturation slightly up.
- **Pitfalls:** clipped whites on product edges; check the waveform.

## Pastel / soft look

> id: practice.looks.pastel | does: low contrast with light, soft colours and lifted shadows | resolve: low contrast, lifted Lift, saturation down, warm-pink gain | premiere: Lumetri Contrast down, Faded Film | when: fashion, lifestyle, gentle stories | see: practice.looks.faded-blacks

**[Standard]**

- **Revolution:** Contrast down, blacks lifted, Saturation a little down, Gain slightly warm or pink.
- **Pitfalls:** flat and muddy rather than soft: keep a clean white point.

## Neon night (cyberpunk)

> id: practice.looks.neon | does: deep blacks with saturated magenta and cyan lights | resolve: qualifiers on lights, Lift to cyan-blue, Gain magenta | premiere: HSL Secondary, split toning | when: night city, music videos | see: practice.node-trees.gamut-compress

**[Standard]**

- **Revolution:** shadows towards blue-cyan, lights pushed towards magenta and cyan with qualifiers,
  deep blacks; watch out-of-gamut colours on Rec. 709.
- **Pitfalls:** clipped, flat neon: saturate less and let roll-off do its job.

## Natural documentary look

> id: practice.looks.documentary | does: an honest, slightly contrasty, true-to-life picture that never draws attention | resolve: balance + mild contrast | premiere: Lumetri Basic | when: documentaries, interviews | see: practice.grading.balance-first

**[Standard]**

- **Revolution:** the balance and contrast nodes only, consistent across interviews; skin on the
  line; no look node, or a very light one.
- **Pitfalls:** the biggest trap with many interviews: every interview balanced differently; match them ([grading.md](grading.md)).

## Film-like contrast without a LUT

> id: practice.looks.film-curve | does: an S-curve with a soft toe and shoulder, slightly warm highlights and cool shadows, plus grain | resolve: custom curves, film-emulation LUT | premiere: Lumetri Creative looks, curves | when: "make it look like film" | see: practice.grading.curve-shoulder, practice.grading.grain-finish

**[Standard]**

- **Revolution:** master curve with gentle toe and shoulder, Lift slightly cool, Gain slightly warm,
  Saturation a touch lower, grain on the last node; a film-emulation LUT on a node if you have one
  you're licensed to use.
- **Pitfalls:** the S-curve alone isn't film: the roll-off and texture matter as much.

## Using a creative LUT well

> id: practice.looks.creative-lut | does: applies a creative LUT after balancing, at partial strength, and corrects around it | resolve: LUT on a late node with key output gain | premiere: Lumetri Creative ▸ Look with Intensity | when: a downloaded or bought look | see: color.finishing.lut, packs.make

**[Standard]**

- **Revolution:** a LUT on a late node (`grade.setLut`) with its mix (`grade.param.lut.mix`) around
  50–80 %; balance before it, not after. Import folders of LUTs with `lut.importFolder`.
- **Pitfalls:** a LUT made for one camera's log on another camera; LUTs on unbalanced shots.

## Day exterior: bluer sky, warmer land

> id: practice.looks.exterior | does: separates sky and land with a gradient: cooler, deeper sky, warmer ground | resolve: linear window, two nodes | premiere: graduated masks in Lumetri | when: landscapes, travel | see: practice.grading.sky

**[Standard]**

- **Revolution:** a linear window on the sky (Gain down, a little blue), its inverse on the ground
  (a touch warmer).
- **Pitfalls:** the gradient crossing a mountain or building edge; soften and angle it.

## Low-key night interior

> id: practice.looks.low-key | does: a dark scene with pools of light, deep shadows and protected faces | resolve: Gamma down, windows on faces | premiere: Lumetri Exposure / Shadows, masks | when: drama, night interiors | see: practice.grading.day-for-night

**[Standard]**

- **Revolution:** Gamma and Lift down on the contrast node, ellipse windows keep faces and lamps
  readable, slight cool shadows.
- **Pitfalls:** lifting the whole picture back up to "see everything" kills the mood; noise in
  crushed shadows of log footage.

## Matching a reference look

> id: practice.looks.reference | does: builds a look by comparing against a reference frame from another film or photo | resolve: import a still into the Gallery, wipe | premiere: Comparison View with a reference frame | when: a client sends a reference | see: color.matching.stills, practice.grading.match-order

**[Standard]**

- **Revolution:** import the reference image into the project, put it on a timeline, grab it as a
  still (`stills.grab`) and wipe against your shot; match levels first, then colour, then
  contrast shape.
- **Pitfalls:** chasing a look the footage can't support (different light, lens or exposure).

## Double exposure (two shots in one)

> id: practice.looks.double-exposure | does: lays one shot through another, like two pictures on one piece of film: a face filled with a landscape | resolve: second clip on V2 with Composite Mode Screen or Lighten, a window, Fusion for more | premiere: second clip on V2, Opacity Blend Mode Screen / Lighten, an Opacity mask | when: memories, dreams, music videos, title sequences | see: effects.opacity, practice.editing.clean-plate

**[Standard]**

- **Revolution:** put the base shot (usually a face or silhouette against a bright sky) on V1 and
  the texture shot (trees, city lights, clouds) on V2. On the V2 clip's **Opacity** set
  **Blend Mode** to **Screen** (or **Lighten**) and **Opacity** around 50–80 %
  (`effects.setParam`, effect `opacity`); [effects.md](../effects.md) ▸ Opacity has the table of
  modes. To keep the texture inside the figure, draw a mask on Opacity, feather it, and track it
  if the person moves (`masks.add`, `masks.track`).
- **Options:** **Screen** keeps the bright parts of both (the classic film look, needs a dark
  texture); **Multiply** keeps the dark parts (a silhouette filled with texture, needs a bright
  background); a comp in the Compositor when you want more than two layers or to see the mix as
  a node graph ([compositor.md](../compositor.md)).
- **Pitfalls:** grade both shots first (contrast decides what shows through); a busy texture
  hides the face, so pick one with large calm areas; Screen can never darken, so a bright texture
  washes everything out.

## VHS, camcorder or security-camera look

> id: practice.looks.vhs | does: makes clean footage look like an old tape, a home camcorder or a CCTV camera: soft, noisy, washed out, with a timestamp | resolve: Fusion / OFX effects (e.g. analog damage), blur, grain, text | premiere: VHS presets, Noise, Gaussian Blur, text layer | when: flashbacks, found footage, a fake security recording, retro music videos | see: shader-effects.use, color.finishing.grain, graphics.text

**[Standard]**

- **VHS / camcorder:** apply the bundled **VHS** shader effect (Effects ▸ Video Effects ▸ User ▸
  Stylize, `effects.apply`; wobble, colour bleed, lifted blacks, grain and scanlines, GPU only,
  [shader-effects.md](../shader-effects.md)). For more: lower saturation a little, lift the
  blacks with the master curve, warm or magenta-tint the highlights.
- **Security camera:** on the look node: Saturation 0–20 % (or a slight green tint with the Gain
  wheel), Contrast up, strong grain (`grade.param.grain.*`); then a text layer for the camera
  name and a date / time in a corner (a graphic clip, [graphics.md](../graphics.md)). A high,
  wide framing sells it more than any effect: punch in or reframe with Motion **Scale** /
  **Position** so it looks like a fixed corner camera.
- **Stutter:** **Posterize Time** (Effects ▸ Video Effects ▸ Time; **Frame Rate**, e.g. 8) makes
  the picture jump like a cheap camera's low frame rate. Between shots, the **VHS Damage** or
  **Glitch** transition (Video Transitions ▸ Grunge & Distort) gives a tape-tracking hit at the cut.
- **Running clock:** **Metadata & Timecode Burn-in** (Effects ▸ Video Effects ▸ Video; **Source**
  Sequence or Media Timecode, **Alignment** Top Left / Top Right …, **Size**, **Text Color**) puts a
  ticking timecode in a corner, which reads like a camera clock. Add the camera name with a text
  layer next to it.
- **Revolution:** no date / time-of-day clock yet (the burn-in shows hours:minutes:seconds:frames);
  a date needs a text layer, and a wall-clock time keyframed text changes.
- **Pitfalls:** too much at once looks like a plugin demo; real tape is mostly soft and dull,
  not glitchy; keep the audio matching (thin, a little hiss).
