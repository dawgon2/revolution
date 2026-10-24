# Audio clean-up

Status: **written** (AUDIO-PLAN A2): Voice Isolation and its model, the manual voice clean-up
process, Clean Up Voice and reviewing voice-over takes.

## Voice Isolation

> id: audio.voice-isolation | does: removes the noise around speech with the DeepFilterNet3 model (DeNoise without it) | ui: settings.audio.voiceIsolation.* | cmd: audio.voiceIsolate.* | resolve: Voice Isolation | premiere: Enhance Speech, DeNoise | when: speech over changing noise: traffic, keyboard, wind, crowd | see: audio.manual-voice-cleanup

**[Fork]** effect, **[Standard]** model

Voice Isolation keeps speech and removes the noise around it with DeepFilterNet3 (Schröter et al.,
open source, MIT / Apache-2.0), a trained model that runs on the CPU in pure Rust.

### What it does

Voice Isolation keeps speech and removes everything else: fans, traffic, keyboard clicks, wind,
room hum. It is a trained neural network, so it handles noise that changes from moment to moment,
which a classic noise reducer can't follow.

It lives with the other restoration effects: Effects panel ▸ **Audio Effects ▸ Noise
Reduction/Restoration ▸ Voice Isolation**. Drop it on a clip like any audio effect.

- **Amount** (0–100 %, default 100 %): how much of the cleaned voice you hear. 100 % is fully
  cleaned; lower values mix the original back in, which keeps some room sound and hides small
  artefacts. Keyframe it like any effect parameter.
- **Attenuation Limit** (6–100 dB, default 100 dB = no limit): the most the effect may take away.
  At 20 dB a noise floor is lowered by at most 20 dB instead of removed, and a clean voice is barely
  changed, so the result sounds more natural on mildly noisy recordings. Lower it when the cleaned
  voice sounds dry or "underwater"; leave it at 100 dB for loud noise (traffic, wind).
  Essential Sound ▸ Repair shows the same setting under Voice Isolation as **Attenuation Limit**
  (`repair.isolateLimitDb`); Clean Up Voice sets it for you by the clip's noise level (see "Noise
  level" below): no limit for loud noise, 18 dB down to 6 dB for mild noise (measured on recorded
  speech: on mild noise the limit sounds more natural and scores better; with loud clicks such as
  typing, a limit lets them through).
- **Delay:** the effect looks 20 ms ahead and works in 10 ms steps, 40 ms in all; playback and
  export compensate for it, so the clip stays in sync.

### Live playback and its cost

**[Fork]**

Voice Isolation runs live while you play, scrub to a point, shuttle or export: there is no
render step. A stereo clip costs about one channel's worth of work (the model listens to the mix
of both channels and cleans each with the same mask, so the stereo image stays). Measured on a
small desktop CPU (Intel i5-13500T, its slower cores): about a third of one core per track with
Voice Isolation. One such track plays with no dropouts through seeks, 2× shuttles and audio
device changes; with four, the first 2× shuttle can drop sound for about 0.2 s while it starts.

- **Beginner:** use it on the voice track(s) only, not on music or effects tracks.
- **Pro:** with many tracks using it on a slower computer, avoid fast shuttling during playback,
  or bounce the cleaned dialogue to a new clip once it sounds right.

### Installing the model

**[Fork]**

The model (about 8 MB, licence MIT OR Apache-2.0) is never shipped with the app. To install it:
**Settings ▸ Audio ▸ Voice isolation ▸ Download** ({ui:settings.audio.voiceIsolation.download}).
The download comes from the DeepFilterNet project's own repository, pinned to one release and
checked against its checksum before it is used; a progress bar and **Cancel download** show while
it runs. **Remove** ({ui:settings.audio.voiceIsolation.remove}) deletes it again. The same row
says whether the model is installed and loaded.

Without the model, Voice Isolation quietly works as **DeNoise** (Amount 100 % = 40 dB of
reduction), so a project that uses it still plays and exports everywhere, also on a computer
without the model. The same happens for sequences whose audio isn't at 48 kHz (the model's rate;
other rates are planned).

