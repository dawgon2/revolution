# Proxies

**[Fork] [Revolution]** · smaller copies of heavy clips that Revolution makes by itself and plays
instead of the originals; written for Phase 6.2 (automatic proxies)

## What a proxy is, and why

> id: proxies.what | does: a smaller copy of a clip shown while editing so heavy footage plays smoothly | resolve: Proxy media / Optimized media | premiere: Proxies | when: 6.2K or other footage the computer can't play live | see: proxies.auto

**[Revolution]**

A **proxy** is a smaller, easier-to-decode copy of a clip. While you edit, Revolution can show the
proxy instead of the original, so playback stays smooth on footage the computer can't decode at
full speed. Your edit, your grade and your export are not affected: an **export always reads the
original files**, at full quality. A clip with a proxy shows a small **P** badge in the Project
panel.

If you know Premiere: this is the same idea as Premiere's proxies (Clip ▸ Proxy ▸ Create Proxies,
the Toggle Proxies button). If you know Resolve: it is close to Resolve's proxy media / optimized
media.

## Automatic proxies

> id: proxies.auto | does: queues proxies in the background for clips that need them, on import or open | ui: settings.media.autoProxies | cmd: media.autoProxies | resolve: — (proxies are made by hand) | premiere: Ingest settings ▸ Create Proxies | when: every project with heavy footage | see: proxies.background

**[Fork]**

You don't have to make proxies yourself. When you import a clip, or open a project, Revolution
looks at each clip and queues a proxy in the background when it is one of these:

- **4:2:2 H.265 or H.264** (an X-M5 occasional setting). The graphics card's video engine can't
  decode these, so they are decoded in software, which is slow at 6.2K. They **always** get a
  proxy (unless proxies are switched off, below). Until the proxy is ready a 4:2:2 H.265 clip
  shows an amber **4:2:2** badge; hovering it says "4:2:2: software decode, proxies recommended".
- **4K or larger long-GOP clips** (H.265 / H.264, such as 6.2K X-M5 footage), while
  **Create proxies automatically for 4K and larger long-GOP clips** is ticked (it is by default).

Clips that already decode quickly (ProRes, DNx, smaller H.264) are left alone. This happens in
the desktop app only: the command-line tool and headless sessions make proxies only when asked
(`media.createProxies` with `preset` `auto`, below).

A finished proxy is attached by itself. Attaching it is not an edit: it adds nothing to the undo
history, and Undo / Redo keep the proxy in place.

The proxy is half the size of the original, H.265 10-bit made on the graphics card's video
engine. If the video engine can't encode, Revolution makes a ProRes 422 Proxy (half size) instead,
by itself, without asking. Proxies have no sound: the sound always plays from the original.

Proxies are written to a **Proxies** folder next to the media (or under Project Settings ▸ Scratch
Disks ▸ Captured and Generated, when that is set). Their names look like
`A001_Proxy.0123456789abcdef.mov`: the 16 characters are a fingerprint of the original file, so
Revolution knows which file a proxy was made from. Importing the same file again, or opening it in
another project, reuses the proxy instead of making it again.

## Never in the way

> id: proxies.background | does: the proxy worker pauses while you play, scrub or export | when: wondering whether proxies slow editing | see: proxies.progress

**[Fork]**

Proxies are made by a background worker that steps aside whenever you are working:

- While you **play**, **scrub** or **export**, it finishes the few frames in hand and waits.
- It starts again half a second after you stop.
- It also waits while the computer is short of memory (under 3 GB free, or a quarter of its
  memory), so on a 16 GB computer it backs off sooner; the render cache does the same.
- It makes one proxy at a time, 4:2:2 clips first.

So making proxies never makes playback drop frames; it only takes longer when you keep playing.

## Watching the progress

> id: proxies.progress | does: badges in the Project panel show each clip's proxy state | cmd: media.proxyStatus | resolve: Proxy status column | premiere: Proxy column | when: checking which clips have proxies; what the little P (or a red P !) on a clip means | see: proxies.mode

**[Fork]**

