# Matching shots: stills, wipe, copy and paste

**[Resolve-style]** for stills and grade copying · **[Fork]** for the "same grade?" and Shot Match work still to come

## Why

> id: color.matching.why | does: why shots of one scene must match, and the quickest route to it | resolve: gallery stills, shot match | premiere: Lumetri Color Match | when: a cut feels wrong because two shots differ | see: color.matching.stills

**[Resolve-style]**

A cut between two shots can feel off even though nothing in them moves: usually the two shots
of one scene differ in colour or brightness. The quickest route: save a good
shot as a **still**, compare the next shot against it, then reuse or adjust the grade.

## Stills gallery

> id: color.matching.stills | does: the Gallery holds still pictures of frames with their grades | ui: stills.grab, stills.delete | cmd: stills.grab, stills.apply, stills.label, stills.delete | resolve: Gallery stills | premiere: — (Comparison View reference) | when: keeping a reference look; putting a still's grade onto other clips | see: color.matching.compare

**[Resolve-style]**

The **Gallery** panel (left side of the Color Grading workspace) holds stills: a picture of the
Program frame plus the grade that made it, saved in the project.

| Control | Id | Command | Does |
|---|---|---|---|
| **Grab Still** | {ui:stills.grab} | `stills.grab` | saves the frame under the playhead and its clip's grade |
| click a thumbnail | `stills.item.<id>` | `compare.set` | chooses it as the reference and starts the comparison (Wipe, or Split if Split was on) |
| double-click a thumbnail | | `stills.apply` | gives its grade to the selected clips |
| **Import…** | {ui:stills.import} | `stills.import` | brings in a reference picture from a file (see below) |
| **Delete** | {ui:stills.delete} | `stills.delete` | removes the selected still |
| **Off / Wipe / Split** | {ui:stills.mode.off} {ui:stills.mode.wipe} {ui:stills.mode.split} | `compare.set` | the comparison mode (Wipe and Split need a chosen still) |

With Workflow set to Resolve, **Ctrl+Alt+G** runs `stills.grab`.

- **Revolution:** no button to rename a still yet: `stills.label` renames one.

## Compare

> id: color.matching.compare | does: compares the current shot with a still by wipe or split | ui: stills.mode.*, compare.divider | cmd: compare.set | resolve: wipe / split screen | premiere: Lumetri Comparison View | when: matching by eye against a reference | see: color.matching.stills

**[Resolve-style]**

1. Park the playhead on the reference shot and press **Grab Still** {ui:stills.grab}.
2. Move to the shot to match and click the still's thumbnail: the wipe starts at once, the still
   covering the left part of the Program picture. Drag the divider {ui:compare.divider} to
   slide it (`compare.set` with `position` 0 to 1; `vertical` turns it into a top / bottom
   wipe, which has no button yet).
3. Press **Split** to see the whole still and the whole live frame side by side.
4. Press **Off** to leave comparison; **Wipe** brings it back.

A still keeps its look even if the source clip is regraded later.

## Copy, paste and append grades

> id: color.matching.copy-paste | does: copies a grade to other clips, replacing or appending nodes | cmd: grade.copy, grade.paste, grade.append, grade.applyFrom, edit.pasteAttributes | resolve: copy / paste grade, append node graph | premiere: Paste Attributes | when: a whole scene shares a grade | see: color.matching.same-grade

**[Resolve-style]**

In the Clip ▸ Grade menu, for video clips:

1. Select the clip with the grade you like and choose **Copy Grade** (`grade.copy`).
2. Select the target clips.
3. Choose **Paste Grade** (`grade.paste`) to replace their grades, or **Append Grade**
   (`grade.append`) to add the copied nodes after the nodes they already have.
4. **Apply Grade From Clip** (`grade.applyFrom`, also in the menu) does copy and paste in one
   command from a source clip id (`from`).

`grade.clear` ("Remove Grade") takes a grade off a clip; it has no menu item yet, so run it from
the command list, the control channel or MCP. Each of these is one undo step however many
clips it touches.