Offline computers: copy `DeepFilterNet3.zip` from the DeepFilterNet repository (or the
`model_120.ckpt.best` checkpoint inside it) and install it with the command
`audio.voiceIsolate.installFile` (`path`); a zip with the wrong checksum or a file that isn't the
DeepFilterNet3 checkpoint is refused.

Commands (also for the control channel and agents): `audio.voiceIsolate.status`, `.download`,
`.cancelDownload`, `.installFile`, `.remove`.

### Two voice models: live or rendered (options and trade-offs)

**[Fork]**

| Model | How it runs | Choose it when | Trade-offs |
|---|---|---|---|
| **Faster** (`"model": "faster"`): DeepFilterNet3, 8 MB | live, inside the Voice Isolation effect | editing: you hear the result at once and can change it any time | a small model made for real time |
| **Better** (`"model": "better"`): MossFormer2_SE_48K, 222 MB, Apache-2.0, from Alibaba's ClearerVoice-Studio | rendered once into a new audio file | the final dialogue, especially over traffic, typing or wind | not live: a 6-core desktop (i5-11600K) needs about 0.35 s per second of mono audio, so a 10-minute interview takes 3–4 minutes, and any change means rendering again |

How much cleaner is Better? **[Fork, measured 2026-10-22]** With five kinds of noise added to
recorded speech, Better beat Faster on average at every level, most of all on typing and wind;
Faster is slightly better on steady fan noise. On 824 clean studio recordings (VoiceBank-DEMAND)
both left the voice as it was. So Better is the default for rendering.

To use Better:

1. Install its model once: get `last_best_checkpoint.pt` from the model's official page
   (huggingface.co/alibabasglab/MossFormer2_SE_48K; Revolution never downloads it for you) and run
   `audio.voiceIsolate.installFile` with its `path` and `"model": "better"`.
2. Save the project, select the voice clips and run `audio.voiceIsolate.render` (Better is the
   default). A job (you can stop it) writes `<clip name> Voice Isolated.wav` in the
   project's folder, adds it to the clip's bin and switches the clips to it at the same point of
   the recording, as one step you can take back; taking it back leaves the new files on disk until
   you delete them. A stopped job changes nothing and deletes what it wrote.
3. Remove the live Voice Isolation effect from those clips yourself; otherwise both run and the
   voice is cleaned twice.

If the Better model is missing or broken, the render uses Faster and says why.

**Noise level [Fork]:** on a clean recording a model's small changes are all you would get, so a
render first measures how far the voice sits above the noise: full strength up to 25 dB, lighter
from 25 to 40 dB, the original sound from 40 dB (Clean Up Voice uses the same levels;
`"gate": false` always gives full strength). The job's status says what it did for each clip.

- **Beginner:** edit with the live effect; render Better only for the clips you deliver.
- **Pro:** render Better on the dialogue before the mix; the originals stay in the bin.

### Voice Isolation, DeNoise or Essential Sound? (options and trade-offs)

There is no single right tool; pick by the noise and how much time you have.

| Option | Good for | Trade-offs |
|---|---|---|
| **Voice Isolation** | Speech over noise that changes: traffic, keyboard, wind, crowd, music bleed | Speech only (it removes music and effects too); under a tenth of one CPU core per channel; needs the model |
| **DeNoise** | Steady noise: fan, air conditioning, hiss | Can't follow noise that changes; strong settings make "watery" artefacts |
| **Essential Sound ▸ Dialogue ▸ Repair** | A quick start for dialogue with several fixes in one panel (Voice Isolation, noise, rumble, hum, sibilance, reverb) | One slider per fix, so less fine control than the effects themselves in Effect Controls |

- **Beginner:** put Voice Isolation on the voice clip at 100 % and listen. If the voice sounds a
  little processed, lower Amount to 70–85 %.
- **Pro:** keep restoration first in the effect stack (Voice Isolation, then EQ, de-esser,
  compression, loudness), so later effects work on clean speech. For a voice recorded in a quiet
  room, try DeNoise first: it changes the voice least. For music or sound effects, don't use Voice
  Isolation at all.

