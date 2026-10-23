# Mixing audio

**[Revolution]** · the Audio Track Mixer and the Audio Clip Mixer: levels, pan, track effects,
submixes and sends, and automation recorded while you play; modelled on Premiere Pro's mixers;
written for AI-PLAN AI3-PREP (feature pages)

## The Audio Track Mixer

> id: mixing.track-mixer | does: one strip per audio track, submix and the master Mix: fader, pan, mute, solo, record arm | cmd: mixer.inspect, mixer.setStrip, mixer.setValue | resolve: Fairlight mixer | premiere: Audio Track Mixer | when: balancing whole tracks rather than single clips; turning a whole track up or down | see: mixing.clip-or-track, mixing.automation

**[Revolution]**

Open Window ▸ **Audio Track Mixer** (on the Fairlight page in the Resolve workflow). Each audio
track (A1, A2…) has a strip, each submix (S1, S2…) has one, and the master **Mix** has the last
one. A strip has a fader (volume in dB), a pan knob, **Mute**, **Solo**, **Record arm** (for
voice-over recording) and its automation mode. `mixer.setStrip` sets these from a script
(`volumeDb`, `pan`, `muted`, `solo`, `mode`, its `output` and how its input channels are used);
`mixer.inspect` lists every strip.

## Clip volume or track volume?

> id: mixing.clip-or-track | does: when to set a level on the clip and when on the track | cmd: clipMixer.set, mixer.setValue | resolve: clip gain vs track fader | premiere: Audio Clip Mixer vs Audio Track Mixer | when: deciding where a level change belongs | see: mixing.track-mixer, mixing.clip-mixer

**[Revolution]**

| Option | Good for | Trade-offs |
|---|---|---|
| **Clip level** (Effect Controls ▸ Volume, the Audio Clip Mixer, Essential Sound's Clip Volume) | Evening out one clip against its neighbours; moves with the clip when you edit | Many clips to touch when a whole track is too loud |
| **Track level** (the strip's fader and its automation) | A whole layer: all the music, all the dialogue | Track automation stays in place on the timeline when clips move |
| **Both** | Most mixes: clips even with each other, tracks balanced against each other | Two places to look when something is too loud |

- **Beginner:** set clip levels so each track sounds even, then use the track faders for the
  balance between dialogue, music and effects.
- **Pro:** keep clip levels for corrections and track automation for the mix moves (music dips,
  swells), so re-cutting the picture doesn't undo the mix.

## Track effects (inserts)

> id: mixing.inserts | does: effects on a whole track, submix or the Mix, before or after its fader | cmd: mixer.addInsert, mixer.setInsert, mixer.removeInsert | resolve: Fairlight track effects | premiere: Audio Track Mixer effect slots | when: one EQ or compressor for a whole track; a limiter on the Mix | see: mixing.submixes

**[Revolution]**

Each strip has effect slots. `mixer.addInsert` puts an audio effect in a slot (`effect`, `slot`,
`postFader`), `mixer.setInsert` turns it on or off, moves it after the fader or sets its
parameters, `mixer.removeInsert` takes it out. Pre-fader effects see the track before its volume;
post-fader effects see it after.

To add one without opening the mixer's slot menu:

1. Open the **Effects** panel and find an audio effect (Audio Effects folder, or type its name in the search box).
2. Drag it onto an audio track's header in the timeline (left of the clips, `timeline.track.A1.fxDrop` while you
   hover) or onto a strip in the **Audio Track Mixer** (`mixer.A1.fxDrop`). The header or strip gets an outline.
3. Let go. The effect goes into the track's first free slot, pre-fader, as one undo step. If all slots are full, the
   status bar says so and nothing changes. Video effects, presets and transitions are ignored there.

Dropping it on a clip instead puts it on that clip only (Effect Controls).

To see and change a track's effects next to where you edit clips:

1. Click an empty part of an audio track's header in the timeline (right of the track name;
   `timeline.track.A1.header`). The header gets a thin outline and the clip selection is cleared.
