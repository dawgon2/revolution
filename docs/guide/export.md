# Export: presets, hardware H.265 and quality

**[Revolution] [Fork]** · turning a finished sequence into a file: picking a preset, how the
H.265 (HEVC) encoder is chosen and when it falls back, bitrate versus constant quality, and what
happens to HDR sequences

## Why

> id: export.why | does: exporting renders every frame with its grade and effects into a file | resolve: Deliver page | premiere: Export mode | when: the edit is finished | see: export.open

**[Revolution]**

Exporting is the last step of every edit: Revolution renders each frame of the sequence (with every
grade and effect) and hands it to an encoder that writes the file. The two choices that matter
most are **which preset** you start from (it sets the format, size and quality in one go) and
**how the size of the file is decided** (a target bitrate, or a constant quality). Everything
else has a sensible default. If you know Premiere, the Export page works the way its Export mode
does: settings on the left, a preview in the middle, a queue on the side.

## Open the Export page

> id: export.open | does: opens the Export page with the sequence's file name, location and format | ui: header.mode.export, export.fileName, export.location, export.format | cmd: file.exportMedia | resolve: Deliver page | premiere: Export mode | when: making a file of the sequence | see: export.presets

**[Revolution]**

1. Click **Export** in the header {ui:header.mode.export}, or choose File ▸ Export ▸
   **Media…** (`file.exportMedia`; Ctrl+M).
2. The settings column shows **File Name** {ui:export.fileName}, **Location**
   {ui:export.location}, **Preset** {ui:export.preset} and **Format** {ui:export.format},
   then the Video, Audio and Effects sections.
3. When the summary looks right, click **Export** {ui:export.button} to export now, or
   **Send to Queue** {ui:export.sendToQueue} to collect several exports and run them later
   with **Start Queue** {ui:export.queue.start} (`export.queue.start`).

## Choose a preset

> id: export.presets | does: chooses or saves a set of export settings for where the file is going | ui: export.preset, export.preset.more, presetManager.* | cmd: export.presets.list, export.presets.save | resolve: render presets | premiere: export presets, Preset Manager | when: YouTube, a master file, a quick review copy; saving export settings to reuse next time | see: export.hevc

**[Revolution] [Fork]**

A preset is a saved set of export settings. Start from the one closest to where the file is
going, then change only what you need (the Preset box then says "Custom"). The fork adds six
H.265 presets for 6.2K footage to upstream's list:

