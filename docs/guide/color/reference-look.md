# Get this look: reference pictures

**[Fork]** Bring in a picture you like and ask how to get its look: Revolution measures the
difference, answers in two parts (in the grade, on set) and offers Shot Match as a first pass.
See also [Matching shots](matching.md).

## Reference pictures

> id: color.reference-look.pictures | does: brings a picture you like (a film frame, a photo) into the Gallery to compare against and match to | ui: stills.import | cmd: stills.import, compare.set, grade.shotMatch | resolve: Gallery ▸ Import (stills) | premiere: Lumetri Comparison View reference | when: you want your shot to look like a picture from elsewhere | see: color.reference-look.get-this-look

**[Fork]** (the idea of importing gallery stills is Resolve's too)

### Why

Often the look you want isn't in your own footage: it is a frame from a film, a photo, a mood
board. Bringing it in as a **reference picture** lets you see it beside your shot (wipe or split)
and lets Shot Match measure it.

### Steps

1. In the **Gallery**, click **Import…** {ui:stills.import} and choose a PNG, JPEG, TIFF, WebP,
   BMP, GIF, Targa or DPX file, or drag the file from your file manager onto the Gallery.
2. It appears as a still named after the file. Click it to compare (Wipe / Split) as with any still.
3. To match to it: in the **Match** tab choose it under **Reference** {ui:grade.match.reference}
   ("Reference picture: …"), then **Shot Match** (see below).

**[Fork]** A reference picture has no grade, so double-clicking it (`stills.apply`) does nothing but
explain that. It is scaled down to at most 1920 px on its longer side and stays on this computer
(in the project's stills folder); it is never shared or used as AI training data. Files larger
than 64 MB or 16 384 px, and damaged files, are refused with a message.

**Trade-offs:** matching to a *still from your own timeline* compares like with like (same camera,
same light). A *reference picture* is the quickest way to aim for a look you saw elsewhere, but its
light, lens and subject differ from yours: treat Shot Match's node as a first pass, and expect
some of the look (lighting, focus, haze) to need work on set rather than in the grade. Matching
**by eye** with a wipe is slower but lets you choose which part of the look matters.

## Get this look from a picture

> id: color.reference-look.get-this-look | does: measures how a reference picture differs from your shot and answers in two parts, in the grade and on set | ui: coach.ask.doThis, coach.ask.reference | cmd: coach.reference, coach.assist, grade.shotMatch | resolve: — (Shot Match to a still, by eye) | premiere: — (Lumetri Comparison View) | when: "how do I get this look?" about a picture you like | see: color.reference-look.pictures

**[Fork]**

### Why

A look is partly colour and partly light. Revolution measures the colour side for you (exposure,
contrast, black level, shadow and highlight colour, saturation, skin, bars, grain) and says which
part a grade can do and which part came from the set, so you don't chase a lighting difference
with a grade.

### Steps

1. Import the picture into the **Gallery** ({ui:stills.import}) and click it so it is the compared
   still.
2. Ask in the Ask box: "How do I get this look?" (the compared reference picture goes along;
   `coach.assist` takes `"reference": <still id>` too). Without the assistant's model,
   `coach.reference` with `"still": <id>` gives the same measured part.
3. Read the answer's parts:
   - **Reference picture (measured):** theirs vs yours, in numbers turned into words.
   - **In the grade:** numbered steps, each from a guide section, starting with Shot Match.
   - **On set:** what light, lens or haze would do that a grade can't.
   - A note when the pictures are too far apart (night against day): a grade only gets part of the way.
4. Click **Do this: Shot Match to the reference** {ui:coach.ask.doThis} under the answer ({ui:coach.ask.reference}) for a first pass: one
   ordinary "Shot Match" node you can see, adjust or switch off. Then work down the steps.

With the picture model installed, the answer also says what it **sees** in the reference (light
direction and softness, background blur, haze or glow, grain, framing). Without it, the answer
comes from the measurements alone.

### Options

- **Beginner:** Do this, then only the first two or three steps; compare with a wipe.
- **Pro:** skip Do this; build balance, contrast and look on separate nodes from the measured
  lines, and use the reference only as a target on the scopes.
- **By eye:** wipe or split against the picture and match one thing at a time (blacks first, then
  colour, then saturation). Slowest, but you choose which part of the look matters.

**[Fork]** Revolution never claims to know how the reference was made: the steps say what would move
*your* shot towards it. The reference picture, its description and its numbers stay on this
computer; a shared example never includes them.

