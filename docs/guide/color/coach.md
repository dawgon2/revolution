# Grading Coach: guided lessons

**[Fork]** · step-by-step lessons for people new to grading, short hints the first time something new comes up, and answers to questions from this guide (GRADE-PLAN G2.12)

## Why

> id: color.coach.why | does: guided lessons, context hints and Ask the coach help you learn the node grader | resolve: — | premiere: — (Learn panel tutorials, closest) | when: learning to grade in Revolution | see: color.coach.start

**[Fork]**

If you are completely new to colour grading, start here: the app teaches you. The node grader
can be a lot to take in at first. A lesson walks you through one task, one step
at a time, on your own clip. Each step says what to do and why that step comes where it does,
outlines the control to use, and waits until you have actually done it before moving on. You
learn by doing the real thing, not by reading about it.

## Start a lesson

> id: color.coach.start | does: picks and starts a lesson on your own clip | ui: coach.lessons.open, coach.lessons.close | cmd: coach.start, coach.lessons, coach.showLessons | when: learning one task step by step | see: color.coach.card

**[Fork]**

Pick a lesson from the list. It works on your own clip, so open the project you want to practise
on first.

1. Click **Lessons** {ui:coach.lessons.open}: in the Resolve workflow it is in the page bar at the
   bottom, left of the Home icon; in the Premiere workflow, on the right of the header at the top
   (next to **Ask the coach** and **Assistant**). Or choose **Help ▸ Grading Lessons…**.
2. The list shows every lesson with a one-line summary; a tick marks the ones you have finished.
   Click **Start** (**Again** for a finished one) next to the lesson you want. Close the list
   with **×** {ui:coach.lessons.close}.
3. The coach card appears at the bottom right of the window {ui:coach.card}. Read the step, then
   do it. The card moves to the next step by itself once you have done it.

In lessons that work on any clip (Your first grade, What a node is, Which node for what), the
"select a clip" step finishes by itself when a clip is already selected as it starts (clicking a
selected clip again changes nothing, so the coach doesn't wait for it). Lessons that need a
particular clip (a face, a noisy shot, a reference) always wait for you to click it. Press **Back** to return to such a step: it then waits for you.

For scripts and agents: the menu entry is the command `coach.showLessons`; each Start button is
`coach.lessons.item.<lesson id>`. `coach.lessons` lists the lessons (and `completed`),
`coach.start` with `{"lesson": "first-grade"}` starts one, and `coach.state` reports the lesson,
the step, its text and what it is waiting for.

## The card

> id: color.coach.card | does: the lesson card: step, why, Next, Back, Skip and Exit | ui: coach.card, coach.next, coach.back, coach.skip, coach.exit, coach.close | cmd: coach.next, coach.back, coach.skip, coach.exit | when: following a lesson | see: color.coach.outline

**[Fork]**

- The card shows the lesson's title, "Step n of m", where the feature comes from
  ([Resolve-style], [Fork], …), what to do, and **Why** that step matters.
- **Back** {ui:coach.back} (`coach.back`) goes to the previous step.
- **Skip** {ui:coach.skip} (`coach.skip`) moves on without doing the step.
- **Exit** {ui:coach.exit} (`coach.exit`) stops the lesson.
- **Next** {ui:coach.next} (`coach.next`) only appears on steps that are just reading or
  looking. Every other step finishes when you do it.
- When the last step is done, the card says **Lesson complete**; **Close** {ui:coach.close}
  removes it.

Undo does not take a lesson back a step. If you undo something a step asked for, press **Back**
and do it again.

## The outline

> id: color.coach.outline | does: a coloured outline marks the control a step is about | ui: coach.spotlight | when: you can't find the control a step names | see: color.coach.card

**[Fork]**

A coloured outline {ui:coach.spotlight} marks the control the step is about: a button, a slider,
a wheel or a whole panel. If the control is in a list that scrolls, the coach scrolls it into
view once. If the control isn't visible at all (for example the step needs another tab or
workspace first), the card says "The highlighted control isn't on screen yet."; follow the step
text to reveal it. The outline is hidden while a window such as Settings is open in front.

## Progress

> id: color.coach.progress | does: finished lessons are remembered between sessions | cmd: coach.state | when: checking what you have done; will it remember which lessons I already did | see: color.coach.lessons

