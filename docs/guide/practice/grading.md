# Practice notes: colour grading

**[Standard]** · how working colourists commonly approach a grade, one technique per note, with
the Resolve and Premiere way and the Revolution way side by side. Written in our own words from
common practice (AI-PLAN AI3 item 0a, 0c); none of it is the one right way: each note says when it
fits and what can go wrong.

## Normalise log footage before grading

> id: practice.grading.normalise-log | does: turns flat log footage into a normal-contrast picture with a colour-space transform before any creative work | resolve: Color Space Transform node or colour-managed project | premiere: Lumetri input LUT or log auto-detection | when: any log or raw footage (F-Log2, S-Log3, LogC) | see: color.color-management.input, practice.grading.balance-first

**[Standard]**

- **Goal:** start every shot from the same, technically correct place.
- **Resolve:** a colour-managed project (DaVinci YRGB Color Managed) or a Color Space Transform
  as the first or last node, from the camera's log to the timeline space.
- **Premiere:** Lumetri reads many log formats automatically or takes a camera LUT as input LUT.
- **Revolution:** set the clip's input to **Fujifilm F-Log2 / F-Gamut** if it isn't detected
  (`clip.interpretFootage`) and the sequence's working space to DaVinci Wide Gamut / Intermediate
  (`sequence.colorSettings`); the transform happens before your first node.
- **Why:** grading log by eye with contrast and saturation alone gives different results per shot
  and breaks highlights.
- **Pitfalls:** applying a camera LUT **and** colour management doubles the transform; a LUT made
  for another log curve (F-Log vs F-Log2) looks almost right but shifts skin.

## Balance before the look

> id: practice.grading.balance-first | does: fixes exposure and white balance on the first node, before any creative grade | resolve: first serial node: Lift / Gamma / Gain, Offset | premiere: Lumetri Basic Correction | when: every shot | see: practice.grading.normalise-log, color.primaries.panel

**[Standard]**

- **Goal:** a neutral, correctly exposed picture that a look can sit on.
- **Resolve / Premiere:** first node (or Basic Correction): exposure with Offset or Gain, white
  balance with Offset or Temperature / Tint, then a look later.
- **Revolution:** first node: Offset / Gain wheels and Temperature / Tint, or the white-balance
  picker on something neutral (`grade.wb.pick`); label it "Balance" (`grade.labelNode`).
- **Why:** a look applied to an unbalanced shot has to fight the cast; balanced shots also match
  each other more easily.
- **Pitfalls:** "neutral" is not always right: a sunset or tungsten scene should stay warm;
  balance to what the scene should feel like, not to grey.

## Set black and white points on the waveform

> id: practice.grading.black-white-points | does: places the darkest and brightest parts of the picture with the waveform instead of by eye | resolve: Lift / Gain with the waveform | premiere: Lumetri Blacks / Whites with the waveform | when: setting contrast on any shot; deciding how dark the blacks or how bright the whites should go | see: color.finishing.scopes

**[Standard]**

- **Goal:** use the full range without crushing shadow detail or clipping highlights.
- **Resolve / Premiere:** watch the waveform; bring the lowest trace near the bottom with Lift /
  Blacks and the highest near the top with Gain / Whites.
- **Revolution:** open the waveform in the scopes (`scopes.read` for numbers) and use Lift and
  Gain on the balance node.
- **Why:** monitors and eyes adapt; the waveform doesn't.
- **Pitfalls:** not every shot should reach both ends (fog, night, a soft look); crushing blacks
  hides noise but also detail in hair and clothes.

## Shape contrast around a pivot

> id: practice.grading.contrast-pivot | does: adds or reduces contrast around a chosen brightness, keeping that brightness where it is | resolve: Contrast and Pivot | premiere: Lumetri Contrast (fixed pivot) or an S-curve | when: punchier or softer pictures without moving skin exposure | see: color.primaries.panel, practice.grading.curve-shoulder

**[Standard]**

- **Goal:** change contrast without making the subject brighter or darker.
- **Resolve:** Contrast with Pivot set to the subject's brightness.
- **Premiere:** Contrast, or an S-curve in RGB Curves anchored at the subject.
- **Revolution:** Contrast and Pivot on the Primaries panel (`grade.param.contrast`,
  `grade.param.pivot` with `grade.setParam`); set Pivot to the face's brightness, then add
  Contrast.
- **Why:** contrast always pivots somewhere; choosing the point keeps faces stable.
- **Pitfalls:** strong contrast also raises saturation in many spaces; check skin after.

## A soft highlight shoulder with a curve

> id: practice.grading.curve-shoulder | does: bends the top of the master curve so bright parts roll off gently, like film | resolve: Custom curve shoulder, Soft Clip | premiere: RGB Curves highlight point | when: skies, windows, lamps that look harsh | see: color.finishing.soft-clip, practice.grading.contrast-pivot

**[Standard]**

- **Goal:** highlights that fade into white instead of hitting a flat edge.
- **Resolve:** lower the top of the custom curve, or use Soft Clip.
- **Premiere:** pull the top of the RGB curve down a little.
- **Revolution:** the master curve (`grade.curve.master`) or Highlight soft clip on the node
  (`hl.knee`, `hl.rolloff`).