2. **Effect Controls** (Premiere workflow) or the **Inspector ▸ Audio** tab (Resolve workflow) now shows
   "Track · A1 · …" with each track effect: **On** (untick to bypass), **Post-Fader**, **Edit…** for the
   EQs and dynamics, **Remove**, and every parameter. Dragging a value is one undo step.
3. You can also drop an audio effect on this view to add it to the track.
4. Click a clip to get the clip's controls back.

This differs from Premiere, where track effects live only in the Audio Track Mixer, and from Resolve, where
they are added from the Mixer; here the mixer slots still work the same way.

| Option | Good for | Trade-offs |
|---|---|---|
| **Clip effects** (Effect Controls, Essential Sound) | Fixing one recording: noise, hum, a harsh voice | One instance per clip |
| **Track inserts** | The same treatment for everything on a track; a limiter on the Mix for delivery | Applies to every clip on the track, even ones that don't need it |

- **Beginner:** fix individual recordings with clip effects; add a track effect only when every clip
  on the track wants it (one EQ for one microphone).
- **Pro:** keep one voice per track so its track inserts suit that voice, and share reverb or
  delay through a bus with sends instead of one copy per track.

## Submixes and sends

> id: mixing.submixes | does: groups tracks into a submix and sends part of a track to a submix | cmd: mixer.addSubmix, mixer.deleteSubmix, mixer.addSend, mixer.setSend, mixer.removeSend, mixer.setStrip | resolve: Fairlight buses | premiere: submix tracks and sends | when: one fader for all dialogue tracks; one reverb shared by several tracks | see: mixing.inserts

**[Revolution]**

A **submix** is a strip without clips that takes the sound of the tracks routed to it: one fader
that controls several tracks at once (all the dialogue tracks, say).

1. **Add Audio Submix Track** (Sequence menu, `mixer.addSubmix`): S1 appears in the mixer.
2. Route tracks to it: set a strip's **output** to S1 (`mixer.setStrip` with `output`). Its
   sound now goes through S1, then to the Mix. A submix can feed a later submix, never an earlier
   one (no feedback loops).
3. Or keep the track on the Mix and add a **send** (`mixer.addSend`): a copy of the track at the
   send's level goes to the submix as well (`mixer.setSend` sets the level, `mixer.removeSend`
   removes it). Put a reverb insert on the submix and send several tracks to it for one shared
   room.

From the timeline: right-click an audio track's header ▸ **Send to** ▸ a submix
(`timeline.track.A1.sendTo.<id>`), or **New Bus** (Resolve workflow) / **New Submix** (Premiere workflow) to
add a submix and send to it. Submixes the track sends to are greyed out. Sends start post-fader at 0 dB.

## Automation: recording mixer moves while playing

> id: mixing.automation | does: records fader, pan, mute, send and effect moves while the timeline plays, as track keyframes | cmd: mixer.recordStart, mixer.recordStop, mixer.touch, mixer.release, mixer.setStrip | resolve: Fairlight automation (Latch, Touch, Write) | premiere: track automation modes | when: riding music under dialogue, fades and swells by ear | see: mixing.keyframes

**[Revolution]**

Set a strip's automation mode, play, and move its controls: the moves are heard at once and
written as keyframes when playback stops (one undo step).

| Mode | Records | When you let go |
|---|---|---|
| **Off** | nothing, and ignores existing automation | — |
| **Read** (default) | nothing; plays the automation | the control follows the automation |
| **Latch** | from the first touch | keeps writing the last value until playback stops |
| **Touch** | only while you hold the control | glides back to the existing automation |
| **Write** | every control, from the start of playback | keeps writing the last value until playback stops |

