# Media, projects and clips

**[Revolution]** · importing, generated media, bins, saving and recovering projects, offline
media and relinking, the everyday clip operations and exchanging timelines with other apps;
modelled on Premiere Pro's Project panel and Clip menu; written for AI-PLAN AI3-PREP (feature
pages)

## Importing media

> id: media.import | does: brings files and folders into the project, including caption files and image sequences | cmd: file.import | resolve: Media page, Import Media | premiere: File ▸ Import, Media Browser | when: starting a project; adding footage, music, graphics | see: cards.insert, media.bins

**[Revolution]**

File ▸ **Import…** (`file.import`) takes files or whole folders. Caption files (`.srt`, `.vtt`,
`.scc`) become caption tracks ([text.md](text.md)); numbered stills can come in as one image
sequence (a preference). For a camera card, copy it first with the verified copy
([cards.md](cards.md)) and import from the copy.

| Option | Good for | Trade-offs |
|---|---|---|
| **Import a folder** | A shoot's whole card copy, organised as on disk | Brings everything, including takes you won't use |
| **Import chosen files** | Music, a logo, a few selects | More clicks for a big shoot |
| **Verified card copy, then import** | Camera originals you must not lose | One extra step, worth it before formatting the card |

## Generated media

> id: media.generated | does: makes bars and tone, black video, a colour matte, a counting leader, transparent video or an adjustment layer | cmd: file.newBarsAndTone, file.newBlackVideo, file.newColorMatte, file.newCountingLeader, file.newTransparentVideo, file.newAdjustmentLayer | resolve: Generators, adjustment clip | premiere: File ▸ New ▸ Bars and Tone, Color Matte, Adjustment Layer… | when: a test signal at the head of a master; a plain coloured background behind text; one effect over many clips | see: effects.apply

**[Revolution]**

File ▸ New makes items that need no file: **Bars and Tone** (a test signal),
**Black Video**, **Color Matte** (a flat colour; `project.matteColor` changes it later), a
**Universal Counting Leader**, **Transparent Video**, and an **Adjustment Layer**: a clip whose
effects apply to everything below it on the timeline, so one grade or blur covers many clips.

## Bins and project items

> id: media.bins | does: organises project items into bins, selects, moves and clears them | cmd: file.newBin, project.moveToBin, project.select, project.delete, project.setMarks | resolve: Media Pool bins | premiere: Project panel bins | when: keeping a big project findable | see: media.import

**[Revolution]**

New ▸ **Bin** (`file.newBin`) makes a folder in the Project panel; drag items in or use
`project.moveToBin`. **Clear** (`project.delete`) removes items from the project (the files stay
on disk). `project.setMarks` sets an item's In / Out for the Source monitor.

## Saving, auto-save and recovery

> id: media.saving | does: saves the project, keeps auto-saved copies and a journal, and recovers unsaved changes after a crash | cmd: file.save, file.saveAs, file.saveCopy, file.revert, file.autoSaveNow, file.recover, file.discardRecovery | resolve: Live Save, Project Backups | premiere: Auto Save, Revert | when: every session; after a crash or power cut | see: projects.where

**[Revolution]**

**Save** (`file.save`), **Save As…** (`file.saveAs`: the project continues under the new name)
and **Save a Copy…** (`file.saveCopy`: the copy is written, you keep working in the original).
**Revert** (`file.revert`) goes back to the last save.

Revolution also keeps a journal of unsaved changes and auto-saves on a timer (Settings ▸ Auto
Save; `file.autoSaveNow` saves one at once). If Revolution or the computer stops before you saved,
the next launch offers **Recover Unsaved Changes…** (`file.recover`), or throw them away with
`file.discardRecovery`. If the graphics card stops (for example out of memory), Revolution
restarts by itself and recovers them at once.

## Offline media and relinking

> id: media.relink | does: reconnects clips whose files moved, makes media offline on purpose and replaces a clip's file | cmd: media.linkMedia, media.relink, media.makeOffline, media.offlineAll, media.replaceFootage | resolve: Relink Selected Clips, Change Source Folder | premiere: Link Media, Make Offline, Replace Footage | when: files moved to another drive; swapping in a new version of a file | see: proxies.relink

**[Revolution]**

A clip whose file can't be found shows the offline slate. **Link Media…** (`media.linkMedia`;
agents use `media.relink`) points it at the file's new place; Revolution checks that the candidate is the same file (size and
a content fingerprint) before accepting it. **Make Offline** / **Offline All**
(`media.makeOffline`, `media.offlineAll`) disconnect media on purpose (to archive or move a
project). **Replace Footage…** (`media.replaceFootage`) swaps an item's file for another, keeping
every clip and its edits.

## Clip operations

> id: media.clips | does: speed and duration, frame holds, nesting, subclips, linking, replacing and syncing clips | cmd: clip.speedDuration, clip.setTimeInterpolation, clip.frameHold, clip.nest, clip.makeSubclip, clip.link, clip.group, clip.replaceFromBin, clip.synchronize, clip.mergeClips | resolve: Change Clip Speed, Freeze Frame, compound clips | premiere: Speed / Duration, Frame Hold, Nest, Synchronize | when: slow motion, a freeze, playing a clip backwards, hiding a clip without deleting it, syncing separate sound | see: editing.cutting