- **Why:** digital sensors clip abruptly; a shoulder hides the edge.
- **Pitfalls:** too much makes whites grey and the picture flat.

## Saturation after contrast

> id: practice.grading.saturation-order | does: sets saturation after exposure and contrast, and watches it on the vectorscope | resolve: Saturation, Color Boost | premiere: Lumetri Saturation, Vibrance | when: colours look dull or too loud | see: color.finishing.scopes

**[Standard]**

- **Goal:** colours that feel rich without going garish.
- **Resolve / Premiere:** raise saturation after contrast; Vibrance / Color Boost lifts dull
  colours more than strong ones.
- **Revolution:** Saturation on the node (`grade.param.saturation`), then check the vectorscope.
- **Why:** contrast changes how saturated colours look; setting saturation first means doing it
  twice.
- **Pitfalls:** skin goes orange quickly; strong reds and blues clip on a Rec. 709 delivery.

## Check skin against the skin-tone line

> id: practice.grading.skin-line | does: uses the vectorscope's skin-tone line to keep faces natural across shots | resolve: vectorscope skin tone indicator | premiere: Lumetri vectorscope skin line | when: every shot with people | see: practice.grading.qualify-skin, color.finishing.scopes

**[Standard]**

- **Goal:** skin that reads as healthy and matches from shot to shot.
- **Resolve / Premiere:** isolate the face (a window or qualifier, or just look), and see whether
  its trace on the vectorscope lies along the skin-tone line.
- **Revolution:** the vectorscope in the scopes panel; isolate the face with a window to read it
  alone.
- **Why:** all skin, whatever its depth, falls near one hue direction; deviations read as ill or
  sunburnt.
- **Pitfalls:** the line is a guide, not a target: makeup, coloured light and creative looks move
  skin on purpose.

## Qualify skin to protect or adjust it

> id: practice.grading.qualify-skin | does: selects skin with a hue / saturation / luma key to warm, cool or soften it alone | resolve: HSL Qualifier on a skin node | premiere: Lumetri HSL Secondary | when: skin goes grey, green or orange after a look | see: color.secondaries.qualifier, color.recipes.skin

**[Standard]**

- **Goal:** fix faces without changing everything else.
- **Resolve / Premiere:** pick skin with the qualifier eyedropper, soften the matte, adjust hue
  and saturation a little.
- **Revolution:** a new node, Qualifier tab, pick skin (`grade.pickQualifier`), widen / soften
  (`qual.hueWidth`, `qual.soft`), check the matte, then small primary moves.
- **Why:** primaries move every colour; a qualifier moves only the selection.
- **Pitfalls:** hard mattes chatter on movement; wood, sand and walls often share skin's hue: limit
  with a window.

## Deepen a sky with a qualifier and a gradient

> id: practice.grading.sky | does: makes a sky bluer or darker with a qualifier limited by a gradient window | resolve: qualifier + linear window | premiere: HSL Secondary + mask, or a graduated mask | when: pale or blown skies | see: color.recipes.sky, color.secondaries.windows

**[Standard]**

- **Goal:** a richer sky without touching the land or faces.
- **Resolve / Premiere:** qualify blue, limit it to the top with a linear window / gradient mask,
  lower gain and add saturation.
- **Revolution:** qualifier on blue plus a linear window (`grade.window.add.linear` on the Windows
  tab) on the same node.
- **Why:** the window keeps blue clothes and reflections out; the qualifier follows the horizon's
  shape.
- **Pitfalls:** blown (white) skies have no colour to qualify: a gradient alone or a sky
  replacement is the only fix.

## A vignette to guide the eye

> id: practice.grading.vignette | does: darkens the edges slightly with a soft window so the eye goes to the subject | resolve: circular window, inverted, gain down | premiere: Lumetri Vignette | when: busy frames, interviews, a cinematic feel | see: color.secondaries.windows

**[Standard]**

- **Goal:** attention on the subject without the viewer noticing why.
- **Resolve:** an ellipse window, inverted, very soft, Gain or Gamma down a little.
- **Premiere:** Lumetri Vignette (amount, midpoint, feather).
- **Revolution:** an ellipse window on its own node, inverted (`grade.window.0.invert`), high
  softness, Gain down; track it if the subject moves.
- **Why:** eyes go to the brightest area first.
- **Pitfalls:** visible dark corners look dated; keep it subtle and soft.

## Put the look on its own node, and mix it in

> id: practice.grading.look-node | does: keeps the creative look (curves, split tones, a LUT) on a late node so it can be dialled up or down for every shot | resolve: look node or LUT node after the correction nodes, Key Output gain | premiere: Lumetri Creative ▸ Look with Intensity | when: a consistent look across a whole film; dialling a look down to 60% on a couple of shots | see: color.finishing.lut, color.nodes.starter-tree

**[Standard]**

- **Goal:** one look that sits on corrected shots, adjustable in strength.
- **Resolve:** a node (or shared node / group post-clip) after the correction nodes; its key
  output gain lowers its strength.
