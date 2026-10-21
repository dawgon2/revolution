# The Edit and Sequence menus

**[Revolution]** · undo, the clipboard, selecting and finding, labels, project clean-up, sequence
settings and tracks, timeline helpers (snapping, linked selection, gaps, through edits, match
frame) and rendering; modelled on Premiere Pro's menus; written for AI-PLAN AI3-PREP (feature
pages)

## Undo and redo

> id: edit-menu.undo | does: steps back and forward through every change, one command at a time | cmd: edit.undo, edit.redo | resolve: Undo / Redo, History | premiere: Undo / Redo, History panel | when: something went wrong, or to compare before and after | see: edit-menu.clipboard

**[Revolution]**

Every command is one undo step (a slider drag is one step, not one per frame): **Undo**
(`edit.undo`, Cmd+Z) and **Redo** (`edit.redo`). The history keeps the last 200 steps and is
cleared when you open or make another project.

## Cut, copy, paste and attributes

> id: edit-menu.clipboard | does: cuts, copies and pastes clips, pastes with insert, copies and removes effects between clips | cmd: edit.cut, edit.copy, edit.paste, edit.pasteInsert, edit.pasteAttributes, edit.removeAttributes, edit.duplicate | resolve: Cut / Copy / Paste, Paste Attributes | premiere: Paste Insert, Paste Attributes, Remove Attributes | when: reusing clips; giving many clips the same effects; stripping effects off many clips at once | see: color.matching.copy-paste

**[Revolution]**

- **Paste** (`edit.paste`) drops the clips at the playhead on the targeted tracks, over what is
  there; **Paste Insert** (`edit.pasteInsert`) pushes later clips along to make room.
- **Paste Attributes…** (`edit.pasteAttributes`) gives the selected clips the copied clip's
  effects and settings: you choose which (Motion, Opacity, Time Remapping, Volume, Panner,
  effects). **Remove Attributes…** (`edit.removeAttributes`) takes the chosen ones off: select a bunch
  of clips first to strip their effects all at once.
- **Duplicate** (`edit.duplicate`) copies a project item in the Project panel.

## Selecting, finding and labels

> id: edit-menu.select | does: selects everything or everything matching, finds items by name, colours clips with labels | cmd: edit.selectAll, edit.deselectAll, edit.selectAllMatching, edit.find, edit.findNext, edit.label, edit.selectLabelGroup | resolve: Select All, clip colours | premiere: Select All Matching, Find, Label | when: acting on every use of one clip in the timeline; colour-coding clips by type (interviews, b-roll) | see: edit-menu.clipboard

**[Revolution]**

**Select All** / **Deselect All** (`edit.selectAll`, `edit.deselectAll`); **Select All
Matching** (`edit.selectAllMatching`) selects every timeline clip from the same media as the
selected one. **Find…** / **Find Next** (`edit.find`, `edit.findNext`) search names. **Label**
(`edit.label`, a colour per command such as `edit.label.green`) colours clips and items;
**Select Label Group** (`edit.selectLabelGroup`) selects everything with the same label.

## Cleaning up a project

> id: edit-menu.cleanup | does: removes unused items, merges duplicate items and opens a file in its own app | cmd: edit.removeUnused, edit.consolidateDuplicates, edit.editOriginal | resolve: Remove Unused Clips | premiere: Remove Unused, Consolidate Duplicates, Edit Original | when: before archiving, getting rid of clips you never used (your files stay on disk, nothing is deleted); a project full of copies of the same file; opening an image in its own editor | see: media.bins

**[Revolution]**

**Remove Unused** (`edit.removeUnused`) takes items no sequence uses out of the project (the
files stay on disk). **Consolidate Duplicates** (`edit.consolidateDuplicates`) merges items that
point at the same file. **Edit Original** (`edit.editOriginal`, Cmd+E) opens the item's file in
the app your desktop uses for it.

## Sequence settings and tracks