**[Revolution]**

- **Speed / Duration…** (`clip.speedDuration`): faster, slower or reversed; **Ripple** moves the
  following clips so nothing overlaps or leaves a gap. `clip.setTimeInterpolation` chooses how
  in-between frames are made: **frame sampling** (repeats frames, crisp but steppy), **frame
  blending** (mixes neighbours, smoother but soft) or **optical flow** (makes new frames from the
  motion, smoothest, can warp fast movement).
- **Frame Hold** (`clip.frameHold`): freezes the clip on a frame.
- **Nest…** (`clip.nest`): turns selected clips into one nested sequence (a clip of its own).
- **Make Subclip** (`clip.makeSubclip`): a new project item for part of a clip.
- **Link** / **Group** (`clip.link`, `clip.group`, `clip.ungroup`): picture and sound that move
  together, or several clips that select together.
- **Enable** (`clip.enable`) switches a clip off without deleting it; **Rename…** (`clip.rename`).
- **Replace with Clip** from the bin (`clip.replaceFromBin`) keeps the timeline clip's place and
  effects with new media.
- **Synchronize** (`clip.synchronize`) lines clips up by their In or Out points, timecode, a
  marker or their audio;
  **Merge Clips** (`clip.mergeClips`) joins a picture and a separately recorded sound into one
  item.
- **Scale to Frame Size** (`clip.scaleToFrameSize`) fits a clip of another size to the sequence;
  **Reveal in Project** (`clip.revealInProject`) shows its item.

## Scene Edit Detection

> id: media.scene-detect | does: finds the cuts inside a clip that is already edited and splits it there, makes subclips or adds markers | cmd: clip.sceneEditDetection | resolve: Scene Cut Detector | premiere: Scene Edit Detection | when: re-editing or grading a finished film that arrives as one file, shot by shot | see: media.clips

**[Revolution]**

Select the clip on the timeline and choose Clip ▸ **Scene Edit Detection…**
(`clip.sceneEditDetection`). It compares every frame with the one before, in the background, and
on each change of shot can **apply a cut** (the default), **create subclips** and / or **generate
markers**. **Sensitivity** (0–100, 50 by default) decides how big a change counts as a cut; shots
shorter than six frames are ignored. Each result is one undo step.

| Option | Good for | Trade-offs |
|---|---|---|
| **Apply cuts** | Grading a finished edit shot by shot (a whole film sent as one file) | Fades and dissolves may be found as several cuts, or none |
| **Markers only** | Checking what it found before cutting | One more step to cut |
| **Subclips** | Building a library of shots from a long file | Many new items in the project |

## Audio channels of a clip

> id: media.audio-channels | does: chooses how a clip's audio channels are used, splits them into mono clips, sets clip gain | cmd: clip.audioChannels, clip.breakoutToMono, clip.extractAudio, clip.audioGain | resolve: Clip Attributes ▸ Audio | premiere: Modify ▸ Audio Channels, Breakout to Mono, Audio Gain | when: a lavalier on one channel and a camera mic on the other; a mono recording heard on one side only | see: mixing.clip-or-track

**[Revolution]**

**Modify ▸ Audio Channels…** (`clip.audioChannels`, Shift+G) maps a clip's source channels:
stereo, mono or picked channels. **Breakout to Mono** (`clip.breakoutToMono`) makes a mono clip
per channel, so a two-microphone recording becomes two clips you can level separately.
**Extract Audio** (`clip.extractAudio`) writes a clip's sound to its own file. **Audio Gain…**
(`clip.audioGain`) sets or adjusts a clip's gain, or normalizes its peak (each clip on its own, or
all selected clips together).

## Exchanging timelines with other apps

> id: media.interchange | does: exports a timeline as EDL, FCPXML or OpenTimelineIO, and imports AAF | cmd: file.exportEdl, file.exportFcpxml, file.exportOtio, file.importAaf | resolve: Export Timeline (EDL, FCPXML, OTIO, AAF) | premiere: Export EDL / AAF / Final Cut Pro XML | when: sending an edit to a colourist, a sound mix or another editor | see: export.why

**[Revolution]**

| Format | Good for | Trade-offs |
|---|---|---|
| **EDL** (`file.exportEdl`) | Simple conform: one video track of cuts | Loses effects, most tracks and long names |
| **FCPXML** (`file.exportFcpxml`) | Final Cut Pro and apps that read it, including Resolve | Effects don't carry over between apps |
| **OpenTimelineIO** (`file.exportOtio`) | Open pipelines and tools that read OTIO | Newer; support varies between apps |
| **AAF import** (`file.importAaf`) | Opening an edit or a sound session from Avid or Premiere | Only what AAF describes (cuts, clips, levels) |