| Preset | What it is for |
|---|---|
| **YouTube 4K HEVC** | uploads to video sites: 3840×2160, Main 10, Rec. 709, about 80 Mbps |
| **6.2K Master HEVC** | a full-size master to keep: 6240×4160, Main 10, QuickTime, about 200 Mbps, uncompressed 24-bit audio |
| **HEVC 4:2:2 10-bit master** | a master that keeps full vertical colour detail (4:2:2) for grading or archiving: Main 4:2:2 10, QuickTime, about 240 Mbps; see Chroma below |
| **Archive HEVC (x265, quality)** | the smallest file at high quality when time doesn't matter: software x265, constant quality 18 |
| **Vertical 2160×3840 HEVC** | phone-shaped (9:16) uploads, the picture scaled to fill |
| **H.265 / HEVC 2160p 4K 10-bit** | a general 4K Main 10 file (upstream's list, kept) |

1. Open the **Preset** list {ui:export.preset} and pick one. Favourites are at the top.
2. To keep your own settings, open **More presets…** {ui:export.preset.more}, type a name in
   **Save current settings as** {ui:presetManager.saveName} and click **Save**
   {ui:presetManager.save} (`export.presets.save`). `export.presets.list` lists every preset.

## How H.265 is encoded, and when it falls back

> id: export.hevc | does: H.265 is encoded by the graphics card's video engine, falling back when it can't | ui: export.video.hevcEncoder, export.video.hwEncodeSpeed, export.video.exportEngines | cmd: export.lanes, export.lanesNote | resolve: Encoder: hardware / native | premiere: Hardware Encoding | when: export is slow or a setting is greyed out; a 4:2:0 or 4:2:2 master; an export that won't play on a client's Mac or Apple device (hvc1); playing a game while it exports | see: hardware.jobs

**[Fork]**

On this machine H.265 is normally encoded by the graphics card's **video engine** through
VA-API, inside Revolution, with the rendered pictures going straight into the encoder's memory
(see [hardware.md](hardware.md)). It is fast enough to export 6.2K at around 18–20 frames per
second. When it can't be used, the export doesn't fail: it moves to the next encoder.

The **Encoder** choice {ui:export.video.hevcEncoder} (Video section, only shown for H.265):

| Choice | What happens |
|---|---|
| **Auto (hardware if available)** | the video engine in Revolution; if it refuses the size or profile, or fails part-way, the system ffmpeg's VA-API encoder carries on, then software x265 |
| **Hardware (VA-API)** | the video engine only; the export stops with a message rather than fall back to software |
| **Software (x265)** | the system ffmpeg's x265 encoder: slower, often a little smaller at the same quality |

1. Leave **Auto** unless you have a reason. Pick **Hardware (VA-API)** when you'd rather know
   than wait for a slow software export; pick **Software (x265)** for archives.
2. After an export, `perf.stats` names the encoder that was actually used.

**[Fork] With an NVIDIA graphics card on Linux** (a laptop such as the XPS 16, or a desktop
card), **Auto** also uses NVIDIA's encoder (NVENC) for 8-bit and 10-bit 4:2:0 H.265, HDR included:

- when the export renders on the NVIDIA GPU (Settings ▸ Playback ▸ **Graphics device** is
  Performance, or Auto on a machine whose fastest GPU is NVIDIA), NVENC goes first, so the
  pictures don't cross to another GPU's video engine;
- otherwise it comes after the VA-API encoders and before software x265.

4:2:2 exports keep using VA-API or x265 (NVIDIA's encoder has no 4:2:2). **Hardware (VA-API)** never uses
NVENC. If NVENC stops part-way through an export (the card is lost, the driver resets), VA-API
carries on from a keyframe and the log says so; if it can't, the export ends naming both.

**Chroma.** Why: delivery files (YouTube, phones) halve the colour detail in both directions
(4:2:0). Cameras such as the X-M5 can record 4:2:2, which keeps every row of colour; keying and
heavy grading later hold up better when the master keeps it too. The **Chroma** choice
{ui:export.video.hevcChroma} (H.265 only; `file.exportMedia` parameter `chroma`, `420` or `422`):

1. Keep **4:2:0 (delivery)** for anything you upload or send to be watched.
2. Pick **4:2:2 (master, 10-bit)** for a master you will grade or cut again. It is always 10-bit,
   so the Profile choice is hidden. Revolution picks a video engine that can encode it by
   itself (here Intel's: the AMD card has none); on a machine without one the export falls back
   to software x265, which is slow at 6.2K. Many players and phones can't play 4:2:2 HEVC; that is expected for a master.

**Encoder speed.** Why: a video engine can spend more or less effort per picture. More effort gives a slightly
smaller or cleaner file at the same bitrate or quality setting, less effort exports faster. Which
matters more depends on the job: a quick review copy wants speed, a final upload wants quality.

The **Encoder speed** choice {ui:export.video.hwEncodeSpeed} (Video section, H.265 with Auto or
Hardware; command parameter `hwEncodeSpeed` of `file.exportMedia`):

| Choice | What happens |
|---|---|
| **Driver default** | Revolution sends no setting; the graphics driver uses its own default (what every earlier Revolution export did) |
| **Speed** | the fastest level the driver offers |
| **Balanced** | the middle of the driver's range |
| **Quality** | the best quality level the driver offers (slowest) |

1. Leave **Driver default** unless an export is too slow (try **Speed**) or you see blocky
   detail at your bitrate (try **Quality**).
2. Drivers offer different numbers of levels (here the AMD card reports 32, the Intel graphics 7),
   so the same choice is not the same speed on both. The ffmpeg VA-API fallback gets the same
   level (`-compression_level`); **Software (x265)** ignores it and uses its own preset.
3. **[Fork]** On an NVIDIA graphics card (NVENC, the H.264 hardware encoder everywhere and H.265
   on Windows) the choice picks one of NVIDIA's seven presets: Driver default is P5 (what
   Revolution always used), Speed P1, Balanced P4, Quality P7. A driver that doesn't know the
   chosen preset gets P5, so the export still runs.

Resolve and Premiere expose encoder effort only for some encoders and under their own names;
here one choice is mapped onto whatever range the driver reports.

**Two GPUs (Export engines).** [Fork] Why: on a machine with two graphics chips that can both
encode H.265 (here the AMD card and the Intel graphics in the processor), each one can export a
different part of the timeline at the same time, and Revolution joins the parts into one file.
For a long 6.2K export that is the difference between slower and faster than real time. It
costs more memory and keeps both chips busy, so it is a choice, not a rule.

The **Export engines** choice {ui:export.video.exportEngines} (Video section, H.265 with Auto or
Hardware, under Encoder speed; command parameter `exportEngines` of `file.exportMedia`, CLI
`--export-engines`):

| Choice | What happens |
|---|---|
| **Auto** | the main chip, plus the second one when it makes the export faster: an export without effects or a grade that qualifies (H.265, 4:2:0, a second chip that encodes, enough free memory); otherwise one |
| **Primary GPU only** | one chip, as every earlier Revolution export did |
| **All GPUs** | every chip that can encode; if the export doesn't qualify it runs on one and the export log says why |

1. Leave **Auto** for most exports. Why Auto leaves graded exports on one chip: with effects, a
   grade, noise reduction or a colour-space interpretation (Log footage) on the clips, the Intel
   graphics render only a few frames per second at full precision, so they would add nothing
   (Revolution would try them and then drop them, which wastes a few seconds). Plain exports gain a
   lot (4K about 34 instead of 25 frames per second). The **?** next to the choice
   {ui:export.video.exportEngines.help} says the same. [Fork]
   **But the second chip can still decode for the first.** When the export is smaller than the
   footage (a 4K export of 6.2K clips), Auto lets the Intel media engine decode the clips and hand
   each picture to the AMD card already shrunk to the size it is drawn at; the AMD card grades
   and encodes. A 4K graded export of 6.2K footage goes from about 25 to about 31 frames per
   second. The export log says "one GPU renders and encodes, the other decodes (hand-off at …)".
   It only happens for clips whose size on screen doesn't change during the clip (no keyframed
   Motion, no temporal noise reduction, not inside a nested sequence) and never with **Maximum
   Render Quality**, which always reads every pixel. Trade-off: the result is not bit-identical
   to a single-chip export. Edges and flat areas match, but very fine texture such as grain or
   sensor noise comes out slightly smoother, because Intel's scaler averages it where the
   compositor would sample it. To keep the old behaviour, pick **Primary GPU only**, turn on
   Maximum Render Quality, or set the environment variable `FILMCRAFT_HANDOFF_SCALE=1`. Resolve
   and Premiere don't split work this way; it is specific to this fork. [Fork]
2. Pick **Primary GPU only** when you are playing a game or using the other chip for something
   else during the export, when memory is tight, or to compare against an earlier single-chip
   export.
3. 4:2:2 masters always run on one chip (only the Intel graphics encode 4:2:2 here).
4. When the two drivers write different stream headers (the AMD and Intel drivers do), the file
   carries the headers inside the video (`hev1`) instead of once at the start (`hvc1`). Modern
   players read both; some Apple software prefers `hvc1`, so pick **Primary GPU only** for a file
   meant for Apple devices if one refuses to play it.

Speed (fork step 6E.3, measured on synthetic test footage, AMD RX 6700 XT + Intel UHD 770):
6.2K about 26 frames per second on both chips against 19 on one; 4K about 34 against 25.
**A chip that is much slower drops out by itself.** The Intel graphics share the computer's memory,
so a heavily graded export (Lumetri, curves, sharpening) runs at a few frames per second on them
and would hold back the AMD card. After a few seconds Revolution compares the two: a chip at less
than a quarter of the other's speed is stopped, and its part goes back to the faster one; at the
very end, when the faster chip is idle and would finish the slower one's last part sooner from
the start, it takes it over. So **All GPUs** (which also tries the second chip on graded exports) is never meant to be
slower than one chip; graded 4K runs at about the one-chip speed. The export log and `perf.stats` show a chip that dropped
out as "too slow" or "its last chunk is faster on …" (not an error). The one chip that decodes
4:2:2 footage is kept while 4:2:2 parts are still to be exported.
If either chip fails during the export, the other one finishes its parts; if both fail, the
export runs again on one chip. `perf.stats` (`export.lanes`, `export.sampleEntry`,
`export.lanesNote`) shows what each chip did, or why the export stayed on one. Per chip,
`failed` says why it stopped because something went wrong; `retired` says why the slow-lane rule
took a working chip off the export (it was holding the other one back), which is not an error.

Neither Resolve nor Premiere offers to split one export across two graphics chips; Resolve's
render queue can use several machines (remote rendering), which is a different feature.

The software fallbacks run the system ffmpeg as a separate program. The fork plan removes them in
Phase 7 (Revolution's own encoders become the fallback), so treat them as a safety net, not a
feature.

## Export a looping GIF

> id: export.gif | does: exports a short range of the sequence as a silent, endlessly looping animated GIF | ui: export.preset, export.format, export.range | cmd: file.exportMedia | resolve: no GIF in the Deliver page | premiere: Animated GIF format | when: a reaction GIF, a meme, a looping clip for a chat, a forum or a README; a short silent loop | see: export.presets

**[Revolution]**

A GIF is a picture format that plays as a short silent loop almost anywhere (chats, forums, web
pages). It has only 256 colours per frame and makes large files, so keep it short and small:
a few seconds at 640×360 and 15 frames per second is the usual size. Resolve's Deliver page has
no GIF format; Premiere's Animated GIF works the same way as this.

1. Set an In and an Out point around the moment you want (I and O on the timeline), so only
   that part is exported.
2. Open the Export page (`file.exportMedia`) and pick the preset **Animated GIF 640×360**
   {ui:export.preset} (640×360 at 15 fps), or choose **Animated GIF** in **Format**
   {ui:export.format} and set your own size and frame rate.
3. Set **Range** {ui:export.range} to **In to Out**.
4. Click **Export**. The file loops forever; there is no sound in a GIF, so the audio settings
   are ignored.

From the command line: `filmcraft-cli export clip.gif --format gif --range inOut` (or
`--start` / `--end` in seconds).

## Bitrate or constant quality

> id: export.bitrate-crf | does: aims an encoder at a file size (bitrate) or a quality (CRF) | ui: export.video.bitrateMode, export.video.crf | resolve: Quality: restrict to / automatic | premiere: Bitrate Encoding: CBR, VBR | when: a file must fit a size or stay under a limit (so many GB, an upload cap), or look a certain way | see: export.presets

**[Standard] [Revolution]**

An encoder can aim for a **size** (a bitrate: so many megabits per second) or for a **quality**
(it spends as many bits as each moment needs). A bitrate is predictable, which upload sites and
players like; constant quality gives an even look and a file size you only learn afterwards.

The **Bitrate Encoding** list {ui:export.video.bitrateMode}:

- **CBR**: the same bitrate all the time. Rarely needed.
- **VBR, 1 pass**: an average target with a ceiling; more bits for busy shots, fewer for still
  ones. The right choice for uploads.
- **VBR, 2 pass**: H.264 only (a hardware H.265 encoder makes one pass, so it is greyed out).
- **CRF (constant quality)**: a quality number from 0 to 51 in **Quality (CRF)**
  {ui:export.video.crf}; lower is better and larger. 18 looks the same as the source to most
  eyes; 22–23 is a good everyday value. x265 reads it as its CRF; the video engine reads it as a
  fixed quantiser (QP), which looks similar but sizes differently.

**If you made presets in the fork before the first upstream merge:** they had no mode list,
only a CRF number. They still load: a preset with a CRF number and no mode opens as **CRF**, one
without a number opens with its bitrate.

## HDR sequences export as Main 10

> id: export.hdr-main10 | does: HDR sequences export as 10-bit H.265 Main 10 to avoid banding | ui: export.video.hevcProfile, export.video.hevcMain10Note | cmd: sequence.colorSettings | resolve: Main10 profile | premiere: HEVC Main 10 | when: exporting HDR | see: export.hevc

**[Fork] [Standard]**

H.265 comes in two everyday profiles: **Main** (8-bit) and **Main 10** (10-bit). 8 bits are
plenty for ordinary (SDR) video, but stretched over HDR's much wider brightness range they show
visible steps (banding) in skies and gradients, so HDR is always 10-bit.

1. A sequence is HDR when Sequence ▸ **Color Management…** (`sequence.colorSettings`) sets its
   working space to Rec. 2100 PQ or HLG (see [color/color-management.md](color/color-management.md)).
2. Exporting such a sequence to H.265 writes **Main 10** with PQ or HLG signalling, even when the
   **Profile** box {ui:export.video.hevcProfile} says Main (8-bit). The dialog says so under the
   Profile box: "HDR export: written as Main 10 (10-bit)" {ui:export.video.hevcMain10Note}.
3. When no encoder here can write HDR H.265, the sequence is tone-mapped and exported as ordinary
   8-bit SDR instead; the note isn't shown then.
4. For an SDR copy of an HDR sequence, export with the `sdr` setting of `file.exportMedia` (the
   dialog has no switch for it yet).