**[Fork]**

Finished lessons are remembered between sessions, in your preferences (`color.coach.completed`;
`coach.lessons` shows `completed: true`). A lesson can be repeated at any time.

Hints you have seen are remembered the same way (`color.coach.hintsSeen`, see below).

## The lessons

> id: color.coach.lessons | does: the list of bundled lessons and what each teaches | when: choosing a lesson | see: color.coach.start

**[Resolve-style]** **[Fork]**

| Id | Lesson | What you learn |
|---|---|---|
| `nodes-and-flow` | What a node is and how the chain flows | Build two nodes and see the second one work on the first one's picture. |
| `first-grade` | Your first grade | Balance, exposure, contrast, saturation, then a look, and why in that order. |
| `which-node` | Which node for what | One job per node; a four-node starter tree for F-Log2. |
| `match-shots` | Match two shots with stills and wipe | Grab a still, wipe it against another shot, match brightness then colour, try Shot Match. |
| `fix-skin` | Fix skin with a qualifier | Pick skin by colour, check the selection, soften it, then adjust only the skin. |
| `tracked-window` | Brighten a face with a tracked window | A soft circle on the face, tracked through the shot, with a small lift inside. |
| `log-noise` | Clean up noise in log shadows | Noise reduction first, then lift the shadows, and compare. |
| `first-comp` | A moving title in a comp | Text, Transform, Merge and Media Out in the Compositor (see [the Compositor](../compositor.md)). |
| `key-comp` | Key a clip over a new background in a comp | Media In, Key, Solid, Merge and Color: a keyed clip over a new background, graded as one picture. |
| `parallel-nodes` | Two corrections on the same picture (parallel nodes) | Add a parallel node so two nodes work on the same image, and see how a mixer blends them (see [nodes](nodes.md)). |
| `layer-look` | A look on a layer, mixed in at part strength | A base grade, a layer node on top with the look, and its opacity and blend mode. |
| `splitter-combiner` | Grade the red, green and blue channels separately | Split a picture into channel nodes, correct the blue one, combine them again. |
| `outside-node` | Grade everything except the selection (outside node) | Pick skin with a qualifier, then grade everything outside it with an outside node. |
| `key-mixer` | Combine two selections (key mixer) | Two qualifiers, a key mixer that combines them, and a node limited by the result. |
| `track-effects` | One effect for a whole track (track effects and sends) | An audio effect dropped on a track header, adjusted in Effect Controls, and the track sent to a new bus. |

## Context hints

> id: color.coach.hints | does: a one-time hint the first time you do something that needs explaining | ui: coach.hint.*, settings.color.coach.hints | cmd: coach.hint, coach.dismissHint, coach.resetHints | when: you want or don't want hints | see: color.coach.ask

**[Fork]**

The first time you do something on the Color page that usually needs a word of explanation, a
small card {ui:coach.hint.card} appears at the bottom right: why it matters first, then what it
is and when to use it, and a link to the lesson about it {ui:coach.hint.lesson}. Each hint shows
once. Hints never appear while a lesson runs; a hint whose moment comes during a lesson waits for
the next time.

- **Got it** {ui:coach.hint.dismiss} closes the card.
- **Turn hints off** {ui:coach.hint.off} switches every hint off. The same switch is **Settings ▸
  Color ▸ Grading Coach ▸ Show Grading Coach hints** {ui:settings.color.coach.hints}
  (`color.coach.hints`, on by default); turn it back on there.

| Hint | Shows the first time you… |
|---|---|
| Nodes | add a node |
| The qualifier | open the Qualifier tab |
| Windows | open the Windows tab |
| Tracking a window | track a window |
| Noise reduction | open the Noise tab |
| LUTs | set a node's LUT |
| Film grain | add grain to a node |
| Stills | grab a still |
| Wipe and split | compare a still with a wipe or a split |
| Shot Match | open the Match tab |
| Qualifier after a strong look | switch on a qualifier in a node that comes after a node with a strong look (a LUT, heavy contrast or saturation, a bent curve, a big colour shift) |
| AI explanations | turn on AI explanations |

