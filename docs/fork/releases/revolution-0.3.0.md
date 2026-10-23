# Revolution 0.3.0

A big colour and compositing release: Magic Mask finds a person or object for you, the Color
page gets a colour warper, HDR zone wheels, a 3D qualifier and planar-tracked windows, and the
compositor gains trackers, roto and paint, expressions, groups and templates. Voice isolation gets
a better model.

## Magic Mask

- Draw a stroke on a person or thing in the Color page's **Magic Mask** tab: Revolution finds the
  whole object and makes a matte for the node, then follows it through the shot (forward,
  backward or both).
- Add strokes to take in more, subtract strokes to leave things out, and refine the edge.
- Runs on the graphics card (about 30 ms a frame on a desktop GPU), on the processor if there is
  no suitable GPU.
- Works in node-tree grades too: outside nodes correct everything except the mask.

## Color page: new tools

- **Colour warper** (Warper tab): a hue / saturation mesh and a chroma / luma mesh. Drag a point
  to move those colours, pin points to keep them where they are, set strength and how far a move
  spreads. One undo step per drag.
- **HDR zone wheels** (HDR tab): Black, Dark, Shadow, Light, Highlight and Specular wheels plus
  Global, each with balance, exposure (in stops) and saturation, and each zone's edge and falloff
  adjustable. A graph shows where each zone acts.
- **3D qualifier** (Qualifier tab ▸ 3D): paint include and exclude strokes on the picture; the
  colours under them become a soft volume to key on. Softness and Despill (use it with Invert to
  clean green or blue screen spill off a subject). HSL stays the default.
- **Planar-tracked windows** (Windows tab ▸ Planar): a window follows a flat surface (a sign, a
  screen, a wall) in perspective, both ways from the playhead. Stop keeps what was tracked; ✕
  planar removes the track. You can still drag the window where you see it.

## Compositor

- **Tracker and Planar Tracker nodes:** match move, stabilise, corner pin, and screen
  replacement on flat surfaces.
- **Roto and Paint nodes:** Bézier / B-spline shapes with per-point feather, shapes that follow a
  track, and paint strokes (colour, clone, reveal, erase).
- **Expressions:** type `=` in a parameter to drive it from time, frame, other parameters or
  noise / wiggle.
- **Groups and macros:** select nodes ▸ Group, open a group, expose its controls.
- **Templates:** save part of a comp and drop it into another (canvas menu).

## Audio

- **Voice Isolation** renders now use the better model when it is installed, and it holds back on
  clips that are already clean, so good recordings stay untouched.

## Known problems

- Warper and 3D-qualifier settings aren't keyframed yet; 3D-qualifier strokes aren't drawn on the
  picture after you make them.
- Expressions don't work on Text, Color-node or tracker parameters yet; Analyse / Track inside a
  group isn't available.
- Magic Mask can mix up two people dressed alike standing next to each other; add a subtract
  stroke on the other person.

## Install

As before: tarball, AppImage, .deb, .rpm and Flatpak for Linux (x86_64 and aarch64), .msi and portable
.zip for Windows (x64 and arm64), and `SHA256SUMS`. The built-in updater offers 0.3.0 to 0.2.x and 0.1.x
users.
