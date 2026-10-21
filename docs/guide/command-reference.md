# Command reference: the commands the other pages only hint at

**[Revolution]** · every command in Revolution has an id, and the control channel, the command line
and the MCP server run all of them by that id. The feature pages teach the workflows; this page
lists the remaining commands by family so each one is named, says what it does and which menu or
key reaches it. "Command only" means no button or menu item: use the control channel, MCP, or
a shortcut you assign yourself ([shortcuts.md](shortcuts.md)). On Linux and Windows `Cmd` in a
shortcut means `Ctrl`. Written for AI-PLAN AI3 (every command has a guide section).

## Opening, closing and creating projects and generated media

> id: command-reference.project-files | does: opens, closes and creates projects and the generated clips under File > New | cmd: file.open, file.newProject, file.close, file.closeProject, file.newBarsAndTone, file.newColorMatte | resolve: Project Manager open / new | premiere: File > New / Open | when: starting or switching projects, adding bars, black or a colour matte

**[Revolution]**

Use these when you start work, move between projects or need a plain generated clip (a
test-signal, a black frame, a solid colour) without importing anything.

1. **File ▸ Open Project…** (`file.open`, `Cmd+O`) opens a project file; **File ▸ New ▸ Project…**
   (`file.newProject`, `Cmd+Alt+N`) makes an empty one.
2. **File ▸ Close** (`file.close`, `Cmd+W`) closes the selected item (`item`);
   **File ▸ Close Project** (`file.closeProject`, `Cmd+Shift+W`) closes the project (`force` is an option).
3. `file.closeAllProjects` closes every open project (`force` is an option);
   `file.closeAllOtherProjects` keeps only the current one.
4. Generated clips sit under **File ▸ New**: **Bars and Tone…** (`file.newBarsAndTone`),
   **Black Video…** (`file.newBlackVideo`), **Color Matte…** (`file.newColorMatte`),
   **Universal Counting Leader…** (`file.newCountingLeader`) and **Transparent Video…**
   (`file.newTransparentVideo`). The first three take a length in seconds; the matte also a colour.
5. `file.templates` lists the project templates (command only). **File ▸ Import From ▸ Demo
   Footage** (`file.importDemoFootage`) adds a built-in generated scene (Ocean Sunset, Aurora, City
   Night, Dunes, Plasma or Forest) for trying things out.

If you know Premiere: these are the File ▸ New and Open entries by the same names. If you know
Resolve: there is no Project Manager step for generated clips; they are ordinary project items.

## Importing and browsing files

> id: command-reference.media-browser | does: browses folders and imports from the Media Browser, pastes media, and shows file properties | cmd: mediaBrowser.*, file.importFromMediaBrowser, edit.pasteMedia, file.mediaProperties, file.mediaPropertiesFile | resolve: Media Storage | premiere: Media Browser | when: finding footage on disk, importing a selection, checking a file before import

**[Revolution]**

The Media Browser looks at folders without bringing anything into the project, so you can check
a file first. The commands behind it:

1. `mediaBrowser.roots` lists the starting locations; `mediaBrowser.navigate` goes to a folder (or
   back, forward, up); `mediaBrowser.list` lists a folder, optionally by file type.
2. `mediaBrowser.select` picks files; `mediaBrowser.probe` reports one file's properties without
   importing; `mediaBrowser.openInSource` opens it in the Source monitor to look at it.
3. `mediaBrowser.import` brings the picked files into a bin (`bin`), and can treat a numbered run
   of stills as one clip (`imageSequence`). **File ▸ Import from Media Browser**
   (`file.importFromMediaBrowser`, `Cmd+Alt+I`) does it from the menu.
4. `mediaBrowser.favorite` adds a folder to the favourites (or removes it); `mediaBrowser.clearRecent`
   forgets the recent folders; `mediaBrowser.settings` sets the file-type filter, list or
   thumbnail view, columns and hover scrubbing.
5. `edit.pasteMedia` pastes file paths as media: into the project, or into the timeline when
   `place` is on (`insert` pushes later clips along).
6. **File ▸ Get Media File Properties for ▸ Selection…** (`file.mediaProperties`,
   `Cmd+Shift+H`) and **▸ File…** (`file.mediaPropertiesFile`, any path) show a file's properties.