### Paste Attributes

**[Revolution]**

Edit ▸ **Paste Attributes…** (`edit.pasteAttributes`, Ctrl+Alt+V, **[Revolution]**) pastes the
copied clip's motion, opacity, effects and so on; since G1 it also carries the grade. The
command's `grade` option (default on) leaves the grade out when set to false; the dialog has no
Grade checkbox yet, so from the dialog the grade always comes along.

## "Same grade?"

> id: color.matching.same-grade | does: tells whether clips carry the same grade, and what differs | ui: grade.match.changes | cmd: grade.compareData | resolve: — | premiere: — | when: after copying a grade around a scene | see: color.matching.check-scene

**[Fork]**

### Why

After copying a grade around a scene it is easy to nudge one clip by accident. Revolution can tell
you which clips carry *exactly* the same grade: same nodes in the same order with the same
settings. Different node names or ids don't make two grades count as different, nor do settings
left at their neutral value.

### Steps

1. Select a clip and open the **Match** tab {ui:grade.tab.match} on the Color page.
2. The first line says **Same grade as:** followed by the other clips with an identical grade, or
   that no other clip has it.
3. For scripts, `grade.compareData` groups every video clip of the sequence by grade
   (a list of `groups`, each with its `clips`); pass `"clips"` to look at some only.

## Compare shots and check a scene

> id: color.matching.check-scene | does: measures how two shots differ and which clip of a scene doesn't match | ui: grade.match.compare, grade.match.checkScene, grade.match.reference | cmd: grade.compareShots, grade.checkScene | resolve: — (by eye with scopes) | premiere: — | when: finding the odd clip out | see: color.matching.shot-match

**[Fork]**

### Why

Two shots of the same scene should look like they belong together. **Compare shots** measures the
two *graded* pictures (brightness by tonal range, colour cast, saturation) and says in plain words
how they differ ("B is about ⅓ stop darker", "B is slightly warmer"), with a match score out of
100. **Check scene** looks at several shots and names the one that differs most from the rest.
It is a guide, not a verdict: a wide shot and a close-up can legitimately differ, and only
tonal ranges both shots actually contain are compared (a note says so when they share little).

### Steps

1. In the **Match** tab, choose the **Reference** clip {ui:grade.match.reference}.
2. Click **Compare shots** {ui:grade.match.compare} (`grade.compareShots` with `"with"`; or
   `"referenceTime"` for any frame of the sequence). The score and the differences appear below.
3. To check a scene, select its clips in the timeline and click **Check scene**
   {ui:grade.match.checkScene} (`grade.checkScene`). The clip that stands out is named first.

## Shot Match

> id: color.matching.shot-match | does: solves a new node so a shot matches a reference shot | ui: grade.match.shotMatch | cmd: grade.shotMatch | resolve: Shot Match | premiere: Lumetri Apply Match | when: a first pass of a match before fine-tuning | see: color.matching.check-scene

**[Resolve-style]** **[Fork]**

### Why

Shot Match does the first pass of a match for you: it solves a new node's Lift / Gamma / Gain /
Offset and saturation so the clip's measured look matches the reference. Because the result is
an ordinary node, you can see exactly what it did, adjust it, or switch it off.

### Steps

1. Choose the **Reference** in the **Match** tab: another clip, or a Gallery still (a grabbed
   frame or an imported reference picture, `"still": <id>`).
2. Click **Shot Match** {ui:grade.match.shotMatch} (`grade.shotMatch`). A node labelled
   "Shot Match" is added at the end of the grade (one undo step); skin tones are weighted less
   so a colour cast on faces doesn't drive the match (`"faceDetection": false` turns that off).
3. Compare again, then refine the node by hand.