The Project panel shows each clip's state next to its name (List view) or in the top-right corner
of its card (Icon view):

| Badge | Meaning |
|---|---|
| **P …** (grey) | the proxy is queued |
| **P 42%** (grey) | the proxy is being made (percent done) |
| **P** (blue) | the proxy is ready and in use |
| **P** (grey) | the proxy is ready, but proxies are switched off |
| **P !** (red) | the proxy could not be made; hover it for the reason |
| **4:2:2** (amber) | 4:2:2 H.265 with no proxy yet: decoded in software, proxies recommended |

Hover any badge to read its meaning. Agents and scripts read the same thing with
`media.proxyStatus`: counts of queued, building, failed and ready proxies, overall progress, and
one row per clip.

## Choosing when proxies are used

> id: proxies.mode | does: chooses whether playback, the timeline and export read proxies or the originals | ui: settings.media.proxies, program.transport.media.toggleProxies | cmd: media.proxyMode, media.toggleProxies, app.settings.media | resolve: Playback ▸ Use Proxy Media | premiere: Toggle Proxies button | when: comparing against the original; exporting | see: proxies.manual

**[Fork]**

Settings ▸ Media ▸ **Proxies** decides which frames read a proxy:

| Choice | What you see |
|---|---|
| **Auto (proxies while playing, original when paused)** (default) | playback and scrubbing use the proxy (smooth); when you stop, the frame on screen is the original (sharp, for judging focus and grading) |
| **Always use proxies** | every frame in the monitors uses the proxy |
| **Never use proxies** | proxies are kept but not used, and no new ones are queued |

To change it:

1. Choose Edit ▸ Preferences ▸ **Media…** (`app.settings.media`). The Preferences window opens
   on the Media page.
2. Pick a choice in **Proxies** {ui:settings.media.proxies} (`media.proxyMode`, with `mode`
   `auto`, `on` or `off`).
3. Tick or untick **Create proxies automatically for 4K and larger long-GOP clips**
   {ui:settings.media.autoProxies} (`media.autoProxies`). Ticking it queues proxies for the large
   clips already in the project.
4. Press **OK** {ui:settings.ok}.

The monitor's **Toggle Proxies** button {ui:program.transport.media.toggleProxies}
(`media.toggleProxies`) is a quick switch between **Always use proxies** (lit) and **Never use
proxies**. From Auto, one click turns proxies on for every frame; Auto comes back from the
Preferences window.

Unlike Premiere, where Toggle Proxies is the only switch, Revolution's default (Auto) uses proxies
only while the picture moves.

## Making or stopping proxies yourself

> id: proxies.manual | does: makes, attaches or cancels proxies for chosen clips | cmd: media.createProxies, media.attachProxies, media.cancelProxies | resolve: Generate Proxy Media | premiere: Proxy ▸ Create Proxies / Attach Proxies | when: clips the automatic rules skipped; stopping the worker | see: proxies.auto

**[Fork] [Revolution]**

To make the automatic proxy for clips that didn't get one:

1. Select the clips in the Project panel.
2. Run `media.createProxies` with `preset` `auto` (the proxy goes to the background worker like
   the automatic ones). Clip ▸ Proxy ▸ **Create Proxies…** (`media.createProxies`) still makes
   proxies with the other presets (ProRes or H.264, quarter or half size) as a foreground job.

To stop proxies that are queued or being made:

1. Run `media.cancelProxies` (all of them), or with `items` for some clips. The half-written files
   are deleted.

## When the original changes

> id: proxies.relink | does: a relinked or replaced original gets a fresh proxy; the old one is detached | when: relinking media, replacing a file on disk | see: proxies.manual

**[Fork]**

If you relink a clip to another file (File ▸ Link Media…), or the file on disk was replaced, the
fingerprint in the proxy's name no longer matches. Revolution then detaches the old proxy and makes
a new one, so a proxy never shows pictures from a different take. Proxies you attached yourself
(Clip ▸ Proxy ▸ **Attach Proxies…**, `media.attachProxies`) are never touched.
