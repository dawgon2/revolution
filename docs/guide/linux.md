# Linux desktop integration

**[Standard] [Revolution]** · how Revolution fits into a Linux desktop (KDE Plasma, GNOME and
others, Wayland or X11): where its files live and which desktop services it uses

## Installing: a distribution package, Flatpak, or the tarball

> id: linux.install | does: the ways to install Revolution on Linux and what each can do | resolve: — (Linux installer) | premiere: — (no Linux version) | when: installing or choosing a package | see: linux.files

**[Standard]**

Why: every way of installing gives you the same editor, but they differ in what Revolution can
reach on your computer and in how it gets updated. None is the one right choice.

- **Your distribution's package** (Arch / CachyOS: `packaging/linux/arch/PKGBUILD` with
  `makepkg -si`; Debian / Ubuntu / Fedora: the release's `.deb` / `.rpm`; steps for each in
  [install](install.md)). Runs like any other
  program: it sees every drive and folder you can, logs go to the system journal, and it uses
  the system's own video drivers. Good if you mostly use one machine and want it to feel native.
  Updates come when you rebuild or install the newer package.
- **Flatpak** (the release's `.flatpak`, `flatpak install --user <file>`). Runs in a sandbox on
  any distribution, with its own copy of the system libraries and video drivers: hardware
  decoding, audio, media keys, notifications and KDE Connect work inside it, and it may eject a
  camera card. It can read and write your home folder and external drives (`/media`,
  `/run/media`, `/mnt`); footage anywhere else on the system isn't visible. Unlike upstream
  FilmCraft's Flatpak it has network access, for the update check, model downloads and sharing an
  answer as a GitHub issue (to take it away: `flatpak override --user --unshare=network
  io.github.dawgon2.Revolution`). Logs go to the log file only (no journal):
  `~/.var/app/io.github.dawgon2.Revolution/.local/state/filmcraft/Logs/`. Good if
  you want it isolated, or your distribution has no package. The shared runtime is a few hundred
  MB the first time.
- **The portable folder or AppImage.** Unpack (or mark the AppImage executable) and run;
  nothing goes system-wide. Good for trying a release, or keeping two versions side by side; the
  portable folder is also the one the in-app updater replaces by itself.

## Where Revolution keeps its files

> id: linux.files | does: settings, data, cache and logs in the standard XDG folders | cmd: app.paths, mediaCache.info | when: backing up settings, freeing cache space | see: linux.install

**[Standard]**

Why: Linux desktops agree on a few standard folders (the freedesktop "XDG Base Directory"
specification), so backups, disk-cleaning tools and other people's habits all know what is safe
to delete and what isn't. Revolution follows it when it uses its default per-user folder:

| What | Folder (default) | Safe to delete? |
|---|---|---|
| Auto-save, crash recovery, presets, keyboard shortcuts, templates, downloaded models, settings | `~/.local/share/filmcraft` (`$XDG_DATA_HOME`) | no: this is your data |
| Media cache (conformed audio, waveforms, render previews) | `~/.cache/filmcraft/Media Cache` (`$XDG_CACHE_HOME`) | yes: Revolution rebuilds it |
| Log files and crash logs | `~/.local/state/filmcraft/Logs` (`$XDG_STATE_HOME`) | yes, but attach them to bug reports first |
| The window's size (`window.json`) | `~/.local/state/filmcraft` (`$XDG_STATE_HOME`) | yes: the next start uses the default size |

If you start Revolution with an explicit folder (`--data-dir <folder>`, the `FILMCRAFT_DATA_DIR`
variable, or a portable install), **everything** stays inside that one folder, as before. On
macOS and Windows Revolution uses its one per-user folder for everything.

1. To see the folders this session uses, run the command `app.paths` (it lists the data, config,
   cache and state folders, where logs go and where the media cache is).
2. To open the log folder, use **Help ▸ Reveal Log Files**.
3. To put the media cache somewhere else (a faster or bigger drive), set **Settings ▸ Media
   Cache ▸ Location**; `mediaCache.info` shows where it is and how big.

An install that ran an older version had its logs and media cache in
`~/.local/share/filmcraft/Logs` and `~/.local/share/filmcraft/Media Cache`. At the first start
Revolution moves those two folders to the new places (a rename: nothing is copied or deleted). If
a folder can't be moved (for example the cache folder is on another drive), Revolution keeps
using it where it is, so **Media Cache Info** and **Delete…** still see its files; the log says
which happened.

## The window's size and title bar

> id: linux.window | does: the window reopens at its size and place; the title bar can be the desktop's or Revolution's | when: setting up the window for your screen; why Revolution does not remember which monitor or position it was on | see: linux.fullscreen

**[Standard]**

Why: an editor is usually set up once for a screen; it should open the way you left it.

1. Resize or maximise the window, then quit Revolution.
2. The next start opens at that size, maximised again if it was. Un-maximising returns to the
   size you had before maximising.

Only the size and the maximised state are kept, not the position: on Wayland the compositor
(KWin, Mutter…) decides where windows go, and KWin's own **Window Rules** (System Settings ▸
Window Management) can pin Revolution to a screen or position if you want that. Full screen is a
setting of its own (next section). To go back to the default size, delete `window.json` in the state folder (see the
table above; with `--data-dir` it is in that folder) while Revolution is closed. Agents
(`--control`) always start at the default size and don't change the file.

