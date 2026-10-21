# Revolution 0.2.2

Pages remember how you arrange them (a **Save Layout** button and layout presets), the audio
mixer gets track effects and sends, and variable-frame-rate clips from phones and screen recorders
stay in sync.

## Save a page's layout

- **Save Layout**, bottom right next to Lessons, keeps the page arranged the way it is now: it
  opens like this every time, also after a restart.
- Right-click it to **Reset to Saved Layout** (undo changes since you saved) or **Restore Original
  Layout** (the page as Revolution ships it).
- Even without saving, a page now keeps how you left it when you go to another page and come
  back, and after you quit (in the Premiere workflow, each workspace does). Before, switching pages
  threw unsaved changes away.

## Layout presets

- **Window ▸ Layout Presets ▸ Save Layout as Preset…** saves every page's arrangement under one
  name, like Resolve's layout presets: one for the laptop screen, one for the desk monitor.
  Choose its name in the same menu to bring it back.
- **Manage Layout Presets…** loads, updates, renames and deletes them, and exports one to a
  `.revlayout` file to import on another computer.
- Also in Save Layout's right-click menu.

## Audio: track effects and sends

- **Drag an effect onto a track.** Drag an audio effect from the Effects panel onto an audio
  track's header, a mixer strip, or the track view in Effect Controls: it becomes a track effect
  in the first free slot (one undo step). Video effects are ignored there.
- **A track's effects next to your clips.** Click an empty part of an audio track header: Effect
  Controls (Premiere workflow) or Inspector ▸ Audio (Resolve workflow) shows the track's effects
  with On / Post-Fader / Edit… / Remove and every setting. Click a clip to get its controls back.
- **Send to.** Right-click an audio track header ▸ Send to ▸ a submix, or New Bus / New Submix.
- **Voice Isolation Attenuation Limit** (also in Essential Sound ▸ Repair): how much Voice
  Isolation may take away. Clean Up Voice sets 12 dB on mildly noisy voices (more natural) and no
  limit on louder noise.
- New Coach lesson: "One effect for a whole track (track effects and sends)".

## Variable-frame-rate clips (phones, OBS, screen recorders)

- A clip whose frames aren't evenly spaced shows **(VFR)** after its frame rate in the Project
  panel, the Media Browser and Properties, plus a **Variable Frame Rate** row (average and
  slowest–fastest rate). MP4 / MOV, MKV / WebM and MPEG-TS.
- Phone clips whose frame times jitter by a millisecond now read as 30 fps, not 29.13.
- Every frame is shown at its own recorded time, so picture and sound stay within one frame on a
  20-minute clip, in playback and export. Noise reduction, frame blending / optical flow and
  reversed clips use the clip's real frames; Step Back / Forward in the Source monitor go to the
  real previous / next frame.
- **Clip ▸ Modify ▸ Interpret as Constant Frame Rate / Conform to Sequence Frame Rate** for when
  you want every recorded frame played evenly (the sound plays as recorded, so it drifts on long
  clips). Interpret Footage takes a frame rate too.

## Known problems

- Clips already on the timeline keep their length after Interpret as CFR / Conform; edit the clip
  in again for the new length.
- Source monitor Go to Out / End on a VFR clip steps back one typical frame.
- New Bus from a track header's menu is two undo steps (the bus, then the send).

## Install

As before: tarball, AppImage, .deb, .rpm and Flatpak for Linux (x86_64 and aarch64), .msi and portable
.zip for Windows (x64 and arm64), and `SHA256SUMS`. The built-in updater offers 0.2.2 to 0.2.x and 0.1.x
users.
