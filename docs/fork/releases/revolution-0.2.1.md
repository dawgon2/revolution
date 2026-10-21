# Revolution 0.2.1

A smaller release after 0.2.0: the AI explainer now understands node trees, receiving a project from
a nearby computer works through the firewall more easily, audio meters keep their clip lights, and the
fixes from 0.2.0's full test run.

## The explainer understands node trees

- Select a parallel, layer, splitter, combiner or key mixer node and **Explain** describes what it
  does with the nodes feeding it (which correction wins where, blend mode and opacity, which channel
  goes where, which selections are combined), not only serial corrections.
- With a local AI model installed, these notes are written by the model, but a note is shown only if
  every name and number in it matches the grade; anything else falls back to the plain built-in text.
- **New explainer model** `smol360-v3` (Settings ▸ AI), now the first choice when installed: trained on
  node trees and compositions as well as single nodes. On 141 held-out test cases 138 of its answers
  pass the fact check (the previous model: 92, almost all misses on comps and branches), at about
  1.3 s per answer on the CPU. Answers that fail the check are never shown; the built-in text is.

## Receiving from a nearby computer

- Receiving now uses the standard port **47821** (another free port if it is taken), so a firewall
  needs only one rule.
- The Linux packages bring a firewall rule for it with them (not switched on); the **Receive** window
  shows the one command that allows it in the firewall your computer uses.
- Typed addresses with a `/24` on the end (as copied from some network tools) are accepted.

## Audio meters

- A meter's clip light now **stays red** after the track clips, until you click the meter (Audio Track
  Mixer, Audio Clip Mixer and Audio Meters); the click also resets the peak hold.
- Every audio track header in the timeline has a thin meter of its own, with the same clip light.

## Fixes

- Fixes from 0.2.0's full test run, mainly around the Resolve-style Edit page that replaced the
  Premiere Tools panel in the Resolve workflow.

## Install

As before: tarball, AppImage, .deb, .rpm and Flatpak for Linux (x86_64 and aarch64), .msi and portable
.zip for Windows (x64 and arm64), and `SHA256SUMS`. The built-in updater offers 0.2.1 to 0.2.0 and 0.1.x
users.