For scripts and agents: `coach.hint` returns the hint on screen (`hint`: `id`, `title`, `why`,
`text`, `lesson`), whether hints are on (`enabled`) and the ids already shown (`seen`); `{"all":
true}` adds every hint. `coach.dismissHint` closes it (`{"off": true}` also turns hints off), and
`coach.resetHints` forgets the hints already shown so each can appear again.

- **Revolution:** no translated hint or lesson texts yet: lessons and hints are in English only, not in
  Spanish or another language; the buttons
  and labels around them follow the interface language.

## Ask the coach

> id: color.coach.ask | does: answers a question with steps from this guide, every control checked | ui: coach.ask.* | cmd: coach.ask | when: you know what you want but not which tool | see: color.coach.hints

**[Fork]**

When you know what you want ("make the sky bluer", "my log footage looks flat") but not which
control does it, ask. The coach looks the question up in this guide, and only this guide, and
answers with the numbered steps of the section that fits best, naming the real buttons. It
doesn't invent anything: if most of your words aren't in the guide it says "The guide doesn't
cover this yet."; if you ask "is there …" or "where is …" about a word the guide never uses, it
names that word ("The guide doesn't mention "hologram"."); a "why …" or "what is X for" question
gets the section's explanation above its steps; if the guide covers it but has no
steps, it says so and offers the closest lesson instead. Every control and command an answer
names is checked against the app you are running; a step that names one this app doesn't have
is left out (the box says how many, and whether controls or commands).

1. Click **Ask the coach** {ui:coach.ask.open} in the page bar at the bottom, left of Home
   (Premiere workflow: the header at the top right). The box opens at the bottom right; click the
   button again to close it.
2. Type your question in the box {ui:coach.ask.input} in your own words, then press Enter or
   **Ask** {ui:coach.ask.go}.
3. Read the steps. Click a step that names a control (its id is `coach.ask.step.<n>`, from 1):
   the coach outlines that control, scrolling its panel if needed. Click it again to remove the
   outline.
4. When there are no steps, or you'd rather be walked through it, click the lesson link
   {ui:coach.ask.lesson} to start that lesson.
5. Close the box with **×** {ui:coach.ask.close}.

Under the answer, "From the guide" names the section the steps came from and its provenance
([Resolve-style], [Revolution], [Fork], [Standard]), so you know whether a Resolve or Premiere
habit applies. Any difference from Resolve or Premiere is only what that section itself says.

For scripts and agents: `coach.ask` with `{"question": "…"}` returns `steps` (each with `text`
and, when it names them, `ui` and `command`), `sources` (`page`, `heading`, `provenance`),
`stepsSource` (which source the steps came from), `lesson` (`id`, `title`, only when it fits the
question), `confidence` and `coverage` (0 to 1), `covered` (false: the guide doesn't cover the
question, so there are no steps or sources), `removed` (steps left out) with `removedControls`,
`removedCommands` and `removedIds`, and `fallback` (no steps: see `lesson`, `summary` and
`message`). Command ids are always checked; any other id-shaped word (a preference key, a
parameter name) can't be, so it is left out of the text. Without the app's window (the CLI, the
headless MCP server) automation ids can't be checked: `uiChecked` is false and steps carry no
`ui` (their text still names the control).

Today the coach finds sections by their words (a small search index with a list of beginner
words, such as "darker" or "grainy", matched to the guide's terms). A control that only appears
in a window you haven't opened yet this session (for example **OK** in the Interpret Footage
dialog) counts as unknown, so its step is left out until you have opened that window once.

## Write a hint

> id: color.coach.write-hint | does: how contributors add a context hint | when: adding a hint | see: color.coach.write-lesson

**[Fork]**

For contributors. The hints are one data file, `assets/coach/hints.toml` (its header and
`crates/engine/src/coach_hints.rs` are the full reference). Each `[[hint]]` has an `id`, a
`title`, `why` (shown first), `text`, an optional `lesson` and `when`, a list of triggers: a
command with an optional `params` pattern (the same patterns as a lesson step), a control shown on
screen (`element`), or a rule the engine checks on the grade (`rule = "qualifierAfterHeavyLook"`).
Write why before what, in plain words of your own.

## Write a lesson

> id: color.coach.write-lesson | does: how contributors write a lesson file | when: adding a lesson | see: color.coach.write-hint

