# Colour management

**[Standard] [Fork]** · F-Log2 input, DaVinci Wide Gamut / Intermediate working space, Rec. 709 output

## Why

> id: color.color-management.why | does: why log footage needs its colour space known and a working space to grade in | resolve: DaVinci YRGB Color Managed | premiere: Color Management settings | when: before grading log or HDR footage | see: color.color-management.input

**[Standard]**

A log clip stores brightness with a curve that squeezes the sun and the shadows into a flat
picture, and its colours use a gamut (F-Gamut) wider than your screen. That is why you don't grade the log directly: grading numbers only mean
the same thing from clip to clip if all clips are first translated into one **working space**.
This fork uses **DaVinci Wide Gamut / DaVinci Intermediate** (Blackmagic's published
specification). Its signal has mid grey at about 0.336, which is why the Pivot slider defaults to
that value. Grade nodes work on this signal when the sequence's working space is DaVinci Wide
Gamut; in a Rec. 709 sequence they work on the Rec. 709 signal, in a Rec. 2100 sequence on the
PQ or HLG signal, so the same numbers look different in each.

## Tell Revolution what the clip is (input)

> id: color.color-management.input | does: sets a clip's input colour space, such as F-Log2 / F-Gamut | ui: colorDialog.space.* | cmd: clip.interpretFootage | resolve: Input Color Space per clip | premiere: Interpret Footage ▸ Color Management | when: a log clip looks grey or wrong | see: color.color-management.working

**[Revolution]**

Most files announce their colour space and need nothing. **Fujifilm clips are detected for you:**
the camera marks F-Log and F-Log2 video as ordinary BT.709 in the stream, but it also writes the
profile into its own metadata, and Revolution reads that on import (checked on the X-M5, 4:2:0
and 4:2:2). Interpret Footage then shows "Fujifilm F-Log2 / F-Gamut" (or F-Log) as the detected
space. Clips imported before this was added keep what they had; set them by hand as below.

If a log clip still looks grey and flat in the viewer (another camera, or a file that was
re-encoded and lost the camera's metadata), set its colour space explicitly:

1. In the Project panel select the clip (the footage item).
2. Choose Clip ▸ Modify ▸ **Interpret Footage…** (`clip.interpretFootage`). The window
   "Interpret Footage" opens.
3. Under "Override media colour space" pick **Fujifilm F-Log2 / F-Gamut** (id `flog2-fgamut`)
   {ui:colorDialog.space.flog2-fgamut}. "Use file metadata" {ui:colorDialog.space.auto} goes back
   to automatic. The line above shows what was detected from the file.
4. Press **OK** {ui:colorDialog.ok}. The clip should now look natural instead of grey.

Plain F-Log (not F-Log2) is `flog-fgamut`, "Fujifilm F-Log / F-Gamut".

## Choose the working space (sequence)

> id: color.color-management.working | does: picks the sequence's working space, DaVinci Wide Gamut / Intermediate for grading | ui: colorDialog.working.dwg-di, colorDialog.wideGamut, colorDialog.ok | cmd: sequence.colorSettings | resolve: Timeline Color Space | premiere: Sequence Color Management | when: setting up a sequence for grading | see: color.color-management.output

**[Revolution]** **[Fork]**

1. Choose Sequence ▸ **Color Management…** (`sequence.colorSettings`). The window "Sequence Color
   Management" opens.
2. Under "Working Color Space" pick **DaVinci Wide Gamut / Intermediate**
   {ui:colorDialog.working.dwg-di}. The other choices are Rec. 709 (the default), Rec. 2100 PQ
   and Rec. 2100 HLG.
3. The two checkboxes below matter in a Rec. 709 sequence: "Wide gamut color (composite in
   BT.2020)" {ui:colorDialog.wideGamut} and "Auto tone map media (HDR and log into SDR)"
   {ui:colorDialog.autoToneMap}, which folds HDR clips into the Rec. 709 picture instead of
   clipping them. In a DaVinci Wide Gamut sequence the log conversion happens regardless.
4. Press **OK** {ui:colorDialog.ok}.

Existing sequences keep their working space; only change it deliberately. Lumetri and the nodes
behave differently in different working spaces, as the same numbers do in Resolve with a
different timeline colour space.

## Output

> id: color.color-management.output | does: converts the working space to Rec. 709 at the end, with highlight roll-off | ui: colorDialog.autoToneMap | cmd: sequence.colorSettings | resolve: Output Color Space, tone mapping | premiere: Sequence output colour space | when: delivering SDR from a wide-gamut grade | see: color.color-management.working

**[Standard]** **[Fork]**

In a DaVinci Wide Gamut sequence the working space is converted to **Rec. 709** at the end,
with highlight roll-off (BT.2390 tone mapping) and colours outside Rec. 709 desaturated into
it, so very bright or very saturated values fold in gently instead of clipping. There is no
separate control yet: a per-grade highlight knob is planned for G2.

## If you know Resolve

> id: color.color-management.resolve | does: how this compares with Resolve's colour-managed setup | resolve: DaVinci YRGB Color Managed | when: you come from Resolve | see: color.color-management.working

**[Resolve-style]**

This matches Resolve's "DaVinci YRGB Color Managed" idea, but the setting lives in the sequence
and in Interpret Footage, not in a project-wide settings page. There is no per-clip input
transform node: the input transform is applied automatically before your first node.

## If you know Premiere

> id: color.color-management.premiere | does: how this compares with Premiere's Interpret Footage and sequence colour settings | premiere: Interpret Footage, Sequence Color Management | when: you come from Premiere | see: color.color-management.input

**[Revolution]**

Interpret Footage and Sequence Color Management are the same dialogs as upstream FilmCraft's
(**[Revolution]**); the DaVinci working space is the fork's addition (**[Fork]**).