After a Write pass the strip switches to Touch (Settings ▸ Audio ▸ "Switch to Touch after
Write"), so the next pass doesn't overwrite everything again. Recorded moves are thinned into a
few keyframes (Settings ▸ Audio sets how much).

- **Beginner:** use **Touch**: you only change what you touch.
- **Pro:** **Latch** for moves that should hold (a music level that stays down after the
  interview starts); **Write** for a first full pass over a scene.

## Editing track keyframes

> id: mixing.keyframes | does: adds, moves and deletes track automation keyframes, or writes a whole curve | cmd: mixer.setKeyframe, mixer.moveKeyframe, mixer.deleteKeyframe, mixer.clearLane, mixer.writeAutomation | resolve: Fairlight automation editing | premiere: track keyframes in the timeline | when: fixing a recorded move; drawing a fade exactly | see: mixing.automation

**[Revolution]**

Every automated control is a **lane** of its strip: `volume`, `pan`, `mute`,
`send.<n>.level` and `fx.<slot>.<parameter>`. `mixer.setKeyframe` adds or changes a keyframe at a
time, `mixer.moveKeyframe` moves one, `mixer.deleteKeyframe` removes one, `mixer.clearLane`
removes a lane's keyframes, and `mixer.writeAutomation` writes a list of (time, value) points in
one step (for agents and scripts).

## The Audio Clip Mixer

> id: mixing.clip-mixer | does: a fader and pan per track that write the clip's own Volume and Panner keyframes | cmd: clipMixer.set, clipMixer.setMode, clipMixer.touch, clipMixer.release | resolve: clip gain and clip keyframes | premiere: Audio Clip Mixer | when: riding one clip's level by ear; the level should move with the clip | see: mixing.clip-or-track

**[Revolution]**

The Audio Clip Mixer looks like the track mixer, but its controls belong to the **clip under the
playhead** on each track. With the same Off / Read / Latch / Touch / Write modes
(`clipMixer.setMode`), a pass writes keyframes on the clips' **Volume** and **Panner** effects, in
the clips' own time, so they travel with the clips when you edit. `clipMixer.set` sets one clip's
level or pan directly.

## Meters and clip lights

> id: mixing.meters | does: peak meters on mixer strips and Audio Meters, with peak hold and a clip light that stays red until clicked | cmd: mixer.inspect | resolve: Fairlight meters (clip indicators reset by clicking) | premiere: Audio Meters and the mixer's clip indicators | when: checking the mix stays under full scale while it plays | see: mixing.track-mixer, practice.audio.loudness-targets

**[Fork]**

Every strip in the Audio Track Mixer and Audio Clip Mixer has a meter, and Window ▸ **Audio
Meters** shows the master Mix with its loudness readout (M / S / I in LUFS, true peak in dBTP).
Each audio track header in the timeline also has a thin meter at its right edge
(`timeline.track.A1.meter`): the louder of the track's channels, with its own clip light, so you
can watch levels without opening a mixer.
The thin line above each bar is the **peak hold**: the loudest moment of the last seconds. The
small light on top of each bar is the **clip light**: it turns red when that strip reaches 0 dBFS
(digital full scale, the loudest level a file can hold) and **stays red** after the sound gets quieter,
so you can see that something clipped even if you looked away.

1. Play the part of the timeline you want to check.
2. If a clip light is red, find the loud spot: lower that track's fader or the clip's volume, or
   put a Limiter on the strip (see Track effects).
3. Click the meter (`mixer.A1.meter` on a strip, `timeline.track.A1.meter` on a header,
   `audioMeters.clip` on the Audio Meters panel) to
   turn the light off and drop the peak hold to the current level, then play the spot again.

Beginners: keep every light dark and the true peak under −1 dBTP for online video. Pros often let
a submix peak hot and catch it with a limiter on the Mix; what counts is the Mix's light and its
true peak. The clip light stays until you click it, whatever the workflow; it is one light per
strip in the Audio Meters panel (lit for every channel at once), not one per channel.