## Project panel: selecting, sorting, views and the freeform layout

> id: command-reference.project-panel | does: selects, sorts and searches project items; sets the view, columns, view presets and freeform layout | cmd: project.*, markers.* | resolve: Media Pool | premiere: Project panel | when: scripting or fine-tuning how the Project panel looks and what is selected

**[Revolution]**

1. Read the project: `project.inspect` (whole project), `project.items` (a bin's rows,
   optionally `recursive`), `project.notes`, `project.view.get`; `captions.list` lists a caption track's captions; `state.inspect` reports the editor
   state (selection, tool, panels). All are queries and change nothing.
2. Select: `project.select` (a list of items), `project.selectAll` (a bin), `project.deselectAll`.
   `project.sort` sorts by a column; `project.searchBinItems` searches inside a bin;
   `project.renameBin` renames one.
3. Columns: `project.columns.list`, `project.columns.set` (the **Metadata Display** choice) and
   `project.columns.resize`.
4. View: `project.view.set` picks list, icon or freeform, the icon size, font size, preview area
   and thumbnails.
5. View presets (ten slots): `project.viewPreset.save` (overwrite the slot),
   `project.viewPreset.saveAs` (a new one), `project.viewPreset.restore`,
   `project.viewPreset.rename`, `project.viewPreset.delete` and `project.viewPreset.list`.
6. Freeform layout (clip cards you place yourself): `project.freeform.layout`,
   `project.freeform.move`, `project.freeform.resize`, `project.freeform.stack`,
   `project.freeform.unstack`, `project.freeform.alignToGrid`, `project.freeform.reset` and
   `project.freeform.options`.
7. Named arrangements of the cards: `project.freeform.saveArrangement`,
   `project.freeform.restoreArrangement`, `project.freeform.deleteArrangement` and
   `project.freeform.arrangements` (the list).

Differences: Resolve's Media Pool has bins and metadata views but no freeform board; the freeform
view follows Premiere's.

## Metadata, media status and relinking

> id: command-reference.media-status | does: reads and edits clip metadata, reports media status, finds and relinks missing files, handles proxies and caches | cmd: metadata.*, media.*, mediaCache.clean, clip.editOffline, clip.sourceSettings | resolve: Metadata panel, Relink Clips | premiere: Metadata, Link Media | when: files moved, a clip shows offline, you want a clip's fields or colour space

**[Revolution]**

1. `metadata.get` and `metadata.set` read and write a clip's fields (Name, Label, Description,
   Scene, Shot, Log Note, Comment, Tape Name, Client, Camera Angle…); `set` takes one `field` and
   `value` or a `fields` map.
2. `media.status` says whether a clip's file is online; `media.findMissing` lists everything
   offline; `media.colorInfo` gives a clip's detected colour space; `media.pixelAspects` and
   `media.proxyPresets` list the choices those settings offer.
3. To fix offline media: `media.search` looks for a file by name in a folder; `media.autoRelink`
   **Relink Moved Media** takes a `from` and `to`, or a `folder`;
   `media.refreshChanged` re-reads files that changed on disk. See [media.md](media.md) for the
   steps with the dialog.
4. Proxies: **Clip ▸ Proxy ▸ Detach Proxies** (`media.detachProxies`) drops the proxy link;
   **▸ Reconnect Full Resolution Media…** (`media.reconnectFullRes`) points the clip at its
   original again. Background proxy building is in [proxies.md](proxies.md).
5. `mediaCache.clean` (**Delete Media Cache Files**) deletes cache files; the open project's own files are kept unless you pass `all`.
6. **Clip ▸ Edit Offline…** (`clip.editOffline`) edits an offline clip's name, tape name, scene,
   shot and log note without the file; **Clip ▸ Source Settings…** (`clip.sourceSettings`) opens a
   camera clip's source settings.

## Clip menu: subclips, waveforms, time interpolation and replacing