The title bar comes from the desktop where it offers one (KDE Plasma, most X11 window managers),
so it matches your theme and its buttons. GNOME on Wayland leaves title bars to the app; there
Revolution draws a plain Adwaita-style frame with the window buttons (no title text; the
window's name still shows in the overview and the dock).

## Full screen

> id: linux.fullscreen | does: Revolution opens full screen by default, like Resolve | resolve: full-screen interface | when: wanting every pixel, or a normal window | see: linux.window

**[Standard]**

Why: an edit wants every pixel. Like DaVinci Resolve, Revolution opens **full screen** by default:
the window covers the desktop's panels and task bar.

- **Switch it** with the full-screen button at the top right (the four corners, left of the
  minimise button), **F11**, **View ▸ Full Screen** or Ctrl+`. The choice is remembered, so the
  next start opens the way you left it. Settings ▸ General ▸ **Full screen** is the same switch.
- **When to leave it on:** a single screen used only for editing, or a laptop, where the panels
  cost real space.
- **When to turn it off:** you work next to other apps (a browser, a script, a chat), use the
  task bar a lot, or want to see notifications. A maximised window keeps the panels.
- **The Project Manager remembers its own state** (as in DaVinci Resolve): by default it floats in
  a window while the editor stays full screen, so opening a project from it brings the project up
  full screen. Switch either one while it is showing (the button or F11 act on the screen you are
  on), or set them in Settings ▸ General ▸ **Full screen** and **Project Manager full screen**.
  Closing a project (File ▸ Close Project) returns to the Project Manager.
- In full screen and on the Project Manager only the full-screen and minimise buttons show; quit
  with **Revolution ▸ Quit Revolution** (Ctrl+Q). The Project Manager's **×** (top left) goes back to
  the open project, or into a new untitled project when none is open.
- **Revolution ▸ Preferences…** opens Settings.
- If the desktop takes the window out of full screen (a KWin shortcut, the window menu), the
  setting of the screen you are on follows. Agents (`--control`) always start windowed.

## The menu in the desktop's global menu

> id: linux.global-menu | does: the menu bar can appear in the desktop's global menu | when: a macOS-style panel layout | see: linux.window

**[Standard]**

Why: with a macOS-style layout (Plasma's **Global Menu** widget in a top panel, or a title-bar
menu button) every app's menu lives in the panel; Revolution's should too.

- Settings ▸ General ▸ **Menu bar ▸ In the desktop's global menu when it has one** (the default):
  when Plasma shows a global menu, Revolution's menus (File, Edit, Clip, …, with their shortcuts,
  checkmarks and greyed-out items) appear there and the window's own menu bar is hidden. Add or
  remove the Global Menu widget and Revolution follows at once.
- **In full screen** the panel is covered, so the menu bar comes back inside the window.
- **Menu bar ▸ In the window:** always keep the menu in the window (and nothing in the panel),
  for example if you prefer the menu next to the Resolve-style page buttons.
- Works on KDE Plasma under Wayland. Elsewhere (GNOME, Plasma on X11, no Global Menu widget) the
  menu simply stays in the window.

## The desktop's accent colour

> id: linux.accent | does: Revolution can use the desktop's accent colour | cmd: appearance.followSystemAccent | when: matching other apps' highlight colour | see: linux.text-size

**[Standard]**

Why: KDE Plasma and GNOME let you pick an accent colour once for every app. Revolution can use
it as its highlight colour (selections, primary buttons, the focus outline, the playhead,
timecodes and track targeting).

1. Open **Settings ▸ Appearance** and turn on **Use the system accent color**.
2. Click **OK**. The highlight becomes the desktop's accent, and changes with it while Revolution
   runs (Plasma: System Settings ▸ Colors & Themes ▸ Colors ▸ Accent color; GNOME 47 and later:
   Settings ▸ Appearance).

Options and trade-offs:

- **Off (default):** Revolution keeps its own look: the Premiere themes' blue, the Resolve
  theme's red-orange, or the **Highlight Color** you picked. Good when you match colours by eye
  and want the interface to stay the same from machine to machine, or when you follow a
  tutorial whose screenshots show the default colours.
- **On:** Revolution matches the rest of your desktop. In the Resolve workflow this replaces the
  Resolve theme's accent too. A very light or very dark accent can make selections harder to
  see on the matching theme; switch it off, or pick a **Highlight Color** yourself, if it does.

When the desktop reports no accent colour (older desktops, window managers without the XDG
desktop portal), the **Highlight Color** applies. Agents can read and set it as
`appearance.followSystemAccent`.

## Animations and reduced motion

> id: linux.motion | does: interface animations follow the desktop's reduced-motion setting | cmd: appearance.motion | when: motion makes you uncomfortable, or you want it snappier | see: linux.accent

**[Standard]**

Why: menus, collapsing sections and scrolling to a clip slide into place for a tenth of a
second. Some people find that motion distracting or uncomfortable, and many desktops have one
switch to turn it off in every app.

**Settings ▸ Appearance ▸ Animations** has three choices:

- **Sync with system (default):** animations are on unless the desktop asks for reduced
  motion, and Revolution follows a change while it runs. On KDE Plasma, drag System Settings ▸
  Workspace ▸ General Behavior ▸ **Animation speed** all the way to **Instant**; on GNOME 46
  or later, turn on Settings ▸ Accessibility ▸ Seeing ▸ **Reduce Animation** (older GNOME:
  GNOME Tweaks ▸ General ▸ **Animations** off). Newer desktop portals also pass on a
  reduced-motion setting of their own, which then decides. Good if you set it once for the
  whole desktop.
- **On:** always animate, whatever the desktop says. Useful when you turned desktop animations
  off for speed (a slow machine, a remote session) but like the small cues in Revolution.
- **Off (reduce motion):** never animate; everything jumps straight to its new place. Pick it on
  desktops that don't report the setting (other window managers; Windows and macOS, where
  Revolution doesn't read it yet) or if you want an instant interface in Revolution only.

Playback, scrubbing and the playhead are never affected: they show your media, not interface
motion. Agents can read and set it as `appearance.motion` (`system` | `on` | `off`).

## The desktop's text size

> id: linux.text-size | does: Revolution can follow the desktop's text size | cmd: appearance.matchDesktopTextSize | when: you made desktop text bigger | see: linux.accent

**[Standard]**

Why: many people make text bigger once for the whole desktop instead of zooming every app. KDE
Plasma keeps it as the font size (System Settings ▸ Text & Fonts, 10 pt by default); GNOME as a
text scaling factor (Settings ▸ Accessibility ▸ Seeing ▸ **Large Text**, or GNOME Tweaks ▸ Fonts
▸ **Scaling Factor**). Revolution can follow it.

1. Open **Settings ▸ Appearance**, leave **UI Scale** on **Auto (system)** and turn on **Match
   the desktop's text size**.
2. Click **OK**. The interface grows by the same factor (an 11 pt Plasma font gives 110 %, GNOME's
   Large Text 125 %), and follows a change while Revolution runs.

Options and trade-offs:

- **Off (default):** UI Scale ▸ Auto uses the display scale only. Revolution's panels are dense;
  this keeps the most timeline and the most monitor on screen, and matches tutorials and
  screenshots.
- **On:** Revolution matches what you set for every app. It scales the **whole interface**, not
  only text: panels, buttons and the timeline grow too, so you see fewer tracks and a smaller
  monitor. Good if you set larger text for comfort or eyesight and want Revolution to follow
  without a second setting. The factor is held between 75 % and 200 %.
- **A fixed UI Scale (75 % … 300 %):** one size for Revolution whatever the desktop says; this
  switch is then ignored. Good if only Revolution needs to be bigger or smaller, or when you
  move between machines and want the same layout on each.

**Ctrl+−** / **Ctrl+=** still zoom for the session; a change of the desktop's text size (or of
the display scale) replaces that zoom with the new size. Desktops that don't report a text size
(other window managers; Windows and macOS for now) behave as if the switch were off. On GNOME
under X11 the desktop already folds Large Text into the display scale, so the switch adds
nothing there (it would otherwise count twice).
Agents can read and set it as `appearance.matchDesktopTextSize`.

## The app icon: rounded or square

> id: linux.app-icon | does: choose the app icon's shape, the rounded macOS-style tile or the plain square picture | cmd: appearance.appIcon | when: "can I change the app icon?"; the launcher or taskbar icon looks wrong next to your other icons | see: linux.accent

**[Fork]**

Revolution's icon is a rounded tile with a little margin and a soft shadow, the shape macOS
uses, so it sits well beside other app icons on most docks and panels. **Settings ▸ Appearance ▸
App icon** switches it:

- **Rounded (macOS style)** (default): the icon the packages install.
- **Square**: the same picture as a plain square, edge to edge. Some panels and icon themes
  (square or flat ones) look tidier with it.

The window's own icon changes at once. On Linux the launcher, taskbar and app-switcher icon come
from your icon theme, so Revolution also writes the chosen icon to your user icon folder
(`~/.local/share/icons/hicolor`). Some desktops cache icons: if the old one stays, it changes
after you log out and back in. In a Flatpak only the window icon changes (the sandbox can't
write there). On Windows and macOS the shortcut and Dock icons stay rounded. Agents can read
and set it as `appearance.appIcon` (`rounded` | `square`).

## Progress on the taskbar and a notification when a job finishes

> id: linux.progress | does: export and proxy progress on the taskbar icon, and a notification when done | when: working in another app during a long export | see: linux.inhibit

**[Standard] [Revolution]**

Why: an export or a batch of proxies can take a while; you shouldn't have to keep Revolution in
front to know when it's done.

- While a job runs (an export, Create Proxies, the Project Manager…), its progress shows on
  Revolution's **taskbar entry** (KDE Plasma's task manager and docks that read the
  `com.canonical.Unity.LauncherEntry` signal; GNOME shows it only with a dock extension).
- When it finishes, a desktop **notification** says so (or why it failed). Click **Show in
  Folder** to open the file manager with the exported file selected.
- You get the notification when Revolution is in the background, or when the job ran 30 seconds
  or more even if Revolution is in front. A cancelled job never notifies; preview renders don't
  either (the timeline shows them).

1. Start an export (**File ▸ Export ▸ Media…**, `file.exportMedia`) and switch to another window.
2. Watch the taskbar entry fill; when the notification appears, click **Show in Folder**.

A desktop without these services simply shows nothing; Revolution works the same. A Revolution
started for an agent (`--control`) leaves them off unless `FILMCRAFT_DESKTOP_INTEGRATION=1` is
set.

## The screen stays on while you play, the computer stays awake while you export

> id: linux.inhibit | does: keeps the screen on during playback and the computer awake during exports | when: long playback or overnight exports | see: linux.progress

**[Standard]**

Why: watching a cut for ten minutes without touching the mouse shouldn't lock the screen, and
the computer shouldn't go to sleep halfway through a long export.

- While the Program monitor plays, Revolution asks the desktop not to blank or lock the screen.
- While a job runs, it also asks the computer not to suspend.
- The request is dropped the moment playback stops or the job ends. It goes through the desktop
  portal (`org.freedesktop.portal.Inhibit`; works in Flatpak too), or the older
  `org.freedesktop.ScreenSaver` interface for the screen only.

## One Revolution at a time

> id: linux.single-instance | does: a second launch hands its files to the running Revolution | when: double-clicking a project while Revolution is open | see: linux.project-files

**[Revolution]**

Why: two copies of Revolution on the same settings and auto-save folder would fight over them,
and double-clicking a project in the file manager should open it in the window you already
have.

1. With Revolution running, double-click a `.fcproj` file (or choose **Open With ▸ Revolution**
   on media files) in the file manager.
2. The running Revolution opens the project (or imports the media) and its taskbar entry asks for
   attention; click it to bring the window forward. (Wayland doesn't let an app jump in front by
   itself.)

Each data folder has its own "one at a time": the fork's launcher (`--data-dir`) and a packaged
install can run side by side. To start a second window anyway, launch with `--new-instance`.
Agents (`--control`) always get their own.

## File dialogs: the desktop's own, and where they start

> id: linux.dialogs | does: Open, Import, Save and Export use the desktop's file dialog and remember folders | cmd: file.import, file.exportMedia | when: a dialog opens in the wrong place | see: linux.files

**[Standard] [Revolution]**

Why: Open, Import, Save and Export use your desktop's own file dialog (KDE's on Plasma, GNOME's
on GNOME, through the XDG desktop portal), so your Places, bookmarks and network folders are all
there.

- **Revolution keeps running while a dialog is open.** Playback, a running export and agents on
  the control port carry on; the dialog's answer is used when you click Open or Save. One file
  dialog at a time: asking for a second shows "A file dialog is already open" in the status line.
- **The dialog belongs to the Revolution window.** It opens over it, stays over it, and moves to
  the same virtual desktop (on Wayland the compositor needs `xdg-foreign`, which KWin and Mutter
  have; elsewhere the dialog is simply a separate window). Closing Revolution while one is open
  asks you to close the dialog first ("Close the file dialog first" in the status line).
- **Each kind of dialog starts where you last used it.** Importing media, opening other files
  (LUTs, presets), opening projects, exporting, and choosing folders each remember their own
  folder, so importing from a card doesn't send the next export there. (Every "choose a folder"
  dialog shares one memory: the card copy destination, an archive, frame exports.) **File ▸ Save As** of a
  project that's already saved starts in the project's folder. A remembered folder that's gone
  (an ejected card) is skipped, and the dialog starts in its usual place.

How you might use it:

- **Card, then project folder (beginner):** import from the card each time; the Import dialog
  opens on the card again while it's mounted, and export opens on your exports folder.
- **One folder per project (pro):** keep media, project and exports inside one project folder;
  after the first import and save every dialog opens there. Trade-off: switching projects means
  one click back to the new project's folder the first time.
- **Prefer the Media Browser for cards:** the **Media Browser** panel (the Media page in the
  Resolve workflow) stays open, shows camera folders as clips and has Favorites; the file dialog is quicker for a single file.

The remembered folders live in the preferences file (`dialogs.lastFolders`); deleting that
entry makes the dialogs start in their default place again. macOS and Windows dialogs remember
their folders themselves.

## Which GPU draws Revolution

> id: linux.gpu | does: chooses which graphics card draws the interface on machines with two | ui: settings.playback.graphicsDevice, settings.playback.batteryPreview | cmd: graphics.listDevices, app.settings.playback | resolve: GPU selection | premiere: GPU selection | when: laptops with two GPUs; a desktop with an iGPU | see: hardware.switches

**[Standard] [Revolution]**

Why: a laptop with integrated and discrete graphics, or a desktop with a CPU's graphics next to a
card, has two GPUs. The one that draws the interface and the monitors does the colour work and
the effects during playback, so it matters for smooth playback and for battery life.

1. Choose Edit ▸ Preferences ▸ **Playback…** (`app.settings.playback`).
2. Open **Graphics device** {ui:settings.playback.graphicsDevice} and pick **Auto**, **Power
   saving (integrated GPU)**, **Performance (discrete GPU)** or one GPU. Each GPU is listed with
   its name and whether it's discrete or integrated.
3. Press **OK**, then quit and start Revolution again: the GPU is chosen when the window opens.

The choices:

- **Auto (default):** the high-performance GPU (the discrete one), except on a battery-powered
  computer with Intel and NVIDIA graphics, where it is the Intel GPU: it decodes the camera's
  H.265 and draws it in place, which measured several times faster there. It also follows the
  system-wide switches: `DRI_PRIME=1` (start Revolution on the discrete GPU from a launcher's
  "Run on discrete graphics" option), `MESA_VK_DEVICE_SELECT`, or `WGPU_POWER_PREF=low`: with
  one of those set, the GPU the driver puts first is used. `__NV_PRIME_RENDER_OFFLOAD=1`
  (NVIDIA's version, what `prime-run` sets) starts it on the NVIDIA GPU. Good for most people.
- **Power saving (integrated GPU)** / **Performance (discrete GPU)** **[Fork]:** a kind of GPU
  rather than a named one, so the choice still means the same thing on another computer, or
  when the laptop's GPUs are listed in a different order after a driver upgrade. A computer
  without that kind uses the other one.
- **The discrete GPU:** the fastest playback with effects and grades. Trade-off: more power and
  heat; on a laptop the battery lasts less.
- **The integrated GPU:** quieter and longer on battery, fine for cutting; heavy grades or 4K+
  effects may drop frames. On a desktop it leaves the card free for a game or another program.

Revolution remembers the GPU by its PCI ids, not its position, so a driver update doesn't change
it. If that GPU isn't there any more (a removed card, an eGPU unplugged, preferences copied from
another computer) Revolution starts on Auto and Settings shows the stored one as "not found".
Hardware video decoding (VA-API) follows the choice when that GPU has a video engine, so decoded
frames are drawn in place without a copy (`FILMCRAFT_VAAPI_DEVICE` still overrides it; see
[Hardware](hardware.md)). The list fills in a moment after Revolution starts; if Settings shows
only Auto and the two kinds, close and reopen it. **[Fork]** Exports render on the same GPU by
the same rule, so a laptop never draws on one GPU and exports on the other by accident; the
Export dialog's two-GPU option still splits an export across both
([Export: Two GPUs](export.md)); with NVIDIA under Performance that is NVENC plus Intel, files `hev1`. On a laptop with an Intel and an NVIDIA GPU, hardware video
decoding uses the Intel video engine first (it decodes the 4:2:2 clips NVIDIA's can't) and
NVIDIA's decoder (NVDEC) takes only the clips Intel's declines (`FILMCRAFT_NVDEC_FIRST=1` puts
NVDEC first instead). When the window draws on the NVIDIA GPU, each 4:2:2 picture still goes
from the Intel engine straight to it (a shared buffer, no copy through main memory). NVIDIA's driver is only loaded when a clip first needs NVDEC, so with
**Power saving** (or Auto on such a laptop) a sleeping NVIDIA GPU stays asleep and saves battery, from the start: Revolution starts with Intel's Vulkan driver only, so switching to Performance takes effect at the next start; the
render node numbers (`renderD128`, `renderD129`) don't matter, Revolution goes by the maker.
Agents:
`graphics.listDevices` lists the GPUs, the stored choice and the one drawing now.

**[Fork] On battery.** Preferences ▸ Playback ▸ **Lower playback resolution on battery**
{ui:settings.playback.batteryPreview} (off by default): while the computer runs on battery,
playback renders one Playback Resolution step lower (Full becomes 1/2); when stopped, and in
every export, full resolution is kept. Plugged in again, it returns to your setting within about
ten seconds (agents: `perf.stats` `playback.effectiveResolution`). Trade-off: less detail while
playing, for less heat and a longer battery. Also
automatic: decoded pictures kept on an integrated GPU are sized from the RAM it shares (an
eighth, at most 4 GB; Preferences ▸ Memory overrides it), and Revolution asks the desktop not to
suspend while an export runs (the lid setting may still suspend it).

## Sound when the audio device has a hiccup

> id: linux.audio-device | does: playback survives an audio device that drops or changes, and reports it | cmd: app.settings.audioHardware, playback.audio | when: Bluetooth headphones, HDMI audio, device changes | see: linux.media-keys

**[Revolution]**

Why: on Linux, sound goes through PipeWire or PulseAudio (or straight to ALSA). A device can drop
out for a moment: a buffer runs dry under load, a USB or Bluetooth device reconnects, the audio
server restarts. Revolution keeps playing through it.

- **A short glitch:** the stream is restarted within about a third of a second and the sound
  carries on in step with the picture (the clock isn't reset, so audio and video stay in sync).
- **The device is gone** (unplugged, its server stopped): Revolution restarts the stream at once on
  the device chosen in Edit ▸ Preferences ▸ **Audio Hardware** (`app.settings.audioHardware`), or
  the desktop's default output when that one isn't there, at the same sample rate and channel
  count as before. When the audio server is still restarting, it tries again every 0.2 s for about
  half a second; the picture holds still meanwhile, then sound and picture carry on together.
- **It keeps failing** (more than three restarts in ten seconds): playback goes on without sound
  and the status line says "Audio output failed: playing without sound (check Settings ▸ Audio
  Hardware)". Press Stop and Play once
  the device is back, or pick another output in Audio Hardware.
- **Switching the default output in the desktop** (Plasma's audio widget, GNOME's Sound
  settings) while Revolution plays: with Audio Hardware's output on the system default, PipeWire
  normally moves the stream along; if it doesn't, Stop and Play picks up the new default.

When the output opens at another sample rate than it had a moment before (the desktop's default
output changed just as you pressed Play), Revolution mixes again at the new rate so the pitch stays
right; if it changes yet again, it plays without sound and the status line says so.

When the sound runs dry anyway (a slow disk at a cut, a busy computer), Revolution plays a moment
of silence, keeps the sound in step with the picture, and from then on mixes a little further
ahead (up to half a second) for the rest of that play. The trade-off: a change made while playing
(a volume fader) is heard that much later. If mixing itself fails, the status line says "Audio
mixing stopped after an error: playing without sound"; if the output can't start at all, it says
"No sound: the audio output could not start" with the reason.

Agents: `perf.stats` reports `playback.audio` (callbacks, underruns, missing frames, `leadMs`,
`mixerPanics`, `mixerStopped`).

## Dragging files in from the file manager

> id: linux.drag-drop | does: dropping files from the file manager, and what works instead on Wayland | cmd: file.import | when: importing by drag and drop | see: linux.paste

**[Revolution]**

Dropping files from Dolphin or Nautilus onto Revolution works on X11. **On Wayland it doesn't
yet:** the windowing library Revolution uses (winit 0.30) has no Wayland drag-and-drop; it
arrives with a later winit version. Until then, on Wayland:

1. **Copy and paste:** select the files in the file manager, Ctrl+C, then click the Project
   panel (or the Timeline) in Revolution and Ctrl+V (`edit.paste`).
2. **Open With:** right-click files in the file manager ▸ **Open With ▸ Revolution**; they are
   imported into the running Revolution (see above).
3. **Import:** **File ▸ Import** (`file.import`) opens the desktop's own file dialog.

## Pasting files, typing with an input method, and the touchpad

> id: linux.paste | does: pasting files and images, input methods for other scripts, touchpad gestures | cmd: edit.paste | when: importing by paste; typing Japanese or Chinese | see: linux.drag-drop

**[Standard] [Revolution]**

- **Pasting files and images.** On KDE Plasma (and wlroots desktops such as Sway) Revolution reads
  the Wayland clipboard directly, so Ctrl+V of files copied in Dolphin or an image copied in a
  browser works even without XWayland. GNOME doesn't offer that to apps; there Revolution reads the
  clipboard through XWayland as before, which works the same as long as XWayland runs (it does
  by default).
- **Input methods (fcitx5, IBus).** Japanese, Chinese, Korean and other input methods work in
  every text field: the candidate text shows while you compose, and only the text you choose is
  entered. Make sure the input method is enabled for Wayland apps (Plasma: System Settings ▸
  Keyboard ▸ Virtual Keyboard ▸ Fcitx 5).
- **Touchpad.** Two-finger scrolling moves the tracks under the pointer, a sideways swipe scrolls
  the Timeline in time.
- **Revolution:** no pinch-to-zoom on Wayland yet (the same windowing library limit as
  drag-and-drop above): zoom with **Alt + two-finger scroll** (or Alt + wheel) over the
  Timeline, the zoom keys, or the zoom slider. On X11 pinch works.

Options for zooming the Timeline, by what suits you:

- **Alt + scroll** keeps your hand on the touchpad and zooms around the pointer: good for
  finding a spot quickly.
- **Zoom keys** (**View ▸ Zoom In / Zoom Out**: `=` / `-` with the Premiere shortcuts, Ctrl+= /
  Ctrl+- with the Resolve ones) zoom in even steps: precise and repeatable, the habit most
  editors build for keyboard-driven cutting.
- **Zoom to Sequence** (`\` with the Premiere shortcuts, Shift+Z with the Resolve ones) is the
  quickest way back to the overview.

## Project files in the file manager

> id: linux.project-files | does: projects get their own icon, type, thumbnail and open with a double-click | when: finding projects in the file manager | see: linux.single-instance

**[Standard] [Revolution]**

Why: a project should behave like any other document: its own icon and type, double-click to
open, a picture of what's in it, and a place in the desktop's recent files.

- **File type.** Revolution installs the type `application/x-filmcraft-project` for `.fcproj`
  files (any capitalisation), so file managers show them as Revolution projects and
  double-clicking opens Revolution (in the running window, see "One Revolution at a time").
- **Open With.** Revolution is offered for the video files it imports (MP4, MOV, MKV, WebM,
  MXF, MPEG-TS, AVI, MPEG, 3GP); audio and picture files import too, through **File ▸ Import**
  or paste, but Revolution doesn't put itself in their Open With menus.
- **Unsaved changes are safe.** If the running Revolution has unsaved changes, a project opened
  from the file manager isn't swapped in: Revolution says so in the status bar; save or close,
  then open it again.
- **Menu actions.** Right-click Revolution in the application launcher or the taskbar: **New
  Project** starts an empty project (in the running window if there is one), **New Window**
  starts a second Revolution (`--new-instance`).
- **Recent files.** Opening or saving a project adds it to the desktop's recent-files list
  (`~/.local/share/recently-used.xbel`): Dolphin's **Recent Files**, the Plasma launcher's recent
  documents and GNOME's **Recent** show it.
- **Thumbnails.** A project's thumbnail is a frame of its active sequence, a tenth of the way in
  (at most 10 seconds), drawn by Revolution itself:
  `filmcraft-cli thumbnail <file> <size> <out.png>` (also works on video and picture files).
  GNOME Files, Nemo and Thunar use it through the installed `.thumbnailer` file. Dolphin needs a
  KDE plugin instead: in this fork it is the separate `filmcraft-kde-thumbnailer` package (see
  `fork/kde-thumbnailer/README.md`); then tick **Revolution projects** in Dolphin ▸ Configure
  Dolphin ▸ Interface ▸ Previews. Dolphin already shows media thumbnails through its own
  `ffmpegthumbs`.

## Media keys and the desktop's media controls

> id: linux.media-keys | does: the play / pause key and the desktop's media controls drive playback | cmd: playback.desktopMediaControls, playback.toggle | when: controlling playback from the keyboard or the lock screen | see: linux.global-shortcuts

**[Standard]**

Why: the play / pause key on your keyboard, Plasma's media widget, the lock screen and GNOME's
media controls should drive the Program monitor like they drive a music player.

While a sequence is open, Revolution shows up as a media player (MPRIS,
`org.mpris.MediaPlayer2.filmcraft`) named after the sequence and project:

| Desktop control | What Revolution does |
|---|---|
| Play / Pause | `playback.toggle` on the Program monitor |
| Stop | `playback.stop` |
| Next / Previous | one frame forward / back (`playhead.stepForward` / `playhead.stepBack`) |
| Seek bar | `playhead.set` |

1. Open a sequence and press the keyboard's play / pause key (Revolution doesn't need to be in
   front).
2. Or open Plasma's media widget in the system tray: the sequence name is the title, the project
   the album.

With no sequence open there is no player.

To leave Revolution out of the media controls, turn off **Settings ▸ Playback ▸ Media keys and the
desktop's media controls** (`playback.desktopMediaControls`). It takes effect at once. Options:

- **On (default):** the play / pause key works while you look at a script or notes in another
  window, and the lock screen shows what is open. Handy when you review a cut from the sofa.
- **Off:** the media keys go to your music or video player instead, even while Revolution is open.
  Pick this if you listen to music while you edit and want the keys to keep controlling it.

## Global shortcuts: play and mark from another window

> id: linux.global-shortcuts | does: play, stop and add markers while another window has the focus | cmd: playback.desktopGlobalShortcuts, markers.add | when: logging notes while watching a cut | see: linux.media-keys

**[Standard]**

Why: when you log notes while watching a cut, or follow a client's comments in a browser, you
want to stop playback or drop a marker without clicking back into Revolution first.

Turn on **Settings ▸ Playback ▸ Global shortcuts** (`playback.desktopGlobalShortcuts`, off by
default). Revolution asks the desktop for two keys that work whichever window is in front:

| Shortcut | Suggested keys | What Revolution does |
|---|---|---|
| Play/Stop | Ctrl+Alt+Shift+Space | `playback.toggle` on the Program monitor |
| Add Marker | Ctrl+Alt+Shift+M | `markers.add` at the Program playhead |

1. Turn the setting on. Plasma (and GNOME 48 or later) shows a dialog listing both shortcuts with
   the suggested keys. Change them there if you like, then confirm.
2. Change them later in Plasma's **System Settings ▸ Keyboard ▸ Shortcuts** (Revolution is listed
   under Applications), or GNOME's **Settings ▸ Apps ▸ Revolution**.
3. Turn the setting off to free the keys at once.

Options, and when to pick which:

- **Global shortcuts** suit a two-window routine: notes or a script on one screen, Revolution on
  the other. They need the desktop's GlobalShortcuts portal (Plasma 6, GNOME 48+, some others);
  without it the switch does nothing.
- **The media keys** (above) already give you play / pause from anywhere with no setup, but no
  marker, and they may be claimed by your music player.
- **Revolution's own shortcuts** (Space, M) are the beginner's choice: nothing to configure, and no
  risk of a global key firing in the wrong app. They only work while Revolution has focus.

## Sending a file to your phone (KDE Connect)

> id: linux.kdeconnect-send | does: sends an export to a phone with KDE Connect | cmd: kdeconnect.send, kdeconnect.devices | when: watching a cut on a phone | see: linux.kdeconnect-remote

**[Standard]**

Why: to watch a cut on the phone a client will see it on, or hand a rough export to someone in
the room, without a cable, a cloud upload or a chat app.

With [KDE Connect](https://kdeconnect.kde.org/) running and a phone (or tablet, or another
computer) paired and in reach, right-click a clip or an export in the **Project** panel and choose
**Send via KDE Connect ▸ <device>**. KDE Connect shows the transfer in its own notification; the
file lands in the phone's Downloads folder. From scripts or an agent: `kdeconnect.devices` lists
the reachable devices, `kdeconnect.send` takes `path` (or a project `item`) and `device` (id or
name; may be left out when only one device is in reach).

The entry only appears while a paired device is reachable. Revolution never starts KDE Connect
itself and never talks to the phone directly: it asks the KDE Connect already running on your
desktop. The Flatpak build is allowed to talk to KDE Connect (`org.kde.kdeconnect`) out of the
box; nothing to set.

- **Revolution:** no GSConnect support yet: on GNOME's GSConnect the entry doesn't appear.

Options, and when to pick which:

- **KDE Connect** is the quickest for a file or two on the same Wi-Fi: no account, nothing leaves
  your network. Large camera originals are slow over Wi-Fi; send an export or a proxy instead.
- **A USB cable or a card reader** is faster for big files and works with no network at all.
- **A cloud folder or a review link** suits someone who isn't in the room, or who wants to leave
  comments on the cut.

### Files shared from your phone

**[Standard]**

Why: to get a phone clip (a behind-the-scenes shot, a voice memo, a reference photo) into the
project without a cable or a cloud folder.

Share a photo, video or audio file from the phone to this computer with KDE Connect. When it has
arrived, Revolution acts as **Settings ▸ Media ▸ Files shared from a phone (KDE Connect)** says:

- **Offer to import** (the default): a bar at the top of the Project panel says "N new file(s)
  from <device>" with **Import** (into a top-level bin called **Inbox**, made when needed) and
  **Dismiss**. Good when the phone also sends things that aren't for this project.
- **Import into the Inbox bin**: no question asked. Quickest while you're shooting B-roll on the
  phone for the project that's open; sort the Inbox bin later.
- **Ignore**: Revolution leaves shared files alone; they stay in the phone's KDE Connect download
  folder (usually Downloads) for you to import by hand.

Only files Revolution can import are offered; documents and links are left to KDE Connect. The
file is used where KDE Connect saved it: move it to your media drive first if Downloads is not
where you keep footage. From scripts or an agent: `kdeconnect.received` (`path`, `device`),
`kdeconnect.importReceived`, `kdeconnect.dismissReceived`; the waiting files are in the UI state
as `phone_shares`.

## Your phone as a playback remote (KDE Connect)

> id: linux.kdeconnect-remote | does: a phone starts, stops and scrubs playback through KDE Connect | when: showing a cut on a TV from the sofa | see: linux.kdeconnect-send

**[Standard]**

Why: to start, stop and scrub a cut from the sofa while a client or the family watches it on the
big screen, without a keyboard in your lap.

Nothing to set up in Revolution: KDE Connect's **Multimedia control** plugin drives any desktop
media player, and Revolution is one while a sequence is open (see *Media keys and the desktop's
media controls* above).

1. In the KDE Connect app on the phone, open the paired computer and tap **Multimedia control**.
2. Pick **Revolution** in the player list (it is named after the sequence; the project shows as the
   album).
3. Play / pause, the skip buttons (one frame forward / back) and the seek bar all act on the
   Program monitor's playhead.

The phone's volume slider does nothing here: Revolution's own mixer and your system volume decide
what you hear. If Revolution isn't in the list, check that a sequence is open and that **Settings ▸
Playback ▸ Media keys and the desktop's media controls** is on.

Options, and when to pick which:

- **Phone remote:** best for watching a cut away from the desk; it needs the phone paired and on
  the same network. The skip buttons step single frames, so use the seek bar for big jumps.
- **A keyboard's media keys** or **Global shortcuts** (above): no phone needed, but you sit at the
  keyboard.
- **Revolution's own keys** (Space, J / K / L): the most precise (shuttle speeds, marks), at the desk only.

## Logs in the system journal, and reporting a crash

> id: linux.logs | does: logs go to the system journal; Help ▸ Report an Issue gathers what a developer needs | cmd: help.reportIssue | when: something went wrong | see: linux.files

**[Standard] [Revolution]**

Why: when something goes wrong, the log is what tells a developer what happened. Revolution
keeps its own log file and, started from the desktop, writes to the system journal too, with
the right levels, so the usual tools find it.

- **Log file:** `~/.local/state/filmcraft/Logs/filmcraft.log` (the two previous runs are kept as
  `filmcraft.1.log` / `filmcraft.2.log`); **Help ▸ Reveal Log Files** opens the folder.
- **Journal:** started from Plasma or GNOME's launcher, records go to the journal with their
  level. `journalctl --user -t filmcraft` shows them; add `-p warning` for warnings and errors
  only. Started from a terminal they appear there instead; in Flatpak only the log file is
  written.
- **Crash logs:** `crash-<day>.log` in the same folder, with what was happening and where.

If Revolution catches an error, the **Revolution hit an error** window offers **Save Project**,
**Report an Issue…** and **Continue**:

1. Click **Save Project** first, to be safe.
2. Click **Report an Issue…**: the file manager opens with the crash log selected and the
   browser opens the issue tracker (`help.reportIssue`). Attach the crash log to the report.
   Nothing is sent anywhere by Revolution itself.
3. Click **Continue** to keep working.
