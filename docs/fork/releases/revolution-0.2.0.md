# Revolution 0.2.0

Four big things since 0.1.3: node trees in the colour grade, a Resolve-style Cut page and Edit page
tools, sending a whole project to another computer (through your server or straight across the
room), and "how do I get this look?" answers from a reference picture.

## Node trees in the grade

The Color page's node strip is now a **node canvas**. Serial grades look and work as before; right-click
a node to add more kinds:

- **Parallel:** two corrections work on the same picture and are blended (skin in one, background in
  the other).
- **Layer:** a look on top of the picture, with a blend mode and opacity.
- **Outside:** corrects everything outside another node's selection.
- **Splitter / Combiner:** work on the red, green and blue channels separately.
- **Key mixer:** combine selections (add, subtract, intersect).

Select a mixer node to get its controls: a weight per input for parallel nodes, blend mode and
opacity per input for layer nodes, mode and invert per key input for key mixers. Node positions you
drag are kept with the project (**Reset Layout** in the canvas menu tidies them). Resolve shortcuts in
the Resolve keymap: Alt+S serial, Alt+P parallel, Alt+L layer, Alt+O outside. The explainer and the
Grading Coach describe branches in plain words, with five new lessons. Older projects open unchanged.

## Resolve-style Edit page tools and a Cut page (Resolve workflow)

- **Edit commands as Resolve names them:** Insert, Overwrite, Replace, Place on Top, Append at End,
  Ripple Overwrite, Fit to Fill; on the Cut page also Smart Insert, Close Up and Source Overwrite.
- A **horizontal timeline toolbar** and Resolve-style **track headers**, a viewer header and
  transport, a **Bins** column next to the Media Pool and a Resolve-style **Inspector**.
- A **Cut page** (Shift+3): the Media Pool and one viewer above, the whole timeline in an upper strip
  and a zoomed timeline below, for fast assembly.
- The Premiere workflow is unchanged.

## Send a project to another computer

In the **Project Manager**: **Send…** and **Receive…** in the bottom bar (and **Send…** on a project's
right-click menu).

- **Through your server** (WebDAV / Nextcloud, SFTP, S3): pick the server and a folder; choose
  **Include proxies** or **Leave out unused media**; the size is shown before sending; progress with
  time left; Cancel. Receive lists the packages on the server, newest first, and imports the one you
  pick, with every clip relinked.
- **Nearby computer, no server:** the receiving computer clicks **Receive from a nearby computer** and
  shows a code like `7-velvet-river-lamp` (valid 10 minutes, once). The sender chooses **Nearby
  computer**, types the code (or clicks **Find** first) and **Send**. The receiver sees who wants to
  send what and clicks **Accept** or **Decline**; nothing is written before Accept. The transfer is
  encrypted, a wrong code can't be guessed by watching the network (a wrong try gets a new code), and
  a dropped connection continues where it stopped if you send again in time.
- The sidebar's **Remote** group lists your servers, with **Manage Locations…** (the same list as
  Settings ▸ Remote Locations).
- If a firewall blocks the transfer, set a fixed port in Settings ▸ Remote Locations and allow it on
  the receiving computer (Windows asks; on Linux see the guide). The guide's Network storage page
  explains each step.

## "How do I get this look?"

- Import reference pictures into the Color page **Gallery** (PNG, JPEG, TIFF, WebP and more), compare
  with a wipe or split, and use them in **Shot Match**.
- Ask "how do I get this look?" while comparing: the answer shows the measured difference (exposure,
  contrast, blacks, shadow / highlight colour, saturation, skin, grain), numbered steps in the grade,
  what belongs on set, and a **Do this** button. Works without the AI models too. Reference pictures
  stay on your computer.

## Smaller things

- The portable download (tarball, also what the updater installs) now adds its own menu entry and
  icon, so everyone gets the new icon, not only the packages.
- Every release page starts with direct download links, the x86_64 Linux and Windows x64 files first.

## Install

As before: tarball, AppImage, .deb, .rpm and Flatpak for Linux (x86_64 and aarch64), .msi and portable
.zip for Windows (x64 and arm64), and `SHA256SUMS`. The built-in updater offers 0.2.0 to 0.1.x users.