> id: command-reference.clip-menu | does: edits subclips, builds waveforms, sets time interpolation, toggles multi-camera, replaces with match frame | cmd: clip.editSubclip, clip.generateAudioWaveform, clip.timeInterpolation.*, clip.multicamEnable, clip.replaceFromSourceMatchFrame, clip.restoreCaptionsFromSource | premiere: Clip menu | when: changing a speed-changed clip's smoothness, fixing a subclip's range

**[Revolution]**

1. **Clip ▸ Edit Subclip…** (`clip.editSubclip`) changes a subclip's start and end (ticks or
   frames); the other options are `restrictTrims` and `convertToMaster`.
2. **Clip ▸ Generate Audio Waveform** (`clip.generateAudioWaveform`) builds the waveform picture
   for chosen items so it shows at once.
3. When a clip plays at a speed other than 100 %, pick how the in-between frames are made under
   **Clip ▸ Video Options ▸ Time Interpolation**: **Frame Sampling**
   (`clip.timeInterpolation.frameSampling`, repeats the nearest frame: crisp but steppy),
   **Frame Blending** (`clip.timeInterpolation.frameBlending`, mixes neighbouring frames: smoother
   but ghostly) or **Optical Flow** (`clip.timeInterpolation.opticalFlow`, estimates motion: the
   smoothest, and the slowest to render).
4. **Clip ▸ Multi-Camera ▸ Enable** (`clip.multicamEnable`) turns a multi-camera sequence on a
   timeline clip on or off (`enabled`).
5. **Clip ▸ Replace With Clip ▸ From Source Monitor, Match Frame**
   (`clip.replaceFromSourceMatchFrame`) swaps the selected clip for the Source monitor clip,
   lining up the frame under the playhead.
6. **Clip ▸ Restore Captions from Source Clip** (`clip.restoreCaptionsFromSource`) brings back a
   clip's original captions after you edited them.

## Clip volume steps

> id: command-reference.volume-steps | does: raises or lowers the selected clips' volume in fixed steps from the keyboard | cmd: clip.volumeUpMany, clip.volumeDownMany, clip.nudgeVolumeDown1, clip.nudgeVolumeDown3 | premiere: Audio Gain nudge | when: quick level changes while listening

**[Revolution]**

1. Select clips on an audio track.
2. `clip.volumeUpMany` (`Shift+]`) and `clip.volumeDownMany` (`Shift+[`) change the clips' volume (**Increase / Decrease Clip Volume Many**); `clip.nudgeVolumeDown1` and `clip.nudgeVolumeDown3` lower it by 1 dB and 3 dB
   (no default shortcut; assign one in the shortcut editor).
3. Undo steps back.

These change the clip's own level. A track's level lives in the mixer (see
[mixing.md](mixing.md)).

## The audio mixer: setting, touching and recording automation

> id: command-reference.mixer-control | does: sets mixer values, touches and releases controls, writes automation passes and deletes submixes | cmd: mixer.*, clipMixer.* | premiere: Audio Track Mixer automation | when: scripting a fader move, or recording a mix pass

**[Revolution]**

The panel is explained in [mixing.md](mixing.md); these are the commands under its faders.

1. `mixer.setValue` sets a lane (`volume`, `pan`, `mute`, a send level or an effect parameter) on
   a strip (`A1`, a submix `S1`, `Mix`).
2. `mixer.touch` (**Touch Mixer Control**) takes a control at a `value` and `time`; `mixer.release`
   (**Release Mixer Control**) lets it go.
3. `mixer.recordStart` starts an automation pass at a time; `mixer.recordStop` (**Write
   Automation**) ends it.
4. `clipMixer.touch` and `clipMixer.release` do the same for the Clip Mixer's volume and pan.
5. `mixer.deleteSubmix` removes a submix track.

## Audio scrubbing, voice-over sync and narration

> id: command-reference.audio-tools | does: toggles audio while scrubbing, syncs voice-over capture and runs text-to-speech narration | cmd: audio.toggleScrubbing, audio.voiceover.sync, tts.render, tts.inspect | premiere: Audio During Scrubbing, Record Voice-over | when: hearing audio as you drag the playhead, or scripting narration

**[Revolution]**

