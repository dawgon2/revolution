# Keyboard shortcuts and workspaces

**[Revolution]** · keyboard shortcut presets (Revolution, Premiere Pro, Final Cut Pro, Avid and,
in this fork, DaVinci Resolve keys), changing and sharing shortcuts, and the workspaces of the
Premiere workflow; written for AI-PLAN AI3-PREP (feature pages)

## Shortcut presets

> id: shortcuts.presets | does: switches the whole keyboard to Revolution's, Premiere's, Final Cut's, Avid's or Resolve's keys | cmd: shortcuts.presets, shortcuts.loadPreset, shortcuts.list | resolve: Keyboard Customization presets | premiere: Keyboard Shortcuts ▸ keyboard layout preset | when: your hands know another editor | see: shortcuts.change, getting-started.workflow

**[Revolution]** **[Fork]** (the Resolve preset)

Edit ▸ **Keyboard Shortcuts…** opens the keyboard editor. Pick a preset (`shortcuts.loadPreset`;
`shortcuts.presets` lists them):

| Preset | Good for | Trade-offs |
|---|---|---|
| **Revolution Default** | New users; each command's own key, plus Premiere's keys where they are free | Mostly like Premiere, not exactly |
| **Premiere Pro Compatible** | Premiere users | Premiere's keys even where Revolution has its own |
| **Final Cut Pro Compatible** / **Avid Media Composer Compatible** | Final Cut or Avid users | Only commands Revolution has are bound |
| **DaVinci Resolve Compatible** (fork) | Resolve users; switching to the Resolve workflow selects it (and switching back restores your previous preset) | Only keys from Resolve's public keyboard reference, for commands Revolution has |

Keys are written once for every system: **Cmd** is ⌘ on macOS and Ctrl on Linux and Windows.

## Changing a shortcut

> id: shortcuts.change | does: binds a key to a command, application-wide or for one panel, and finds conflicts | cmd: shortcuts.set, shortcuts.clear, shortcuts.get, shortcuts.forKey, shortcuts.conflicts, shortcuts.undo, shortcuts.redo | resolve: Keyboard Customization | premiere: Keyboard Shortcuts editor | when: a command you use all the time has no key, or a key you want is taken | see: shortcuts.share

**[Revolution]**

1. Search for the command in the keyboard editor and press the new key in its row
   (`shortcuts.set`); remove a key with `shortcuts.clear`.
2. A key can work everywhere or only while one panel has the focus (a **panel shortcut**, which
   wins over the application one inside that panel).
3. Taken keys are shown as conflicts (`shortcuts.conflicts`); `shortcuts.forKey` tells what a key
   does now, `shortcuts.get` what keys a command has. Changes in the editor can be undone there
   (`shortcuts.undo`, `shortcuts.redo`).

## Saving and sharing your keys

> id: shortcuts.share | does: saves your keys as a named preset, exports and imports preset files | cmd: shortcuts.savePreset, shortcuts.deletePreset, shortcuts.export, shortcuts.import | resolve: Keyboard Customization ▸ Export / Import | premiere: Keyboard Shortcuts ▸ Save As, preset files | when: moving to another computer; sharing a layout with a team | see: shortcuts.change

**[Revolution]**

**Save As** a preset (`shortcuts.savePreset`) keeps your current keys under a name; presets are
files in your data folder (Keyboard Shortcuts), and the keys in use are restored at every launch.
`shortcuts.export` writes a preset file, `shortcuts.import` reads one; a file made on Windows is
converted so its Ctrl becomes the main modifier.

## Workspaces (Premiere workflow)

> id: shortcuts.workspaces | does: switches between panel layouts for each task and saves your own | cmd: window.workspace.colorgrading, window.workspace.saveChanges, window.workspace.saveAs, window.workspace.reset, window.workspace.delete | resolve: pages (see pages.md) | premiere: Window ▸ Workspaces | when: changing task: cutting, colour, sound, graphics | see: pages.layout

**[Revolution]**

In the Premiere workflow, Window ▸ **Workspaces** switches the panel layout: **Editing**,
**Assembly**, **Color** (Lumetri), **Color Grading** (the node grader), **Audio**, **Effects**,
**Captions and Graphics**, **Review**, **Learning** and **All Panels**. Move or resize panels and
**Save Changes to This Workspace** (`window.workspace.saveChanges`), **Save as New Workspace…**
(`window.workspace.saveAs`), **Reset to Saved Layout** (`window.workspace.reset`) or delete one
you made (`window.workspace.delete`). A workspace keeps unsaved changes when you switch to another
one and back, and after you quit, until you reset it. In the Resolve workflow the same idea is the page bar
([pages.md](pages.md)).
