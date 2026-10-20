# Revolution 0.1.3

Two things: Revolution now comes in every common package format for Linux and Windows, and it can
keep projects and media on your own server (Nextcloud, any SSH server, MinIO / S3) without
mounting anything.

## Install it the way you like

Every release now has, for **x86_64 and ARM (aarch64 / arm64)**:

- **Linux:** a portable folder (`…-linux-<arch>.tar.gz`: unpack and run `revolution`), an
  **AppImage**, a **.deb** (Debian, Ubuntu, Mint), an **.rpm** (Fedora, openSUSE) and a **Flatpak**.
- **Windows:** an **.msi** installer and a portable **.zip**.
- One `SHA256SUMS` file to check the downloads.

Each package is installed and started on a clean system before the release is published. The
built-in updater knows which kind you installed: the portable folder and the AppImage update
themselves (the AppImage keeps the previous file so **Roll Back** works); for .deb, .rpm, Flatpak
and the Windows installer it tells you the one command or click that updates it. The install page
of the guide (`docs/guide/install.md`) explains which to pick.

The Flatpak may use the network (update check, model downloads, sharing a question as a GitHub
issue); the control port still listens on this computer only.

## Projects and media on your own server

**File ▸ Remote Locations…** and **Settings ▸ Remote Locations**:

- **Add a server** once: WebDAV (Nextcloud, ownCloud, Synology), SFTP (any Linux server) or
  S3-compatible (MinIO, Garage, Amazon S3). **Test** connects, lists the folder and checks that the
  server supports safe project locks. Passwords and keys go into the **system keyring** (KWallet,
  GNOME Keyring, KeePassXC), never into projects, settings or logs; a plain file is used only if
  you allow it.
- **Browse** the server like a folder and **Import** clips: they download into the media cache in
  the background (a stopped download continues where it stopped) and appear in the project.
- **Open a project from the server.** Revolution works on a local copy and puts a small lock file
  next to the project, so a second computer opening it sees who has it and opens it read-only (or
  takes it over after a warning). **File ▸ Save** saves to the server; the server's previous copy is
  kept (the newest 10). If someone else changed the file or took the project over meanwhile, your
  save becomes a **conflict copy** next to it instead of overwriting their work. Quitting gives the
  lock back.
- **Save Project Here** puts the open project on the server.
- **Send and receive whole projects:** a project with all its media as one package, uploaded to the
  server in the background; on the other computer **Receive Package** downloads it (continuing
  after a dropped connection), checks every file against the package's list and opens it with
  every clip relinked.
- Project packages are now **`.revpkg`** files; older `.fcpackage` files still open.

Everything is pure Rust (no system libraries needed). For SSH sign-in with a key, ed25519 keys are
recommended over RSA.

Not yet: playing a clip straight from the server without downloading it first.

Settings and projects from 0.1.x carry over.