**[Fork]** The match is solved on the clip's grade *before* its effects (the new node runs before
the effect stack), while the reference is measured as it finally looks. Effects after the grade
that change colour (Lumetri, Tint, opacity) aren't accounted for: check with **Compare shots**
afterwards. The Match tab shows once the clip has a grade (add an empty node first: it changes
nothing); the commands work on any video clip. These requests render frames while you wait, so a
long scene check takes a few seconds (at most 64 clips are measured).

**[Resolve-style]** Like Resolve's Shot Match, the idea is "make this look like that"; the solver
and its controls are Revolution's own (it writes the primaries of a node instead of a hidden
adjustment).

## The node explainer

> id: color.matching.explainer | does: measures what each node actually does to the picture and says it in words | ui: grade.match.explainGrade, grade.match.explainNode | cmd: grade.explainGrade, grade.explainNode | resolve: — | premiere: — | when: reading someone else's grade, or your own later; seeing where in the frame a node is doing its work | see: color.matching.ai

**[Fork]**

### Why

A grade with several nodes is hard to read. The explainer measures what each node actually does
to the picture (not just which sliders moved) and says it in one sentence, e.g. "Node 2 'Skin':
affects 18 % of the frame; inside it, midtones +⅙ stop, slightly less magenta; shadows and
highlights untouched".

### Steps

1. Select a node and open the **Match** tab.
2. Click **Explain node** {ui:grade.match.explainNode} (`grade.explainNode`) for the selected node,
   or **Explain grade** {ui:grade.match.explainGrade} (`grade.explainGrade`) for one sentence per
   node.
3. Click **Show what changed** {ui:grade.match.changes} to see a heat map of where, and how
   strongly, the node changed the picture (`grade.explainNode` with `"overlay": <png path>`).
4. **[Fork]** On a node that combines other nodes' pictures or selections, **Explain node** gives
   the sentence described in [nodes.md](nodes.md) ("What the explainer says about branches"),
   returned as `branch` by `grade.explainNode`.

