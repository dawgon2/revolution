# Installing Revolution

Status: **written** (PKG: every release format, from Revolution 0.1.3 on).

Every Revolution release on GitHub (`github.com/dawgon2/revolution`, Releases) carries one
program in several packages, for Linux and Windows on both kinds of processor: **x86_64** (Intel /
AMD, most PCs; Windows calls it **x64**) and **aarch64** (ARM: Snapdragon laptops, Ampere, some
mini PCs; Windows calls it **arm64**). Which one to pick depends on how you want it installed and
updated, not on the editor: inside, each one is identical. The file `SHA256SUMS` lists a checksum
for every file, so you can check a download (`sha256sum -c SHA256SUMS --ignore-missing`).

Not sure which processor you have? On Linux run `uname -m` in a terminal; on Windows open
Settings ▸ System ▸ About and read "System type".

## Which package to choose

> id: install.choose | does: compares the release's packages so you can pick one for your computer | resolve: one installer per system | premiere: Creative Cloud app | when: "which file do I download?" | see: install.portable, install.windows-msi, linux.install

**[Fork]**

Why: they differ in where the program lives, what it can reach and how it gets updated. There is
no single right choice; a beginner is usually happiest with the first option for their system,
and you can switch later (your projects and settings stay where they are).

| You use | Easiest | Also possible |
|---|---|---|
| Debian, Ubuntu, Mint, Pop!_OS | `.deb` | Flatpak, AppImage, portable folder |
| Fedora, openSUSE, RHEL | `.rpm` | Flatpak, AppImage, portable folder |
| Arch, CachyOS, Manjaro | the PKGBUILD (built from source) | portable folder (updates itself), AppImage |
| Any other Linux | AppImage or Flatpak | portable folder |
| Windows 10 / 11 | `.msi` installer | portable `.zip` |

- **Newer versions from inside the app.** The portable Linux folder and the AppImage replace
  themselves with one click when a new release is out ([updates](updates.md)). The others tell
  you about it, and you install the new one just as you did the first time (your package
  manager, the software centre, or the newer installer).