1. `audio.toggleScrubbing` (`Shift+S`) turns audio on or off while you drag the playhead.
2. `audio.voiceover.sync` (**Sync Voice-over Capture**) takes an optional `time`; the recording steps are in [audio.md](audio.md).
3. `tts.render` (**Synthesize Narration**) turns `text` into narration with a voice, pitch and
   pace; `sample` makes a short preview, `wait` blocks until done. `tts.inspect` reports a
   narration clip's settings (see [text.md](text.md)).

## Voice-isolation model and the AI assistant

> id: command-reference.ai-model | does: downloads, cancels and removes the Voice Isolation model, and asks, stops and reads the assistant's answers | cmd: audio.voiceIsolate.download, audio.voiceIsolate.cancelDownload, audio.voiceIsolate.remove, ai.cancelDownload, coach.assist, coach.assistResult, coach.assistForget, coach.feedback, coach.shareExample | when: managing disk space for models, or driving the assistant from a script

**[Fork]**

Models are downloaded only when you say yes, and these commands are how.

1. `audio.voiceIsolate.download` fetches the Voice Isolation model (`wait` blocks until done);
   `audio.voiceIsolate.cancelDownload` stops it; `audio.voiceIsolate.remove` deletes it again.
2. `ai.cancelDownload` cancels an assistant (language) model download.
3. `coach.assist` asks the assistant a question (optionally the page, the selection, grade facts
   and a model). It returns an id at once; `coach.assistResult` collects the answer (`wait` to
   block); `coach.assistForget` stops and drops an answer you no longer want.
   `coach.feedback` rates an answer (👍 / 👎 and a correction) and `coach.shareExample` previews,
   saves or links an answer as an example (see color.coach.assistant-share).
4. `coach.seen` reports that an `element` is on screen; `color.spaces` lists
   colour spaces; `compare.get` reads the still comparison; `stills.list` lists saved stills.

All are command only, apart from the buttons the coach panel shows.

## Introspection and diagnostics

> id: command-reference.introspection | does: lists commands, history, events, fonts, LUTs and presets, and reports system compatibility | cmd: command.list, history.list, events.list, events.clear, help.systemReport, fonts.list, lut.list, presets.list | when: finding out what the app can do or why something failed

**[Revolution]**

All command only, and all read-only except `events.clear`.

1. `command.list` lists every command with its label, menu, shortcut and parameters (the MCP
   `command_list` tool is the same list). `history.list` lists the undo history.
2. `events.list` lists events (filter by `level`, or `since` an id); `events.clear` empties it.
3. `help.systemReport` (**System Compatibility Report**) is the system compatibility report; see
   [hardware.md](hardware.md) for what the hardware paths mean.
4. `fonts.list` lists fonts (`system` chooses whether the system font folders are scanned); `lut.list` lists
   LUTs; `presets.list` lists effect presets.

## Preferences and keyboard shortcuts as commands

> id: command-reference.prefs-shortcuts | does: reads, sets and resets settings, and queries and resolves keyboard shortcuts | cmd: prefs.get, prefs.set, prefs.reset, prefs.schema, shortcuts.list, shortcuts.resolve, shortcuts.audit, shortcuts.deletePreset | resolve: Preferences | premiere: Settings, Keyboard Shortcuts | when: changing a setting from a script, finding which command a key triggers

**[Revolution]**

1. `prefs.schema` lists every setting with its type and allowed values (by `category`);
   `prefs.get` reads one `key` or all; `prefs.set` writes a `key` / `value` or a `values` map;
   `prefs.reset` returns a category to its defaults. Same settings as the Settings window.
2. `shortcuts.list` lists keys (filter by `query`, `panel`, `assigned`, and `platform`);
   `shortcuts.resolve` says what command a key combination runs in a panel;
   `shortcuts.audit` lists the keys adopted by the shortcut audit (Premiere defaults for commands that had none); `shortcuts.deletePreset`
   removes a saved preset. The preset and editor steps are in [shortcuts.md](shortcuts.md).

## Replacing fonts, shapes and graphics lists