**[Fork]**

For contributors. A lesson is a TOML file in `assets/lessons/`, added to `BUNDLED_FILES` in
`crates/engine/src/coach.rs` (the module documentation there is the full reference). No other
code is needed.

```toml
id = "my-lesson"            # lower case, digits, `.`, `_`, `-`; the file is my-lesson.toml
title = "Short title"
summary = "One sentence for the lesson list."
provenance = ["resolve-style"]   # resolve-style | filmcraft | fork | standard
order = 8                   # position in the list

[[step]]
text = "What to do, in plain words."
why = "Why it matters, before how."
target = ["grade.addFirst", "grade.addNode"]   # automation id(s) to outline; first on screen wins
expect = { command = ["grade.addNode", "grade.insertNode"] }
```

1. `expect` is exactly one of:
   - `{ command = "id" }` (or a list of up to 8 ids), optionally with `params`: the step is done
     when that command succeeds with matching parameters. A table in `params` must match the
     same keys (other keys are ignored); `{ gt = 1.0 }`, `ge`, `lt`, `le`, `ne` compare numbers;
     `{ any = true }` means "present"; `{ nonzero = true }` means "a number, or a number in a
     list, other than 0"; `{ in = ["a", "b"] }` means "one of these"; a list matches item by
     item. Example: `params = { clips = [{ any = true }] }` (at least one clip selected).
   - `{ element = "automation.id" }`: done when that control is on screen (for "open this tab"
     steps).
   - `{ manual = true }`: the reader presses Next.
2. A step may add `already = "clipSelected"`: it finishes by itself when it starts (or on
   `coach.state`) while a clip on a video track is selected. Use it only where any clip will do,
   never where the lesson needs a particular clip (a face, a noisy shot, a reference).
3. Use only real command ids and automation ids. The engine test checks every command id; the UI
   test in `crates/ui-egui/tests/ui/coach_ui.rs` walks each lesson and checks that every outlined
   control is on screen.
4. Write in your own words, plain language, why before how. Never copy a manual.
5. Limits: 64 KiB per file, 40 steps, 1 200 characters per text, patterns at most 6 deep and 64
   values. Unknown fields are refused.

## Also ask the assistant

> id: color.coach.assistant | does: adds an answer written by a small language model on this computer, checked against the guide | ui: coach.ask.assistant, coach.ask.assistStop | cmd: coach.assist, coach.assistResult, coach.assistForget | when: the guide's steps don't quite fit your question; how long a picture look takes on built-in graphics | see: color.coach.ask

**[Fork]**

The keyword coach repeats the guide's own steps; the assistant (a small language model on your
computer; nothing leaves it) writes an answer in its own words from the sections that fit.
How fast it is depends on the computer: with a separate graphics card (for example an AMD Radeon)
it runs there, about 2–4 seconds per answer with the first words after about a second; with
only built-in graphics it runs on the processor, 20–40 seconds per answer. It is off until you
tick it, and the guide's answer is always shown above it at once. Resolve and Premiere have
nothing like it. (Scripts and testers: `FILMCRAFT_LLM_GPU=0` forces the processor, `=1` also
uses built-in graphics, which is usually slower than the processor.)

1. In the **Ask the coach** box, tick **Also ask the assistant (runs on this computer)**
   {ui:coach.ask.assistant}. It stays ticked next time (the same switch is in Settings ▸ Color ▸
   Grading Coach).
2. Ask as usual. The guide's answer appears at once; below it, under **Assistant**, the
   assistant's text appears as it is written, marked "Not checked against the guide yet."
3. Changed your mind? Click **Stop** {ui:coach.ask.assistStop}. Closing the box, unticking the
   box or asking a new question also stops it.
4. When it finishes, the text is checked: a line that names a button, command or shortcut the
   guide sections don't have is left out (the box says how many). Lines marked
   "General advice:" are shown dimmer: they aren't from the guide. **Sources** lists the guide
   sections the answer used (or was given).
5. Ask a follow-up ("and how do I undo that?"): the assistant gets your last question and its
   answer too. To start over, click **New topic** {ui:coach.ask.newTopic} (closing the box does
   the same).