Numbers are measured on the frame at the playhead (or the clip's middle frame), rounded to ⅙ of
a stop and whole percentages; anything below a just-noticeable change is called "untouched".

## AI explanations

> id: color.matching.ai | does: a local model rewrites the explainer's measured sentences in plain words | ui: settings.color.ai.* | cmd: ai.explanation, ai.download, ai.select, ai.setEnabled, ai.remove, ai.models | resolve: — | premiere: — | when: you want friendlier explanations | see: color.matching.explainer

**[Fork]**

### Why

The explainer's template sentence is exact but terse ("midtones +⅙ stop, slightly less
magenta"). A small language model running on your own computer can say the same thing in plainer
words for a beginner ("this node brightens the middle tones a little"). It is optional and
offline, and it never decides anything: it only rewrites the measured facts, and Revolution checks
every word it writes against them before showing it.

### Steps

1. Open **Settings ▸ Color** (`app.settings.color`) and find **AI explanations**.
2. Pick a model in **Model** {ui:settings.color.ai.model} (`ai.select`). Each entry shows its
   size, licence and whether it is installed; all are openly licensed instruct models. Most are
   small (0.1–1.2 GB). **SmolLM3 3B (4-bit)** (about 2 GB) is listed for the coming assistant
   ("Ask the coach" with a model, fork AI3); it can explain nodes too, but it is several times
   slower than the small ones (about 6 tokens, roughly 4 words, a second on a 12-thread desktop CPU), so pick a small
   one for explanations. [Fork]
3. Click **Download** {ui:settings.color.ai.download} (`ai.download`). The model comes from its
   publisher, checked against its pinned SHA-256; the bar shows progress and **Cancel download**
   stops it. **Remove** {ui:settings.color.ai.remove} (`ai.remove`) deletes it again.
4. Tick **Explain nodes in plain words with a local AI model** {ui:settings.color.ai.enabled}
   (`color.ai.enabled`, or `ai.setEnabled`) and click OK.
5. In the **Match** tab click **Explain node** {ui:grade.match.explainNode}. The template sentence
   appears at once; when the model's text is ready and passes the check it replaces it, with a
   small **checked** badge.

### What "checked" means

Every number, direction ("warmer", "brighter"), tonal range and strength word in the model's
text must agree with the node's measured facts, in a small fixed grammar, and the text may be at
most two short sentences. If it doesn't pass, or the model is missing or fails, you see the plain
template sentence instead. The model gets 15 s to write; a text cut off at that point usually
fails the check ("rejected"), while "timeout" means no answer at all within 60 s (loading the
model included). A timeout or failure is tried again the next time you click; a rejection is
remembered for that node until it changes. While the model writes, each finished sentence that
passes the check appears as soon as it is done (usually under half a second for the first), so
you can read along; if a later sentence fails the check, the whole text falls back to the
template. **[Fork]** You often won't wait at all: when you stop adjusting a node for about
half a second (and nothing is playing), Revolution starts writing its explanation in the
background, so **Explain node** is usually answered at once. Only the latest node is written
ahead; an older background answer you never asked for is dropped. When the model was asked, a small **plain template**
hint under the sentence says why. Most stock models fail this strict check, so with them expect
the template often.

**[Fork]** For the mixer sentences above (and the Compositor's node lines) the check is stricter
still: the model's text must be one of a few fixed ways of saying it, with exactly the node names,
shares, opacities and blend modes the graph has. These sentences come from the graph itself, not
from measuring the picture, so the template is already exact; the model can only choose the
wording, never add or change a fact.

### The Revolution-tuned explainer

Revolution has its own explainer models: small models trained to describe a node in exactly the
words the check accepts. In tests the best one passed 35 of 36 sample nodes and answered in about
1.6 seconds. They download like any other model (Settings ▸ AI, **Download**), from the fork's
public releases. The "fast" 135M tune below isn't published yet: in the **Model** list it says
"Built on this computer" and its **Download** stays greyed out. If you haven't picked a model yourself, Revolution
uses the best tuned explainer that is installed, and the standard default model otherwise.

The newest, **Revolution explainer, SmolLM2 360M tuned v3 (8-bit)**, is the first choice: it also
learned compositor nodes and branch nodes (parallel, layer, splitter / combiner, key mixer,
outside), where the older tunes mostly fall back to the built-in text. In tests 138 of 141 sample
answers passed the check, at about 1.3 seconds each.

A newer tune, **Revolution explainer, SmolLM2 360M tuned v4 (8-bit)** [Fork], also learned the HDR zone
wheels, the colour warper, the 3D qualifier, the compositor's Tracker, Planar Tracker, Roto and Paint
nodes, and node groups. In tests it passed as many of the older sample answers as v3 (138 of 141) and
every one of the 21 new ones (v3: 18). Until it is published it is only on the computer it was built
on: there it says "Built on this computer" in the **Model** list, and you pick it yourself; Revolution
keeps choosing v3 automatically until v4 can be downloaded.

On a slower computer, pick **Revolution explainer, SmolLM2 135M tuned (8-bit, fast)** in the
**Model** list: it answers in about half the time (0.6 s instead of 1.3 s here) and passed nearly
as many sample nodes (one fewer of 76). Revolution never picks it on its own: it is for when the
standard tune feels slow.

### Privacy

Nothing leaves your computer. The only network use is the download you start; the model is
never bundled with Revolution and runs on the CPU. Agents can do the same with `ai.models`,
`ai.download`, `ai.select`, `ai.setEnabled` and `grade.explainNode` with `"wait": true` (which
blocks at most 20 s; still writing then, it returns the template with an `aiKey` to poll with
`ai.explanation`; while pending, its `partial` field holds the checked sentences so far).

## If you know Resolve

> id: color.matching.resolve | does: how stills, wipes and copying grades compare with Resolve | resolve: Gallery, memories, versions | when: you come from Resolve | see: color.matching.stills

**[Resolve-style]**

Stills, wipe and split work like the gallery and its wipe.

- **Revolution:** no memories, versions, groups or timeline grades yet: grades belong to clips.