> id: edit-menu.sequence-settings | does: changes a sequence's size, frame rate and audio, uses presets, adds and deletes tracks | cmd: sequence.settings, sequence.presets, sequence.addTracks, sequence.deleteTrack, sequence.deleteTracks, sequence.open, sequence.close | resolve: Timeline Settings | premiere: Sequence Settings, sequence presets, Add / Delete Tracks | when: a new sequence for a delivery size; changing the timeline frame rate (25 to 24 fps); more tracks | see: getting-started.import-xm5

**[Revolution]**

**Sequence Settings…** (`sequence.settings`) sets frame size, frame rate, pixel aspect and audio
sample rate; **Sequence Presets** (`sequence.presets`) are ready-made sets.
**Add Tracks…** / **Delete Tracks…** (`sequence.addTracks`, `sequence.deleteTracks`) add or remove
several at once; `sequence.deleteTrack` removes one. Colour settings have their own dialog
([color/color-management.md](color/color-management.md)).

## Timeline helpers

> id: edit-menu.helpers | does: snapping, linked selection, gaps, through edits, match frame, subsequences, simplifying | cmd: sequence.snap, sequence.linkedSelection, sequence.closeGap, sequence.joinThroughEdits, sequence.matchFrame, sequence.makeSubsequence, sequence.simplify | resolve: Snapping, Linked Selection, Match Frame | premiere: Snap, Linked Selection, Close Gap, Match Frame, Simplify Sequence | when: everyday timeline work; selecting only the audio of a clip; finding the original source clip at the same frame | see: timeline.keys

**[Revolution]**

- **Snap** (`sequence.snap`): clips snap to edits, markers and the playhead while you drag.
- **Linked Selection** (`sequence.linkedSelection`): clicking a clip also selects its linked
  audio or video; turn it off to select only the audio (or only the video) of a clip. **Selection Follows Playhead** (`sequence.selectionFollowsPlayhead`) selects
  the clip under the playhead as you move it.
- **Gaps:** **Close Gap** (`sequence.closeGap`) removes a selected gap; jump to the next or
  previous gap in the sequence or in a track (`sequence.goToNextGap`, `goToPrevGap`,
  `…InTrack`).
- **Through edits** (a cut where the media continues, so nothing changes on screen): show them
  (`sequence.showThroughEdits`) and join them back (`sequence.joinThroughEdits`).
- **Match Frame** (`sequence.matchFrame`) opens the clip's source at the same frame in the
  Source monitor; **Reverse Match Frame** (`sequence.reverseMatchFrame`) finds the Source
  monitor's frame on the timeline. `sequence.revealNested` opens a nested sequence at the frame.
- **Make Subsequence** (`sequence.makeSubsequence`, Shift+U) makes a new sequence from the
  selected clips.
- **Simplify Sequence…** (`sequence.simplify`) makes a tidy copy: by default without disabled
  clips and empty tracks; optionally with gaps closed, clips moved down, effects or text removed.

## Rendering parts of the timeline

> id: edit-menu.rendering | does: renders the selection or the audio ahead, and deletes render files | cmd: sequence.renderSelection, sequence.renderAudio, sequence.renderInToOut, sequence.deleteRenderFiles, sequence.deleteRenderFilesInToOut, sequence.normalizeMixTrack | resolve: Render Cache ▸ Render in Place | premiere: Render Selection, Render Audio, Delete Render Files, Normalize Mix Track | when: a heavy part stutters and the automatic cache hasn't reached it | see: render-cache.why

**[Revolution]**

The render cache works on its own in the background ([render-cache.md](render-cache.md)); these
are for doing it now: **Render Selection** (`sequence.renderSelection`), **Render In to Out**
(`sequence.renderInToOut`), **Render Audio** (`sequence.renderAudio`), and **Delete Render
Files** (`sequence.deleteRenderFiles`; `…InToOut` for the marked range). **Normalize Mix
Track…** (`sequence.normalizeMixTrack`) sets the whole mix's peak to a level (0 dB by default).