6. Want to see it done? When a lesson shows what the assistant described and the guide's answer
   above doesn't already offer it, click **Show me: …** {ui:coach.ask.assistLesson} to start that
   lesson (the lesson is picked from your question and the sections the answer cites).

No model installed, or nothing passed the check: one line says so and the guide's answer stays.
"I don't know" beats a guess: when no guide section matches the question at all, the assistant
isn't asked ("The guide has nothing on this, so the assistant didn't guess."); when it says the
sections don't cover your question, its answer (naming the closest section) is kept and a line
{ui:coach.ask.assistDontKnow} points you to the guide's closest answer above. Scripts see
`dontKnow: true`, or `fallback.kind` `nothingFound`.
The model is **SmolLM3 3B (4-bit)**, about 2 GB. Scripts: `coach.assist {question}` → `{id}`;
`coach.assistResult {id}` (`pending` + `partial`, or `done`); `coach.assistForget {id}` stops it.
The assistant answers by **picking the guide's own steps**: the model only chooses and orders
the numbered steps of the sections it was given, and Revolution shows those steps word for word
with their section, so it can't make up a button or a menu. Steps it picks from a section that
matches your question much less well than the best one (under half its score) are left out, because
the small model tends to pad answers with unrelated steps. It reads less like a conversation
than a written answer. Scripts can ask for `style: "free"` (the model writes its own answer, then
lines naming unknown controls are taken out); in tests it invented far more, so `pick` is the
default. [Fork]

**Looking at the picture (optional, early).** Once the picture model is installed, a question
about the picture itself (exposure, colour, skin, sky, light) also gets a look. To install it:

1. Open Settings ▸ Color and find **Picture model** under AI explanations (Qwen3-VL 4B Instruct,
   about 8.9 GB, Apache-2.0).
2. Click **Download** {ui:settings.color.ai.vision.download} (`ai.vision.download`). It comes
   from the publisher's Hugging Face page at a fixed version, and every file is checked against
   its checksum. **Cancel download** {ui:settings.color.ai.vision.cancel} stops it; the row says
   **installed** when it is done (`ai.vision.status`).
3. **Remove** {ui:settings.color.ai.vision.remove} (`ai.vision.remove`) deletes it again, with the
   text model prepared from it; other files you put in its folder stay.