### Why it works this way

Classic noise reduction learns the noise floor and subtracts it, which works only while the noise
stays the same. DeepFilterNet was trained on thousands of hours of speech and noise, so it
recognises speech directly: it sets a gain for 32 frequency bands every 10 ms and, below 4.8 kHz,
rebuilds the voice's fine detail from the last few moments of sound ("deep filtering"). Revolution's
port gives the same output as the original to within 16-bit precision.

### Noise under music or an instrument (fan, hiss)

**[Fork]**

Use **DeNoise**, not Voice Isolation: Voice Isolation keeps only speech, so it would remove the
guitar together with the fan. A fan is steady noise, and steady noise is what DeNoise is for.

1. Effects panel (Window ▸ Effects) ▸ **Audio Effects ▸ Noise Reduction/Restoration ▸ DeNoise**:
   drag it onto the middle of the clip's **audio** (its waveform), well away from the clip's edges.
   Check the **fx** badge appears on that clip (see [effects.md](effects.md), "When an effect seems
   missing").
2. Select the audio part of the clip and open **Effect Controls** (**Keyframes** in the Resolve
   workflow; Window menu, Shift+5) or the Inspector's **Audio** tab. Set while it plays:
   - **Reduction** (0–40 dB, default 12): how far the fan goes down. Start at 15–20 dB and go up a
     few dB at a time; stop when the instrument sounds thin or "watery".
   - **Threshold** (0–20 dB, default 6): how far a sound must rise above the noise to be kept.
     Lower it when soft notes or the ends of notes get cut; raise it when fan still comes through
     between notes.
   - **Smoothing** (default 50 %): raise it to 70–80 % when you hear burbling or chirping, common
     on long held notes.
3. DeNoise learns the noise by itself while it plays (no "learn" step): give it a second at the
   start before you judge.

Optional: a **High Pass** at 60–70 Hz takes fan rumble away (keep it under 82 Hz, a guitar's low E);
**DeHummer** at the fan's pitch when it whines. Order: High Pass → DeHummer → DeNoise.

What to expect: when the noise is louder than the instrument, the quietest parts of the playing
(soft picking, the tails of notes) are under the noise, and any noise reducer either leaves some
noise or eats those details. Aim for "noise clearly lower, instrument still natural"
(12–20 dB of Reduction) rather than "noise gone". A microphone closer to the instrument, or the fan
switched off, helps more than any amount of clean-up.

- **Beginner:** DeNoise at 15 dB Reduction, defaults otherwise, listen, adjust Reduction only.
- **Pro:** High Pass first, then DeNoise; compare with the effect switched off (the switch next to
  its name) at the same loudness, since a quieter result tends to sound "cleaner" when it isn't.

## Cleaning up a voice by hand

> id: audio.manual-voice-cleanup | does: the order to fix a voice by hand: rumble, noise, hum, esses, level, tone, loudness | ui: essentialSound.* | cmd: essentialSound.* | resolve: Fairlight EQ, Dynamics, Voice Isolation, Dialogue Leveler | premiere: Essential Sound ▸ Dialogue | when: a voice you care about; checking what one click did | see: audio.cleanup-voice

**[Standard]** process

The order most audio editors follow for a voice-over or interview, written out so you can do it
yourself and so Revolution's one click can be checked against it. Every step is
an ordinary Essential Sound control (`{ui:essentialSound.type.Dialogue}` first), so you can also do
the same with separate effects in Effect Controls.

1. **Listen first, on headphones.** Note what is wrong: rumble (traffic, handling, a desk), steady
   noise (fan, hiss), changing noise (traffic, keyboard, wind), hum, harsh "s" sounds, a voice that
   jumps between quiet and loud. Fix only what you hear.
2. **Remove rumble.** Reduce Rumble (`{ui:essentialSound.repair.rumble.on}`) at about 3 (66 Hz);
   go to 4 (78 Hz) if the low end still booms. A voice has nothing useful below ~70 Hz, so this is
   safe on almost every voice.
3. **Reduce the noise.** Compare the noise in the pauses with the voice:
   - noise you only hear in the pauses (more than ~40 dB under the voice): leave it;
   - quiet steady noise: Reduce Noise (`{ui:essentialSound.repair.noise.on}`) at 3–5;
   - loud or changing noise: Voice Isolation (`{ui:essentialSound.repair.isolate.on}`) at 7–10.
   Listen to the voice itself while you raise it; stop where the voice starts to sound processed.
4. **Hum** (only if you hear a steady low tone): DeHum at the mains frequency (50 Hz in most of
   the world, 60 Hz in the Americas).
5. **De-ess** if "s" and "t" sounds are sharp: DeEss at 3–6.
6. **Even out the level.** Dynamics (`{ui:essentialSound.clarity.dynamics.on}`) at 2–3 for a
   steady speaker, 4–6 for one who moves away from the mic or gets loud.
7. **Shape the tone** (optional, by taste): EQ with a preset such as Balanced Voice.
8. **Loudness last.** In Loudness, Auto-Match (`{ui:essentialSound.autoMatch}`) to your delivery
   target and turn on Limit True Peak (`{ui:essentialSound.loudness.limit}`) with the ceiling at
   −1 dBTP. For YouTube and most streaming services the target is −14 LUFS; broadcast usually
   wants −23 LUFS (EBU R128) or −24 LKFS (ATSC A/85).

Order matters: noise reduction works best on the raw voice, compression after it doesn't pump the
noise back up, and loudness comes last because every step before changes the level.

## Clean Up Voice (one click)

> id: audio.cleanup-voice | does: one click measures a voice clip and sets the Essential Sound controls and loudness | ui: essentialSound.cleanupVoice | cmd: audio.cleanupVoice, jobs.cancel | resolve: Voice Isolation + Dialogue Leveler (closest) | premiere: Enhance Speech + Auto-Match (closest) | when: many voice clips; a fast start before tuning by ear | see: audio.manual-voice-cleanup, audio.voice-isolation

**[Fork]**

The quickest decent start for one voice clip or many: select the clips and click **Clean Up
Voice** (`{ui:essentialSound.cleanupVoice}`, at the top of Essential Sound ▸ Repair; command
`audio.cleanupVoice`). It measures each clip and sets the clean-up controls and the loudness for
it; then listen and tune by ear the one or two sliders that need it.

It does the manual process above for you as a starting point: it measures the clip (noise floor
against the voice level, energy below 80 Hz, how sharp the "s" sounds are, how much the level
varies; with the Voice Isolation model installed it also lets the model measure how much of the
sound is noise, which catches key clicks and wind gusts between words) and sets the same visible
controls: Reduce Rumble, Voice Isolation (when its model is
installed, else Reduce Noise), DeEss, Dynamics, and Loudness matched to −14 LUFS with the true peak
held under −1 dBTP. It is one undo step, and every slider it sets stays yours to change.

It listens to the whole clip several times (to measure, then to check the loudness it lands on),
so it runs in the background: keep editing, and the settings appear when it finishes (a few
seconds per minute of audio with the model). It shows in the job list, where you can stop it;
a stopped clean-up changes nothing.

| Option | Good for | Trade-offs |
|---|---|---|
| **Clean Up Voice** | A fast, consistent start on many clips; beginners | Leaves hum, reverb and EQ alone (listen for those yourself); it decides from measurements, not from listening |
| **The manual process** | A voice you care about, unusual noise, a particular sound | Takes a few minutes per clip; results depend on your ears and headphones |
| **Both** | Most work: click, then listen and adjust the one or two sliders that need it | — |

- **Beginner:** select the voice clip, click Clean Up Voice, listen. If the voice sounds
  processed, lower Voice Isolation or Reduce Noise one or two steps.
- **Pro:** use it for the loudness and true-peak work on many clips at once (select them all; each
  is matched on its own), and set restoration by ear.

**How well it works:** on recorded speech mixed with fan, room, traffic, keyboard and wind noise
(0, 5 and 10 dB under the voice), the one click came out as good as the manual process above in
every case (within 0.1 dB on traffic, keyboard and wind, ahead on fan and room because it uses
Voice Isolation where the manual process uses Reduce Noise), and closer to −14 LUFS. That was
with the Voice Isolation model installed; without it, check the result by ear more carefully.

For agents and the control channel: `audio.cleanupVoice` with `clips`, `target` (LUFS, default −14),
`ceiling` (dBTP, default −1), `isolate` (force Voice Isolation on or off), `wait: true` to run it
in place and get the report (otherwise it returns a `job` id; `jobs.list` / `jobs.cancel`), and
`apply: false` to get the suggested settings and measurements without changing anything.

## Recording a voice-over

> id: audio.voiceover-record | does: records narration from a microphone onto a track at the playhead, with pre-roll, countdown and punch-in | cmd: audio.voiceover.settings, audio.voiceover.start, audio.voiceover.stop | resolve: Fairlight ADR / record on an armed track | premiere: Voice-over Record | when: narration, voice-over, pickups | see: audio.voiceover-takes, audio.cleanup-voice

**[Revolution]**

1. Pick the microphone and its input channel in **Voice-Over Record Settings**
   (`audio.voiceover.settings`), with the file name to use (default "Voice-over"), the countdown
   beeps (on) and the pre-roll and post-roll (2 seconds each).
2. Park the playhead where the voice should start, or set In and Out to **punch in**: recording
   then covers only that range.
3. Click the record button in the audio track's header (`audio.voiceover.start`). Playback starts
   the pre-roll earlier so you can hear the lead-in; the beeps count down to the record point.
4. Stop (`audio.voiceover.stop`, or reaching the Out point). The recording is written as a mono
   WAV file (Project Settings ▸ Scratch Disks, else next to the project), imported and placed on
   the track: one undo step. The pre-roll isn't kept.

| Option | Good for | Trade-offs |
|---|---|---|
| **Record to picture in Revolution** | Timing the voice to the cut as you watch | Computer fans and room noise get in; clean up afterwards ([Clean Up Voice](#clean-up-voice-one-click)) |
| **Record elsewhere, import** | A quiet booth, a better recorder | No timing to picture while recording |
| **Punch-in over In / Out** | Replacing one line | Matching the voice's level and room to the takes around it |

## Reviewing voice-over takes

> id: audio.voiceover-takes | does: keeps every voice-over recording as a take and swaps takes in place | cmd: audio.voiceover.takes, audio.voiceover.pickTake | resolve: Take Selector (closest) | premiere: — | when: re-recording a line until it sounds right | see: audio.cleanup-voice

**[Fork]**

Takes list in Essential Sound; commands `audio.voiceover.takes` and `audio.voiceover.pickTake`.

Every voice-over recording is kept. When you record again at the same point on the same track,
the new recording goes on the timeline and the earlier ones stay in the project as *takes* of that
point. Select the voice-over clip: Essential Sound shows **Takes** with each one's waveform and
length; **Use** puts that take in place, full length
from the record point, as one undo step. The clip's processing (Clean Up Voice or any Essential
Sound settings, effects, clip gain) moves to the take you pick, so you can clean up once and then
choose by ear.

| Option | Good for | Trade-offs |
|---|---|---|
| **Record again at the same point, pick in Takes** | Short lines, retakes until it sounds right | Picking places the whole take; a take you trimmed by hand comes back at full length |
| **Record each take on its own track, mute the others** | Comparing takes against the picture, cutting between takes word by word | Uses tracks; you mute and unmute by hand |
| **Record one long take, cut out the bad parts** | Long narration read straight through | More editing afterwards; no list of takes |

- **Beginner:** record, listen, record again if you stumbled, then pick the best take in the list.
- **Pro:** for a line you'll assemble from several takes, record each on its own track and cut
  between them on the timeline; the Takes list is for whole-line choices.