- **Premiere:** Creative ▸ Look and Intensity.
- **Revolution:** a late node with the look (curves, wheels or `grade.setLut` with its mix,
  `grade.param.lut.mix`); copy it to other shots with `grade.append`.
- **Why:** corrections differ per shot, the look shouldn't; separating them keeps both simple.
- **Pitfalls:** a look before balancing exaggerates differences between shots. (Revolution has no
  shared / group nodes yet: re-append the look node after changing it.)

## Split toning: cool shadows, warm highlights

> id: practice.grading.split-tone | does: tints shadows and highlights towards opposite colours, the "teal and orange" family of looks | resolve: Lift towards teal, Gain towards orange; or Color Warper | premiere: Lumetri Creative ▸ Shadow / Highlight Tint | when: a stylised, cinematic contrast between skin and background | see: practice.grading.look-node, practice.grading.qualify-skin

**[Standard]**

- **Goal:** separation between warm subjects and a cool world.
- **Resolve / Premiere:** push shadows (Lift / Shadow Tint) towards teal-blue and highlights (Gain
  / Highlight Tint) towards orange, gently.
- **Revolution:** on the look node, Lift wheel slightly towards blue-cyan, Gain slightly towards
  orange (`grade.wheel.lift`, `grade.wheel.gain`).
- **Why:** skin is orange-ish; a cool background makes it stand out.
- **Pitfalls:** overdone, everything turns two-tone and skin goes radioactive; protect skin with a
  qualifier when the push is strong.

## Finish with grain

> id: practice.grading.grain-finish | does: adds fine grain on the last node of the tree, after the look, to hide banding and unify sources | resolve: Film Grain (last node or timeline) | premiere: Noise effect or a grain overlay | when: skies and gradients that band; mixed cameras | see: color.finishing.grain, color.recipes.grain

**[Standard]**

- **Goal:** a texture that ties shots together and hides 8-bit banding on delivery.
- **Resolve / Premiere:** Film Grain on a last node; Premiere often uses a grain overlay.
- **Revolution:** grain on the last node (`grade.param.grain.strength`, size, softness).
- **Why:** gentle noise breaks up steps in gradients after compression.
- **Pitfalls:** streaming compression smears heavy grain into mush; check on the exported file.

## Match shots: blacks, whites, then mids and colour

> id: practice.grading.match-order | does: matches a shot to a reference in a fixed order: black level, white level, midtones, then colour | resolve: gallery still wipe, parade, Shot Match as a start | premiere: Lumetri Comparison View, Color Match | when: two angles of one scene don't cut together | see: color.matching.shot-match, color.recipes.match

**[Standard]**

- **Goal:** shots of one scene that feel continuous.
- **Resolve / Premiere:** wipe against a still of the reference, compare parades, match black
  then white then mid levels per channel, then saturation and skin.
- **Revolution:** grab a still of the reference (`stills.grab`), wipe, Shot Match for a first pass
  (`grade.shotMatch`), then fine-tune and check with `grade.compareShots`.
- **Why:** levels first: a colour mismatch is often a level mismatch in one channel.
- **Pitfalls:** matching numbers exactly can be wrong when the light really differs (a window
  in one angle); match what the viewer feels.

## Day for night

> id: practice.grading.day-for-night | does: makes a daylight shot read as night: darker, cooler, less saturated, lights kept bright | resolve: Gamma / Gain down, cool tint, low saturation, windows on practicals | premiere: Lumetri exposure, temperature, saturation, masks | when: night scenes shot in daylight | see: practice.grading.vignette

**[Standard]**

- **Goal:** a believable night from a day shot.
- **Resolve / Premiere:** bring exposure down a lot, cool the picture, lower saturation, darken
  the sky most, keep any light sources and eyes brighter with windows.
- **Revolution:** a node with Gain / Gamma down, Temperature cool, Saturation low; a gradient window
  to darken the sky; ellipse windows to keep practicals and faces lit.
- **Why:** at night eyes see less colour and a bluer world; light comes from sources, not the sky.
- **Pitfalls:** hard shadows and a bright sky give it away: shoot with the sun behind, avoid sky.
- **Variants (pick by the story, not one correct night):**
  - **Moonlit blue** (classic film night): strong cool balance (Temperature well down or Gain to
    blue), Saturation 30–50 %, deep shadows; reads instantly as night, but can look like a filter.
  - **Neutral, modern night:** only a little cool, darker and more contrasty, colours kept close
    to natural; closer to how night looks on modern cameras, but needs good lights in shot.
  - **Urban sodium night:** warm-orange practicals and street light against cool shadows (a
    qualifier keeps the lights warm on their own node); city exteriors, see
    [practice.looks.neon](looks.md) for the saturated version.
  - **Day for dusk:** less darkening, sky kept a little brighter and pushed to deep blue or
    purple with a gradient window, warm lights on: the "blue hour" between day and night.
  - **Keep the lights glowing:** after darkening, a qualifier or window on shop signs, lamps and
    windows on a later node with Gain up; **Wonder Glow** (Video Effects ▸ Lights & Glows) adds a
    halo ([effects.md](../effects.md) ▸ stylised effects).