- **What it can reach.** Packages, the AppImage and the portable folder see every drive you can;
  the Flatpak only your home folder and external drives ([linux](linux.md#installing-a-distribution-package-flatpak-or-the-tarball)).

## The portable Linux folder (tarball)

> id: install.portable | does: runs Revolution from a folder you unpack, with no install; it can replace itself with newer releases | resolve: — | premiere: — | when: trying a release; keeping versions side by side; Arch without building | see: updates.install, install.choose

**[Fork]**

The file is `revolution-<version>-linux-x86_64.tar.gz` (or `-aarch64`).

1. Make a folder for Revolution and unpack it there (no administrator rights):
   `mkdir -p ~/Apps/revolution && tar -xzf revolution-<version>-linux-x86_64.tar.gz -C ~/Apps/revolution`.
2. Point a `latest` link at it: `ln -sfn revolution-<version> ~/Apps/revolution/latest`.
3. Start `~/Apps/revolution/latest/revolution` (make that your launcher's command).
   `NOTES.md` beside it says what is new.
4. To remove it, delete the folder. Your projects, settings and caches stay in your data folder
   ([linux](linux.md#where-revolution-keeps-its-files)).

With the `latest` link, Revolution can install a newer release by itself: it unpacks the next
version into its own folder beside this one, checks it against `SHA256SUMS`, then moves the link;
the old folder stays, so you can go back ([updates](updates.md)). Without the link it still runs,
but only tells you about new releases.

## AppImage

> id: install.appimage | does: one file that runs on almost any Linux; mark it executable and start it | resolve: — | premiere: — | when: a distribution with no package; a USB stick | see: install.choose

**[Standard]** (the AppImage format)

1. Download `revolution-<version>-linux-x86_64.AppImage` (or `-aarch64`).
2. Make it executable: in the file manager, Properties ▸ Permissions ▸ "Allow executing", or
   `chmod +x revolution-*.AppImage`.
3. Double-click it, or run `./revolution-<version>-linux-x86_64.AppImage`. Nothing is installed;
   it needs no FUSE library.

The `.AppImage.zsync` next to it lets tools such as AppImageUpdate fetch only the parts that
differ in a newer release. Your desktop menu doesn't list the AppImage unless you add it (or use
a helper such as AppImageLauncher, Gear Lever).

## Debian and Ubuntu (.deb)

> id: install.deb | does: installs Revolution like any Debian / Ubuntu program, with its menu entry and file types | resolve: — (Linux installer) | premiere: — | when: Debian 12+, Ubuntu 22.04+, Mint, Pop!_OS | see: install.choose, linux.install

**[Standard]**

1. Download `revolution-<version>-linux-x86_64.deb` (`-aarch64` for ARM).
2. Install it with your software centre (double-click the file), or in a terminal:
   `sudo apt install ./revolution-<version>-linux-x86_64.deb` (apt fetches what it depends on).
3. Start **Revolution** from the application menu, or run `revolution`. The command-line tool is
   `revolution-cli`.

To update, install the newer `.deb` as above. To remove: `sudo apt remove revolution`.

## Fedora and openSUSE (.rpm)

> id: install.rpm | does: installs Revolution on Fedora, openSUSE or RHEL-family systems, with its menu entry | resolve: — (Linux installer) | premiere: — | when: Fedora 36+, openSUSE Tumbleweed, RHEL 10 | see: install.choose, linux.install

**[Standard]**

1. Download `revolution-<version>-linux-x86_64.rpm` (`-aarch64` for ARM).
2. Install it: double-click it (GNOME Software / Discover), or
   `sudo dnf install ./revolution-<version>-linux-x86_64.rpm`
   (openSUSE: `sudo zypper install ./revolution-<version>-linux-x86_64.rpm`).
3. Start **Revolution** from the application menu, or run `revolution`.

It needs glibc 2.35 or newer, so RHEL 9 refuses it instead of installing a program that can't
start. To remove: `sudo dnf remove revolution`.

## Flatpak

> id: install.flatpak | does: installs Revolution in a sandbox on any Linux, with its own runtime | resolve: — | premiere: — | when: an immutable distribution (Silverblue, SteamOS, Bazzite); keeping it isolated | see: linux.install, install.choose

**[Standard]**

1. Download `revolution-<version>-linux-x86_64.flatpak` (or `-aarch64`).
2. Install it for your user: `flatpak install --user revolution-<version>-linux-x86_64.flatpak`
   (or open it with your software centre). The first time it also downloads the freedesktop
   runtime from Flathub (a few hundred MB, shared with other Flatpaks).
3. Start **Revolution** from the menu, or `flatpak run io.github.dawgon2.Revolution`.

What the sandbox allows is listed in [linux](linux.md#installing-a-distribution-package-flatpak-or-the-tarball).
To remove: `flatpak uninstall io.github.dawgon2.Revolution`. Revolution isn't on Flathub yet, so
`flatpak update` doesn't update it: install the next release's file as above.

## Arch and CachyOS (PKGBUILD)

> id: install.arch | does: builds and installs Revolution as an Arch package from the source | resolve: — | premiere: — | when: Arch, CachyOS, EndeavourOS, Manjaro | see: install.choose, linux.install

**[Fork]**

1. Get the source of a release (the "Source code" archive of the release, or `git clone`).
2. In `packaging/linux/arch`, run `makepkg -si`. It builds with Rust (about 10–30 minutes the
   first time) and installs `revolution` (and `revolution-kde-thumbnailer` for project pictures in
   Dolphin).
3. Start **Revolution** from the menu, or run `revolution`.

An AUR package is planned, so `yay` / `paru` can install and update it for you. Until then,
rebuild from the newer source to update, or use the portable folder, which updates itself.

## Windows installer (.msi)

> id: install.windows-msi | does: installs Revolution on Windows 10 / 11 with a Start menu entry; removed in Settings ▸ Apps | resolve: installer from Blackmagic's site | premiere: Creative Cloud app | when: "how do I install Revolution on Windows?" | see: install.windows-zip, install.choose

**[Fork]**

1. Download `revolution-<version>-windows-x64.msi` (`-arm64` on an ARM laptop).
2. Double-click it. Windows SmartScreen may warn "Windows protected your PC", because the
   installer isn't signed yet: choose **More info ▸ Run anyway** if you trust the download (check
   it against `SHA256SUMS` first: `certutil -hashfile <file> SHA256`).
3. It installs for all users into `Program Files\Revolution` and adds **Revolution** to the Start
   menu. Settings and projects live in `%APPDATA%\Revolution`.

To update, run the newer `.msi`: it replaces the old version. To remove it, Settings ▸ Apps ▸
Installed apps ▸ Revolution ▸ Uninstall (your projects stay).

Where this differs on Windows: the Linux desktop integration ([linux](linux.md)) isn't there,
and heavy camera files play best with [proxies](proxies.md).

## Windows portable (.zip)

> id: install.windows-zip | does: runs Revolution on Windows from a folder, settings kept beside it, nothing installed | resolve: — | premiere: — | when: no administrator rights; a USB drive | see: install.windows-msi

**[Fork]**

1. Download `revolution-<version>-windows-x64-portable.zip` (or `-arm64`) and extract it.
2. Run `revolution.exe`. Because `portable.txt` is in the folder, preferences, presets, logs and
   crash recovery are kept in a `data` folder next to it, so the whole thing moves with the
   folder. Delete `portable.txt` to use `%APPDATA%\Revolution` instead.
3. To remove it, delete the folder.