> id: command-reference.graphics-misc | does: replaces fonts across projects, adds shape layers, lists graphics and templates, resets and selects layers | cmd: file.replaceFonts, graphics.newShape, graphics.list, graphics.template.list, graphics.template.thumbnail, file.exportGraphicsTemplate, graphics.resetAllParameters, graphics.resetDuration, graphics.selectNextGraphic, graphics.selectPreviousGraphic, graphics.selectPreviousLayer | premiere: Graphics and Titles menu | when: a font is missing, adding a rectangle or ellipse, resetting a graphic

**[Revolution]**

1. **Graphics and Titles ▸ Replace Fonts in Projects…** (`file.replaceFonts`) swaps one font
   (`from`) for another (`to`) wherever it is used.
2. `graphics.newShape` adds a shape (`rectangle`, `ellipse`, `polygon` or `path`) with a position,
   size and duration, to a new graphic or to `clip`.
3. `graphics.list` lists a graphic's layers; `graphics.template.list` lists templates (search,
   category, built-in or user); `graphics.template.thumbnail` renders one to a small PNG;
   **File ▸ Export ▸ Motion Graphics Template…** (`file.exportGraphicsTemplate`) saves the
   current graphic as a template.
4. **Graphics and Titles ▸ Reset All Parameters** (`graphics.resetAllParameters`) resets the
   selected layers (or all) and **Reset Duration** (`graphics.resetDuration`) returns the clip to
   the default length, limited by the next clip.
5. **Graphics and Titles ▸ Select ▸ Select Next Graphic** (`graphics.selectNextGraphic`),
   **Select Previous Graphic** (`graphics.selectPreviousGraphic`) and **Select Previous Layer**
   (`graphics.selectPreviousLayer`, `Cmd+Alt+[`) move the selection.

## Aligning, distributing and stacking graphic layers

> id: command-reference.graphics-layout | does: aligns layers to the frame or each other, distributes them evenly, and changes their stacking order | cmd: graphics.align, graphics.alignFrame.*, graphics.distribute, graphics.arrangeLayer, graphics.bringForward, graphics.bringToFront, graphics.sendBackward, graphics.sendToBack | premiere: Align and Distribute, Arrange | when: tidying a lower third, centring a title

**[Revolution]**

1. Select two or more layers (one is enough for aligning to the frame).
2. **Graphics and Titles ▸ Align to Video Frame** has **Right** (`graphics.alignFrame.right`),
   **Top** (`graphics.alignFrame.top`), **Bottom** (`graphics.alignFrame.bottom`),
   **Center Horizontally** (`graphics.alignFrame.hcenter`) and **Center Vertically**
   (`graphics.alignFrame.vcenter`).
   `graphics.align` is the general form: `align` left / hcenter / right / top / vcenter / bottom,
   `to` the frame, the group or the selection.
3. **Distribute** (three or more layers): **Distribute Vertically** (`graphics.distributeVertically`)
   spaces the centres evenly; **Distribute Space Horizontally**
   (`graphics.distributeSpaceHorizontally`) and **Distribute Space Vertically**
   (`graphics.distributeSpaceVertically`) make the gaps equal. `graphics.distribute` is the general
   form (`axis`, and `space` for equal gaps).
4. **Arrange**: **Bring Forward** (`graphics.bringForward`, `Cmd+]`), **Bring to Front**
   (`graphics.bringToFront`, `Cmd+Shift+]`), **Send Backward** (`graphics.sendBackward`, `Cmd+[`),
   **Send to Back** (`graphics.sendToBack`, `Cmd+Shift+[`); `graphics.arrangeLayer` takes the
   target (`front`, `back`, `forward`, `backward` or an index).

## Graphic text and nudge keys

> id: command-reference.graphics-keys | does: aligns text, steps font size and leading, and nudges the selected object by keyboard | cmd: graphics.alignTextCenter, graphics.alignTextRight, graphics.fontSizeDown, graphics.fontSizeDown5, graphics.fontSizeUp5, graphics.leadingDown, graphics.leadingDown5, graphics.leadingUp5, graphics.nudgeUp, graphics.nudgeDown, graphics.nudgeRight | premiere: text keyboard shortcuts | when: fine-tuning type without the mouse

**[Revolution]**

1. With a text layer selected: `graphics.alignTextCenter` (`Cmd+Shift+C`) and
   `graphics.alignTextRight` (`Cmd+Shift+R`) set the paragraph alignment.
