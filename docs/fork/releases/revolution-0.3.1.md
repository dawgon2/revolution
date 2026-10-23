# Revolution 0.3.1

A follow-up to 0.3.0 that finishes what 0.3.0 left open: warper and 3D-qualifier settings can be
keyframed, trackers work inside groups, roto and Magic Mask run faster on the graphics card, and
the Grading Coach knows the new tools.

## Color page

- **Warper and 3D qualifier keyframes:** Strength and Range (Warper) and Softness and Despill
  (3D qualifier) have keyframe buttons, like the other node settings.
- **3D-qualifier strokes stay on the picture:** include strokes show in green, exclude strokes in
  dashed red, while the 3D qualifier is open.
- **Magic Mask:**
  - **Person** mode now finds the whole figure from one click on a shirt or a face. **Object**
    mode keeps exactly what you click.
  - Better and Faster run their last step on the graphics card too, so each tracked frame is
    quicker.
  - Your Better / Faster choice is remembered for next time.
  - Editing inside a nested sequence now asks you to track its Magic Mask again, instead of
    showing a matte that no longer fits.

## Effect Controls

- Mask rows have a **Planar** button: the mask follows a flat surface (a sign, a screen) in
  perspective, as grade windows already could.

## Compositor

- **Trackers and roto inside groups:** Analyse and Track now work on nodes inside a group.
- **Expressions** on a Color node's grade settings and a Text node's settings.
- **Tracker Confidence:** the Inspector shows how sure the tracker was on each frame. Green bars
  mean sure, amber and red mean check that frame.
- **Corner Pin:** pick which tracked point each corner goes to (Top left … Bottom left).
- **Roto shapes are drawn on the graphics card:** about 7–20× quicker on a desktop GPU with
  feathered shapes at 6K (measured on synthetic shapes).
- Groups whose contents don't change are drawn once and remembered, like other unchanging parts
  of a comp.

## Media and playback

- Film transferred to 29.97 video with soft pulldown (MPEG-2 3:2 pulldown flags) now reads as
  23.976 fps, not "Variable".
- In the Source monitor, **End** and **Go to Out** land on the last real frame of
  variable-frame-rate clips.

## Grading Coach and languages

- The Coach answers questions about the 0.3.0 tools (Magic Mask, the 3D qualifier, planar
  windows, the warper, HDR wheels, trackers, roto, paint, expressions, groups and templates).
- German, French and Japanese text for the new compositor and Color page tools.

## Updates

- Every release, patch releases included, comes in every format again.
- The built-in updater only offers a release that has a file for your install.

## Known problems

- Tracker and Planar Tracker data can't take expressions: the tracks are measured motion, and an
  expression would replace it.
- Magic Mask can still mix up two people dressed alike standing next to each other; add a
  subtract stroke on the other person.

## Install

Tarball, AppImage, .deb, .rpm and Flatpak for Linux (x86_64 and aarch64), .msi and portable .zip
for Windows (x64 and arm64), and `SHA256SUMS`. The built-in updater offers 0.3.1 to earlier
versions.
