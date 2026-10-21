# Tracking

**[Revolution] [Fork]** · making something follow movement in the picture: a mask that follows a
face, a new picture pinned onto a phone or TV screen; written for Phase Y3 (Y3.3 screen
replacement)

## What tracking does

> id: tracking.what | does: follows a part of the picture and writes keyframes for a mask, a layer or Corner Pin | cmd: masks.track, tracking.attach, tracking.cornerPin | resolve: Tracker (Color page), Fusion trackers | premiere: mask tracking | when: something placed once must stay on a moving object; a blur, mask or title that follows a person or car as they walk or drive | see: tracking.follow, tracking.screen

**[Revolution]**

**Tracking** follows a part of the picture from frame to frame and writes what it found as
keyframes, so something you place once keeps its place while the camera or the subject moves.
Revolution tracks by looking for small, detailed spots (corners, texture, text) inside the area you
mark and following them. Flat areas (a blank wall, a dark screen) have nothing to follow; the
track stops and says so ("lost track") instead of guessing.

There are three kinds:

| What follows | Use it for | Command |
|---|---|---|
| a **mask** (move, rotate, scale) | blurring a face, colour-correcting one object | `masks.track` (Mask Path's track buttons) |
| a **layer's Motion** (position; optionally rotation and scale) | a title, arrow or sticker that stays on something moving | `tracking.attach` (Motion's Attach to Track buttons) |
| the four **Corner Pin** points (perspective) | putting a picture onto a screen, a sign, a page | `tracking.cornerPin` (Corner Pin's Track Surface buttons) |

## Making a title or sticker follow something

> id: tracking.follow | does: makes a title, arrow or sticker follow something that moves | cmd: tracking.attach, tracking.blurRegion | resolve: Fusion Tracker + Merge | premiere: — (After Effects tracking) | when: callouts on moving objects; blurring a face or a plate | see: tracking.what

**[Fork]**

1. Put the shot on **Video 1** and the title, arrow or sticker on **Video 2**, above it.
2. Go to a frame where the thing to follow is clearly visible and move the layer onto it
   (**Motion ▸ Position**, or drag it in the Program monitor). What gets followed is the detail in
   a small box (120 × 120 pixels) around the layer's **Position**, in the shot below.
3. In **Effect Controls** (**Keyframes** in the Resolve workflow; the **Inspector**'s Transform has the same row), on Motion's **Attach to
   Track** row, pick what to follow with the wrench menu: **Position** (the default),
   **Position & Rotation**, or **Position, Scale & Rotation**.
4. Click **▶▶** (forward) or **◀◀** (backward); the single arrows do one frame. Each run is one
   undo step and writes Motion keyframes; clicking again stops it.

Which method to pick is a trade-off: **Position** is the steadiest and right for most callouts
(the label stays upright and the same size even if the object turns); the other two also turn and
grow the layer with the object, which looks glued on but shakes more when the detail is weak. For
blurring a face or a number plate, use a mask on a blur instead (**Gaussian Blur** or **Mosaic**
with a mask, then Mask Path's track buttons): the mask follows, the blur stays inside it. The
command `tracking.blurRegion` does all three in one go (the effect, an ellipse mask around a point,
the track; `effect` `mosaic` for pixelation), for agents and scripts; each part stays its own undo
step, so if the track is lost the blur is still there to place by hand.

## Screen replacement with Corner Pin

> id: tracking.screen | does: pins a picture onto a screen or sign with Corner Pin's Track Surface | cmd: tracking.cornerPin | resolve: Fusion Planar Tracker | premiere: Corner Pin keyframed by hand | when: screen replacement, signs, pages | see: tracking.what

**[Revolution] [Fork]**

1. Put the shot with the screen on **Video 1** and the picture you want on the screen on
   **Video 2**, above it, over the same frames.
2. Apply **Corner Pin** (Effects ▸ Video Effects ▸ Distort) to the clip on Video 2.
3. Go to a frame where the screen is clearly visible. In **Effect Controls** (**Keyframes** in
   the Resolve workflow), set **Upper Left**, **Upper Right**, **Lower Left** and **Lower Right**
   to the screen's four corners. The picture now sits on the screen in this frame.
4. On Corner Pin's **Track Surface** row, click **▶▶** (track forward) or **◀◀** (track backward);
   the single-arrow buttons track one frame at a time. The track runs in the background, shows its
   progress in the jobs list, and clicking any of the buttons again stops it. What was tracked
   before stopping is kept.
5. Play it back. Each tracking run is **one undo step**, so Undo removes a whole run.

Why it works: a flat surface seen at an angle changes shape in a predictable way (a
**perspective** change). Revolution follows the detail **inside** the four corners, works out that
shape change each frame and moves the four points with it, so the corners stay on the screen even
when they leave the frame or something passes in front of one of them.

Tips:

- Track the **surface**, not its contents: a screen playing video has changing content; pin the
  corners to the bezel or frame around it if the screen itself is busy or black.
- If the track drifts after a while, stop it, fix the four points at the frame where it drifted
  and track again from there.
- The surface is tracked in the clip **right below** the one with the Corner Pin at the frame you
  start from (or in the clip itself if nothing is below). An agent or script can choose another
  clip with the command's `source` parameter.

If you know Premiere: Premiere's Corner Pin has no tracker of its own (people use After Effects or
Mocha for this). If you know Resolve: this is like a Planar Tracker driving a Corner Pin in Fusion,
done here on the clip's effect instead.