It needs about 10 GB of free memory while it looks. Then, for a picture question, Revolution renders the Program frame small, the vision model writes
one or two sentences about what it sees that bears on your question ("the sky shifts from purple
to orange at the horizon, low sun"), and the assistant gets that line along with the measured
numbers. How-to questions ("how do I add a node?") skip it. The very first look prepares the
model (about 25 seconds and a 4.3 GB file next to it). With a separate graphics card (AMD Radeon,
NVIDIA GeForce, Intel Arc) the model runs on it: loading takes several seconds, then each look
about 1–2 seconds. With only the graphics built into the processor (such as Intel's
built-in graphics) it runs on the processor:
about 8–30 seconds a look. The answer starts that much later. The model stays loaded for two
minutes after a look, so follow-up questions don't load it again; then the memory is freed.
Asking the same question about the same frame again reuses the look. A later question about the
shot that isn't about exposure, colour or skin ("what would you do with this shot?") doesn't
start a new look: it gets the last look of that shot, if there is one. Change the clip's grade or
effects and it counts as a new shot. If
the look fails, the assistant answers without it. Nothing leaves the computer. Scripts: it
happens with `context: "auto"`; the line reaches the model as `Picture (seen):`. Resolve and
Premiere have nothing like it. [Fork]

## The Assistant panel

> id: color.coach.assistant-panel | does: a chat with the assistant in a panel you can float anywhere or dock beside any panel | ui: assistant.open, assistant.input, assistant.send, assistant.new | cmd: coach.assist, coach.assistResult, coach.assistForget | when: you want to ask several questions in a row while you work | see: color.coach.assistant

**[Fork]**

The same assistant as **Also ask the assistant**, as a chat that stays open while you work.
It answers about all of Revolution (grading, editing, audio, compositing, export), from this guide,
on your computer.

1. Click **Assistant** {ui:assistant.open} in the page bar at the bottom, left of Home (Premiere
   workflow: the header at the top right). The panel opens floating, left of where the coach's
   cards appear.
2. Type a question in the box at the bottom {ui:assistant.input} and press Enter or **Send**
   {ui:assistant.send} (Shift+Enter starts a new line). The answer appears as it is written, then
   is checked the same way as in the Ask box; **Stop** ends it early.
3. Click an answer line that names a control: the control is outlined (click it again to remove
   the outline). **Show me: …** starts the lesson for it.
4. Each question follows up on the last answer. **New chat** {ui:assistant.new} clears the chat
   and starts a new topic.
5. Put it where it suits you. Drag the panel by its **Assistant** tab: drop it in the middle of
   another panel to make it a tab there, or on a panel's edge to split that space. For example,
   on the Color page between **Curves** and **Primaries**, beside the **Clips** strip, or on the
   Fusion page between the timeline and the media pool. Drag the tab out again to float it.
   Saving the workspace (or the page layout) keeps it there.
6. Click **Assistant** again to close the panel (an answer being written stops). The chat isn't
   saved with the project.

What it knows about your shot: with each question (from the panel or the Ask box) the assistant
is told which clips are selected and, when the selected clip has a grade, what its current node
does to the frame under the playhead, measured from the picture (for example "lifts the mids
about a third of a stop, a little warmer"). It also gets a short measured fact sheet of the frame
under the playhead, before and after the grade, read the way the scopes would: where the mids sit
against middle grey, how much is clipped or crushed, the contrast, the colour of near-neutral
areas (in words and roughly in kelvin), saturation, and how much of the frame has skin-tone
colours and where they sit against the skin line. It also says whether the clip is log footage
(measured after its input transform) or display-referred. Select the clip and node you're asking about
first so the answer fits it (with nothing selected, the top clip under the playhead is measured).
These are numbers turned into words, so "skin-tone colours" can be a sunset or a wooden table,
not only a face; with the picture model installed (see "Looking at the picture" above), picture
questions also get a look at the frame itself. `coach.assist` takes
`"context": "auto"` for this; scripts can still pass `selection` / `gradeFacts` / `pictureFacts`
themselves.

Options: the Ask box is quicker for a single question and shows the guide's steps at once; the
panel suits a conversation, or keeping answers next to the controls they name. A beginner may
find a floating panel easiest; docking it beside the panels you use most keeps it out of the way
of the picture. When the model isn't installed, each answer says so and shows the guide's own
answer instead, with a **Set up the assistant's model (Settings ▸ Color)** link
that opens Settings ▸ Color, where the model is downloaded
{ui:settings.color.ai.download}.

## Where the Assistant runs: graphics card or processor

> id: color.coach.assistant-where | does: says whether the Assistant's AI model runs on your graphics card (GPU) or your processor (CPU), and how to check | cmd: coach.assist | when: is the AI / Assistant running on my graphics card or GPU, why is it slow, how much video memory does it use | see: color.coach.assistant, color.coach.assistant-panel

**[Fork]**

**Why it matters:** the same answer takes about 2–4 seconds on a separate graphics card and 20–40
seconds on the processor (CPU), so the Assistant is slow there.

1. With a separate graphics card (an AMD Radeon, NVIDIA GeForce or Intel Arc) the Assistant's model
   runs on it automatically. With only the graphics built into the processor it runs on the
   processor instead, because built-in graphics are usually slower for this.
2. To check: answers that start within about a second and finish in a few seconds mean the
   graphics card. Revolution's log file (in the **Logs** folder of Revolution's data folder) says
   "LLM on the GPU" with the card's name, or "LLM: on the CPU" with the reason.
3. While loaded the model takes about 2.4 GB of the graphics card's memory; it is loaded when you
   ask and shares the card with playback and rendering.
4. If the graphics card fails mid-answer, Revolution carries on with the processor rather than stop.
   (Testers: `FILMCRAFT_LLM_GPU=0` forces the processor, `=1` also allows built-in graphics.)

## Assistant retrieval by meaning

> id: color.coach.assistant-search | does: an optional embedding model that ranks guide sections by meaning for the Assistant, mixed with the word ranking | cmd: coach.assist | when: "can the Assistant understand other wordings" | see: color.coach.assistant

**[Fork]** · Resolve and Premiere have no built-in assistant.

1. By default the Assistant ranks guide sections by shared words, like Ask the coach.
2. Optional, being measured: an embedding model on this computer (BAAI bge-small-en-v1.5,
   MIT, about 130 MB, models folder `embed/bge-small-en-v1.5`) also ranks them by meaning; the
   word ranking's best three stay first and meaning fills the rest.
3. To try it, start Revolution with `FILMCRAFT_ASSIST_SEARCH=mixed` (or `words`, `meaning`). The
   first answer prepares every section once (minutes); later starts reuse that.

It becomes the default only if the Assistant's measured answers improve with no lookups lost.

## Rate an answer and help improve the Assistant

> id: color.coach.assistant-share | does: rates an Assistant answer and, only if you choose, shares it as an example (on GitHub, publicly) or saves it to a file | ui: coach.ask.rate.up, coach.ask.rate.down, coach.ask.rate.correction, coach.ask.rate.share, share.preview, share.grade, share.picture, share.frame, share.consent, share.github, share.save, share.close | cmd: coach.feedback, coach.shareExample | when: the Assistant got something wrong or right and you want it to do better, or you want to keep an answer | see: color.coach.assistant, color.coach.assistant-panel

**[Fork]**

**Why it matters:** the Assistant learns from real questions with a note of what a good answer
would have been. Revolution never sends anything by itself and has no server: an answer leaves
your computer only when you open the GitHub page and submit it with your own account.

You have three options, each fine:

- **Share on GitHub** (public): helps everyone's Assistant, including models released openly.
  Read the preview first; you must tick that you agree.
- **Save to a file**: keep it, check it, or send it another way. No agreement needed.
- **Neither**: a 👍 / 👎 stays on your computer with the answer and goes nowhere.

1. Under any Assistant answer (in the **Assistant** panel or the **Ask the coach** box), click
   👍 {ui:coach.ask.rate.up} or 👎 {ui:coach.ask.rate.down}. A box **What should it have said? (optional)** {ui:coach.ask.rate.correction} appears: write the better answer if you
   like. Both stay with the answer on your computer (`coach.feedback`).
2. Click **Help improve the Assistant…** {ui:coach.ask.rate.share}. The dialog shows the preview {ui:share.preview}: exactly the text that is
   shared or saved, nothing more (`coach.shareExample`). It holds the question, the answer, the
   guide sections it used, your rating and note, the Revolution version and the page. Project, bin,
   clip and file names, your user and computer names, folder paths, network addresses and e-mail
   addresses are replaced by stand-ins such as `<clip>` and `<path>`.
3. Tick extras only if they help: **Include the grade's settings** {ui:share.grade}, **Include the measured picture
   facts** {ui:share.picture} and **Include a small frame from my footage (saved as an image)** {ui:share.frame}. All three start off;
   the preview updates as you tick.
4. To share: tick **I agree** {ui:share.consent} (it will be public on GitHub and used to train and improve
   Revolution's Assistant, including models released openly), then **Open on GitHub** {ui:share.github}. Your browser
   opens a pre-filled *New issue* page on the fork's GitHub with the label `assistant-example`;
   check it there and submit it with your own account. A frame can't go in a link: save it (next
   step) and drag the image into the issue. An example too long for a link becomes a short note
   asking you to paste the saved file.
5. To keep it or send it another way: **Save example…** {ui:share.save} and pick a folder (no agreement needed). It
   writes `assistant-example-<id>.md`, and the frame as a `.png` beside it (named in the text). A
   second save never overwrites the first.
6. **Close** {ui:share.close} when done. Nothing has left your computer unless you submitted the GitHub page.

From a script: `coach.feedback {id, rating: up | down | none, correction}`;
`coach.shareExample {id, route: preview | github | file, grade, picture, frame, consent, folder}`
(`github` needs `consent: true` and returns the `url`; `id` is the answer's, from `coach.assist`).

**[Fork]** The owner's build also has **Send to my training inbox** in the dialog: the whole
conversation, unstripped, goes to the `assistant-inbox` folder in Revolution's data folder on this
computer (nothing goes over the network), for the fork's own training runs (`route: inbox`).

Differs from Resolve and Premiere: neither has a local assistant, so there is nothing to compare.

