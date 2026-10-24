# Revolution 0.3.2

A small follow-up to 0.3.1: you can now see which AI tools are using memory and stop them, the
Timeline Viewer defaults to 1080p, effect settings are easier to find, and Project Manager
thumbnails keep their real shape.

## AI tools

- **Window ▸ AI Tools:** every AI tool (the Assistant and AI explanations, the Assistant's picture
  model, Magic Mask, voice clean-up and rendered Voice Isolation, Text to Speech) with whether its
  model is loaded and what it is doing. **Stop** on a row ends that tool's work and frees its
  memory; **Stop All and Free Memory** does them all (also Edit ▸ Stop All AI and Free Memory).
- **Assistant ▸ Free memory:** stops the answer being written and frees the Assistant's models.
  The chat stays.
- Nothing is lost when you stop: a stopped job changes nothing, a stopped answer says it was
  stopped, and each model loads again the next time you use its tool. Voice Isolation's small live
  model stays loaded so effects on your clips keep working.

## Timeline Viewer

- Plays and pauses at **1080p** by default: the timeline's own shape fitted in 1920 × 1080
  (1620 × 1080 for a 3:2 6.2K timeline, 1080 × 1920 for a vertical one), so large footage plays
  lighter. Choose Full or 1/2 in the resolution menu when you want more detail; exports always
  use the full size.

## Effects

- Right-click a clip ▸ **Effect Controls** (**Keyframes** in the Resolve layout) opens its settings.
- The Inspector's Effects tab, when empty, points to the **Audio** tab, where audio effects such as
  DeNoise are.

## Project Manager

- Thumbnails keep the timeline's own shape (3:2, 16:9, vertical…) with bars, instead of being
  stretched.

## Guide and Assistant

- New guide sections the Assistant answers from: reducing fan or hiss noise under music or an
  instrument (DeNoise settings, High Pass, what to expect), what to do when an effect seems
  missing (wrong clip, video or audio part), and stopping AI tools.
