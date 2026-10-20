# Newer builds: the daily check and one-click install

**[Fork]** · how this Revolution finds out that a newer build is out, what that check sends, and
how one click installs the new build without touching the one you are running. Neither Resolve
nor Premiere works quite like this; the differences are noted in each section.

## Why Revolution checks for updates, and what the check sends

> id: updates.why | does: explains the daily look for a newer Revolution build and exactly what it sends | ui: settings.update.check | cmd: app.update.check, app.update.status, app.update.setEnabled | resolve: Check for updates (Help menu) | premiere: Creative Cloud updates | when: you wonder whether Revolution talks to the internet, or want it not to | see: updates.install, updates.settings

**[Fork]**

New builds fix problems and add tools, but you only benefit if you know they exist. So every day,
in the background, Revolution asks GitHub for the newest release of this build's line and
compares it with the build you are running. When it finds a newer one, a small **Update
available** window opens in the bottom-right corner; otherwise you never notice the check.

What the check sends: **one request to GitHub (api.github.com)** asking for the newest release.
GitHub sees your IP address, as with any web page; nothing else is sent: no usage data, no
names, no identifiers. The first **Update available** window says this too.

The check never gets in your way: it runs in the background, so a slow or missing connection
never stalls Revolution; if GitHub doesn't answer (offline, rate-limited, a broken reply) it
simply tries again the next day, and nothing is shown.

**Where it differs:** Resolve checks when you open it or from Help ▸ Check for Updates; Premiere
leaves updates to the Creative Cloud app. Revolution's check is part of the app, daily, and
can be turned off ({ui:settings.update.check}).

Builds made by someone else (a distribution package, Flatpak) don't check, or point you at
their own updater: the people who packaged them update them.

## Update, Later, or Don't check

> id: updates.install | does: installs a newer Revolution build with one click, keeping the running one for going back | ui: update.install, update.later, update.optOut, update.restart, update.releasePage | cmd: app.update.install, app.update.status | resolve: — | premiere: — | when: the Update available window is open | see: updates.why, updates.rollback

**[Fork]**

The **Update available: X → Y** window shows the new build's release notes, so you can decide
whether now is a good time. You have three answers:

- **Update** ({ui:update.install}): downloads the new build, checks it, and installs it beside
  the one you are running. You can keep working meanwhile; a bar shows the progress. When it
  says **Installed**, choose **Restart Now** ({ui:update.restart}) or keep working and restart
  later: the next time you start Revolution from its launcher, the new build opens.
- **Later** ({ui:update.later}): closes the window. The next day's check asks again only if an
  even newer build appears.
- **Don't check for updates** ({ui:update.optOut}): turns the daily check off. You can turn it
  back on, or check by hand, in **Settings ▸ Updates**.

Why it is safe to click **Update**:

1. The download is checked against the release's published checksums (`SHA256SUMS`): a damaged
   or tampered file is refused.
2. The new build is unpacked into its own folder next to the current one. Only when everything
   is in place does Revolution switch the launcher's `latest` link to it, in one step.
3. If anything fails or the download is interrupted, nothing is different: the build you are
   running stays as it was, and half-finished downloads are removed (at the latest by the next
   update).
4. The previous build is kept, so you can go back ([Roll back](#roll-back-to-the-previous-build)).

**Restart Now** closes Revolution as usual (it asks about unsaved work first) and then starts
the new build.

**An AppImage does this too** (from 0.1.3): **Update** downloads the release's AppImage for your
processor, checks it and puts it where the old file was; the old one is kept beside it as
`<name>.AppImage.previous`, which **Roll back** moves back.

Other installs offer **Open the Release Page** ({ui:update.releasePage}) instead and say what to
do: the `.deb` / `.rpm` or Arch package through your package manager, a newer `.flatpak`, a newer
Windows `.msi` (or portable `.zip`), or a fresh build of a source folder ([install](install.md)).

## Settings ▸ Updates: turn the check on or off, check now

> id: updates.settings | does: turns the daily look for newer builds on or off and looks right away | ui: settings.update.check, settings.update.checkNow | cmd: app.settings.update, app.update.check, app.update.setEnabled | resolve: Preferences ▸ System ▸ General ▸ Check for updates | premiere: — | when: you want to check right now, or stop the daily check | see: updates.why

**[Fork]**

1. Open Edit ▸ Preferences ▸ **Updates** (`app.settings.update`).
2. **Check for updates every day** ({ui:settings.update.check}) is on by default. Untick it to
   stop the automatic check (`app.update.setEnabled` with `"enabled": false` does this too).
3. **Check Now** ({ui:settings.update.checkNow}, `app.update.check`) asks GitHub right away,
   even when the daily check is off. The line above it says what the last check found: up to
   date, an update, or why it didn't get an answer.

## Roll back to the previous build

> id: updates.rollback | does: switches the launcher back to the build a newer one replaced | ui: settings.update.rollback | cmd: app.update.rollback, app.update.* | resolve: — | premiere: — | when: an update brought a problem and you want the build you had before | see: updates.install

**[Fork]**

After an update, Settings ▸ Updates shows **Roll Back to …** ({ui:settings.update.rollback},
`app.update.rollback`) with the name of the build the update replaced. It points the launcher back
at that build; the next start opens it. The newer build stays in its folder, so the next check
offers it again (choose **Later** or turn the check off while you wait for a fix).

Agents and scripts use `app.update.*` (`check`, `status`, `install`, `rollback`, `setEnabled`; `command.list`
shows their parameters).