2. Font size: `graphics.fontSizeDown` (`Cmd+Alt+Left`, one unit), `graphics.fontSizeDown5`
   (`Cmd+Alt+Shift+Left`) and `graphics.fontSizeUp5` (`Cmd+Alt+Shift+Right`) step it.
3. Leading (line spacing): `graphics.leadingDown` (`Alt+Down`), `graphics.leadingDown5`
   (`Alt+Shift+Down`) and `graphics.leadingUp5` (`Alt+Shift+Up`).
4. Nudge the selected object by one: `graphics.nudgeUp`, `graphics.nudgeDown`,
   `graphics.nudgeRight`; by five: `graphics.nudgeUp5`, `graphics.nudgeDown5`,
   `graphics.nudgeLeft5`. The other directions and steps are in [graphics.md](graphics.md).

The shortcuts above are the registered defaults.

## Lumetri sections, preset lists and LUT files

> id: command-reference.lumetri-lists | does: switches Lumetri sections on or off, lists presets with thumbnails, and exports or removes LUTs | cmd: lumetri.setSection, lumetri.presets, lumetri.presetThumbnails, lut.export, lut.remove | premiere: Lumetri Color panel section toggles | when: scripting a look, saving a LUT to share

**[Revolution]**

1. `lumetri.setSection` switches one of Basic, Creative, Curves, Wheels, HSL or Vignette on or
   off on a clip (leave `on` out to toggle) without touching its settings.
2. `lumetri.presets` lists look presets (in a `folder` if given); `lumetri.presetThumbnails` renders
   a contact sheet of them (`width`, `columns`, optional `path`).
3. `lut.export` writes a library or built-in LUT to a file as `cube` or `3dl`; `lut.remove`
   deletes a LUT from the library. LUT use in the node grader is in
   [color/finishing.md](color/finishing.md).

## Export presets, queue, sound presets and compositions

> id: command-reference.export-lists | does: reads and deletes export presets, lists the export queue, deletes sound presets and makes a new comp | cmd: export.presets.get, export.presets.delete, export.queue.list, essentialSound.deletePreset, comp.new | when: scripting deliveries, cleaning old presets

**[Revolution]**

1. `export.presets.get` returns one export preset's settings; `export.presets.delete` removes
   it. `export.queue.list` lists queued exports (see [export-queue.md](export-queue.md)).
2. `essentialSound.deletePreset` deletes a saved Essential Sound preset by name (and audio type).
3. `comp.new` creates a compositor comp with a name, size, frame rate and duration (see
   [compositor.md](compositor.md)).

## Playhead movement and markers

> id: command-reference.playhead-markers | does: moves the playhead to the ends, edit points and clip edges, steps five frames, and sets split-edit marks | cmd: playhead.*, markers.* | premiere: Go to, Mark Split | when: precise navigation, setting a J / L cut's split points

**[Revolution]**

1. Jump: `playhead.start` (`Home`) and `playhead.end` (`End`); `playhead.prevEdit` (`Up`) to the
   previous edit point, `playhead.nextEditAnyTrack` (`Shift+Down`) and
   `playhead.prevEditAnyTrack` (`Shift+Up`) on any track; `playhead.selectedClipStart`
   (`Shift+Home`) and `playhead.selectedClipEnd` (`Shift+End`).
2. Step: `playhead.stepBack5` (`Shift+Left`) and `playhead.stepForward5` (`Shift+Right`) move five
   frames.
3. Markers (`target` is the program or the source monitor): **Markers ▸ Go to Previous Marker**
   (`markers.goPrev`, `Cmd+Shift+M`), **Go to Out** (`markers.goToOut`, `Shift+O`) and **Clear
   Out** (`markers.clearOut`, `Cmd+Shift+O`).
4. Split edits (a different In for picture and sound): **Markers ▸ Mark Split** has **Video In**
   (`markers.markSplitVideoIn`), **Video Out** (`markers.markSplitVideoOut`) and **Audio Out**
   (`markers.markSplitAudioOut`); **Markers ▸ Go to Split** has `markers.goToSplitVideoIn`,
   `markers.goToSplitVideoOut`, `markers.goToSplitAudioIn` and `markers.goToSplitAudioOut`.

## Placing, moving and trimming by command

> id: command-reference.timeline-edits | does: places, moves, razors, trims, rolls, slips, slides and rate-stretches clips by exact amount | cmd: timeline.place, timeline.move, timeline.razor, timeline.trim, timeline.roll, timeline.slip, timeline.slide, timeline.rateStretch, timeline.select, timeline.setTrack | resolve: Trim mode | premiere: Trim tools | when: exact edits from a script or a repeatable recipe

**[Revolution]**

These are the commands the mouse tools use. They take exact times, so they suit scripts and
checks. All are command only.

1. `timeline.select` selects clips (`add` to add, `toggle` to flip). `timeline.place` puts a
   project item on a track at a time (`insert` pushes later clips along).
2. `timeline.move` moves clips to a track and time (`insert`, `linked`, `copy`). `timeline.razor`
   cuts at a time on one track or clip.
3. `timeline.trim` moves a clip's `in` or `out` edge, `regular` or `ripple`; `timeline.roll` moves
   the cut between two neighbours; `timeline.rateStretch` changes a clip's speed by dragging an
   edge.
4. `timeline.slip` changes the part of the source a clip shows; `timeline.slide` moves a clip and
   trims its neighbours.
5. `timeline.setTrack` sets a track's lock, sync lock, enabled, mute, solo, name, volume and pan.
6. **Sequence ▸ Apply Default Transitions to Selection** (`trim.applyDefaultTransition`) adds the
   default transition at the selected cuts; `trim.tick` (**Advance Trim Playback**) takes a `clock` in seconds.

## Keyboard nudging, slipping and selecting

> id: command-reference.timeline-keys | does: the remaining nudge, slip and slide steps and the previous-clip selection key | cmd: timeline.nudgeDown, timeline.nudgeLeft5, timeline.nudgeRight, timeline.nudgeRight5, timeline.slideLeft5, timeline.slideRight, timeline.slideRight5, timeline.slipLeft5, timeline.slipRight, timeline.slipRight5, timeline.selectPrevClip | premiere: Nudge, Slip, Slide | when: frame moves by keyboard | see: timeline.keys

**[Revolution]**

[timeline.md](timeline.md) explains nudging, slipping and sliding. The exact ids:

1. Nudge: `timeline.nudgeRight` (one frame right), `timeline.nudgeLeft5` and
   `timeline.nudgeRight5` (five frames), `timeline.nudgeDown` (to the track below).
2. Slip: `timeline.slipRight`, `timeline.slipLeft5`, `timeline.slipRight5`.
3. Slide: `timeline.slideRight`, `timeline.slideLeft5`, `timeline.slideRight5`.
4. Select the previous clip: `timeline.selectPrevClip` (`Cmd+Up`).

## Track targets by track number

> id: command-reference.timeline-targets | does: toggles targeting and source patching for each track number and for all tracks, and shifts targets up or down | cmd: timeline.toggleTarget*, timeline.toggleAll*, timeline.moveVideoTargetsUp, timeline.moveAudioTargetsUp | premiere: Toggle Target Video / Audio N | when: switching targets from the keyboard | see: timeline.targeting

**[Revolution]**

The idea of targets and source patches is in [timeline.md](timeline.md). One command exists per
track number so each can have its own key:

1. Video: `timeline.toggleTargetV1`, `timeline.toggleTargetV2`, `timeline.toggleTargetV3`,
   `timeline.toggleTargetV4`, `timeline.toggleTargetV5`, `timeline.toggleTargetV6`,
   `timeline.toggleTargetV7`, `timeline.toggleTargetV8`.
2. Audio: `timeline.toggleTargetA1`, `timeline.toggleTargetA2`, `timeline.toggleTargetA3`,
   `timeline.toggleTargetA4`, `timeline.toggleTargetA5`, `timeline.toggleTargetA6`,
   `timeline.toggleTargetA7`, `timeline.toggleTargetA8`.
3. All at once: `timeline.toggleAllVideoTargets` (`Cmd+0`), `timeline.toggleAllAudioTargets`
   (`Cmd+9`), `timeline.toggleAllSourceVideo` (`Cmd+Alt+0`), `timeline.toggleAllSourceAudio`
   (`Cmd+Alt+9`).
4. Shift every target a track: `timeline.moveVideoTargetsUp`, `timeline.moveVideoTargetsDown`,
   `timeline.moveAudioTargetsUp`, `timeline.moveAudioTargetsDown`.
5. `timeline.toggleSoloTargetedAudio` solos and `timeline.toggleOutputTargetedVideo` hides or
   shows every targeted track.

## Multi-camera: selecting, cutting and the grid

> id: command-reference.multicam | does: switches or cuts to a camera by number, pages through cameras, and shows the multi-camera grid and preview | cmd: multicam.cutToCamera, multicam.selectCamera2, multicam.grid, multicam.inspect, multicam.nextPage, multicam.prevPage, multicam.recordStop, multicam.selectionTopDown, multicam.showPreviewMonitor, multicam.transmitView | resolve: multicam angle switching | premiere: Multi-Camera | when: cutting a multi-camera interview live while it plays | see: trimming.multicam

**[Revolution]**

The workflow is in [trimming.md](trimming.md). Use these when you want a key per camera or a script:

1. **Cut to Camera** by number. `multicam.cutToCamera` takes `camera` 1 to 16 (or a 0-based
   `angle`), an optional `time`, and `videoOnly`. For cameras 2 to 9 there is one command each,
   which adds an edit at the playhead and shows that camera from there: `multicam.cutToCamera2`
   (`Ctrl+2`), `multicam.cutToCamera3` (`Ctrl+3`), `multicam.cutToCamera4` (`Ctrl+4`),
   `multicam.cutToCamera5` (`Ctrl+5`), `multicam.cutToCamera6` (`Ctrl+6`),
   `multicam.cutToCamera7` (`Ctrl+7`), `multicam.cutToCamera8` (`Ctrl+8`) and
   `multicam.cutToCamera9` (`Ctrl+9`).
2. **Select Camera**: while recording, the number keys cut to that camera; when stopped they
   switch the clip at the playhead. The commands for cameras 2 to 9 are `multicam.selectCamera2`
   (`2`), `multicam.selectCamera3` (`3`), `multicam.selectCamera4` (`4`),
   `multicam.selectCamera5` (`5`), `multicam.selectCamera6` (`6`), `multicam.selectCamera7` (`7`),
   `multicam.selectCamera8` (`8`) and `multicam.selectCamera9` (`9`); each takes `videoOnly`.
3. `multicam.grid` returns the grid page at the playhead (layout, pages, and each cell's camera,
   angle, name and whether it is active). `multicam.nextPage` and `multicam.prevPage` turn the
   page when the angles do not all fit; `multicam.inspect` reports the multi-camera clip at the
   playhead, its cameras and the recorder state. All three are command only.
4. `multicam.recordStop` ends a live switching pass (a pass is one undo step).
   `multicam.selectionTopDown` makes switching act on the topmost of stacked multi-camera clips
   instead of the lowest. `multicam.showPreviewMonitor` shows the program next to the grid (off:
   the grid fills the monitor). `multicam.transmitView` stores the setting to send the grid to a
   transmit device; there is no transmit output yet.

## Branching a grade by command

> id: command-reference.grade-graph | does: branches a clip grade (parallel, layer, outside, splitter, key-mixer nodes), connects ports, picks the output | cmd: grade.addParallel, grade.addLayer, grade.addOutside, grade.addSplitter, grade.addKeyMixer, grade.connect, grade.disconnect, grade.setOutput | resolve: parallel, layer and outside nodes | when: scripting a branched grade

**[Resolve-style]** **[Fork]**

1. `grade.addParallel`, `grade.addLayer`, `grade.addOutside`, `grade.addSplitter` and
   `grade.addKeyMixer` branch from `node` (default: the current node) and return the new ids.
2. `grade.connect` joins `from` / `fromPort` to `to` / `toPort` (node 0 is the grade's input);
   `grade.disconnect` frees `to` / `toPort`; `grade.setOutput` picks the output `node`. A loop
   or a graph with no single output is refused and nothing changes. Each is one undo step.
