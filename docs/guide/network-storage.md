# Projects and media on a network share

**[Fork] [Revolution]** · keeping projects and clips on a home server (a NAS, another PC) that is
mounted as a folder: how Revolution tells you a file is on a share, and what it does to keep your
work safe there. This page grows with STORAGE-PLAN Phase S1; sections not written yet are marked.

## Why it matters where a file lives

> id: network-storage.why | does: explains why Revolution shows whether a project or clip is on a network share | resolve: shared storage / network media | premiere: projects on a NAS | when: project or media on SMB, NFS, sshfs or a Plasma network folder | see: network-storage.check

**[Fork]**

A share mounted by Plasma (Dolphin's Network places), by `/etc/fstab` or by autofs looks like an
ordinary folder, but it isn't one: reading from it is slower than a local disk, it can stop
answering when the server sleeps or the Wi-Fi drops, and saving over it isn't always as safe as on
a local disk. So Revolution first finds out **where** a file lives; later steps (a local copy of
clips, timeouts so the app never hangs, extra-safe saves) use that to take care only of the files
that need it.

Revolution recognises SMB / CIFS, NFS, sshfs, rclone, GNOME / Plasma network folders (`kio-fuse`,
`gvfs`) and a few others. It looks only at the system's mount table, never at the file itself, so
the answer comes at once even while the server is down.

## Check where your project and clips are

> id: network-storage.check | does: shows whether the project file and clips are on a network share | resolve: — | premiere: — | when: you want to know whether a project depends on a server | see: network-storage.why

**[Fork]** · neither Resolve nor Premiere shows this; it is Revolution's own.

1. Open **File ▸ Project Settings ▸ General** (`file.projectSettings.general`).
2. The first line says **Project file: on a local disk** or **Project file: on a network share
   (cifs, //nas/media)**, with the share's type and server (`projectSettings.storage.project`).
   An unsaved project shows no line.
3. If any clips are on shares, the next line says how many: **Clips on network shares: 3 of 12**
   (`projectSettings.storage.clips`).
4. In the **Project** panel (Media Pool in the Resolve workflow), a clip on a share has a grey
   **NET** badge next to its other badges, in list and icon view; hover it to see the share, e.g.
   **On a network share: //nas/media (cifs)** (`project.item.<id>….net`). Revolution re-reads the
   mount table every few seconds, so a share you mount or unmount shows up shortly after.

For scripts and agents: `media.storage` (optionally `{"item": <id>}`) answers for the project file
and every file clip: `{"project": {"kind": "network", "fsType": "cifs", "source": "//nas/media"},
"items": [{"item", "path", "storage"}], "networkItems": n}`; `kind` is `local`, `network` or
`unknown` (`unknown` on systems other than Linux for now).

## Local copies of clips on a share

> id: network-storage.copies | does: keeps copies of a project's network clips on this computer so they play smoothly and survive a dropped share | resolve: — (Resolve relies on proxies or optimized media) | premiere: — (Premiere relies on proxies) | when: editing big camera files that live on a NAS | see: network-storage.drops

**[Fork]**

Playing 6.2K camera files over Wi-Fi from a NAS stutters, and when the share drops the clips
go with it. **Settings ▸ Media Cache ▸ Network Copies ▸ Local copies of clips on network shares**
(`mediaCache.networkCopies`, **Automatic** by default) keeps a full copy of each such clip in the
media cache folder (`Network Copies`), and once a copy is complete Revolution reads the copy
instead of the share. The copy is byte-for-byte the same file, so the picture, the grade and
exports are exactly as from the original.

- A copy belongs to one version of the file: if the file on the server changes (re-rendered,
  replaced), the old copy is no longer used and a new one is made.
- If the share isn't answering, Revolution uses the last complete copy it has, so you can keep
  editing while the NAS is asleep. A file that has been deleted or moved on a share that *is*
  answering goes offline as usual: an old copy never hides that.
- **Local copies size limit** (`mediaCache.networkCopiesGb`, 100 GB): above it the copies used
  least recently are deleted, the open project's last. Revolution only ever deletes copies it
  made; the Media Cache management policy leaves this folder alone.
- Set it to **Off** to never read or make copies.

Different from Resolve and Premiere: both expect you to make proxies or optimized media for
slow storage; Revolution's copies are the original files, so nothing is re-encoded. Proxies still
work as before and are the better choice when the disk is small.

Copies are made in the background, one file at a time, only while you aren't playing (pressing
play pauses the copy; it carries on from where it stopped a few seconds after you stop). A file
that changes on the server while it is being copied isn't kept; it is copied again later. When a copy is complete a
message says **Local copy of A001.mov ready: it now plays from this computer**. A copy that
fails (the share dropped, the disk is full) says so and isn't tried again until Revolution
restarts; its half-made file is removed.

In the **Project** panel a clip whose copy is complete shows **NET ✓** instead of **NET**; hover it
for **…; plays from its local copy**.

Still being built: not copying when the disk holding the media cache is nearly full (for now the
size limit and a clean stop when the disk fills up protect it).

## Saving a project that lives on a share

> id: network-storage.saving | does: explains how Revolution keeps saves safe when the project file is on a network share | resolve: — | premiere: — | when: saving a project stored on a NAS or another PC | see: network-storage.check

**[Fork]** · Resolve keeps projects in its own database, so it has no direct equivalent; Premiere
saves a project on a share like any other file.

Every save in Revolution writes to a temporary file next to the project, flushes it to disk and
then swaps it in, so a crash mid-save never leaves half a file. Over SMB that swap isn't always
as safe as on a local disk, so when the project file is on a network share Revolution does more:

1. **A copy on this computer first.** Before writing to the share, File ▸ Save (`file.save`),
   Save As and Save a Copy write the same project to
   `<data folder>/network-backups/<name>-<number>.fcproj` (the number tells apart projects with
   the same name in different places). Only the latest save is kept there.
2. **Read back.** After the save, Revolution reads the file on the share back and compares it with
   what it saved.
3. **If something goes wrong** (the share stops answering, the disk is full, the file reads back
   different), the save reports an error that ends with **This save is kept on this computer:**
   and the path of the local copy. Open that file with File ▸ Open to carry on; save it back to
   the share once the share works again.
4. **Auto-saves stay local.** For a project on a share, auto-save versions go to the data
   folder's `Auto-Save` folder on this computer instead of an `Auto-Save` folder next to the
   project, unless you chose a folder in **Project Settings ▸ Scratch Disks ▸ Project Auto Save**.

The save's result (for scripts) includes `localCopy`: the path of the local copy, or `null` for a
project on a local disk.

## When the server stops answering

> id: network-storage.drops | does: explains what happens to clips when a network share stops answering | resolve: media offline | premiere: media offline | when: the NAS sleeps, the Wi-Fi drops or the server reboots during an edit | see: network-storage.check

**[Fork]**

A share that disappears can make a plain file read wait for minutes. Revolution gives every read,
open and size check on a network share **8 seconds**; if the share hasn't answered by then, the
clip is treated as **offline** (the same red offline mark and "Media Offline" picture as a
missing file) instead of the app freezing. Once one file on a share has timed out, Revolution
treats that whole share as down for **30 seconds**, so its other clips go offline at once
instead of each waiting 8 seconds (opening a project with many clips on a sleeping NAS stays
quick). Files on local disks are read exactly as before.

What differs from Resolve and Premiere: both also show offline media when a share vanishes, but
may stall while the operating system waits for the server; Revolution's time limit is its own.

When the share answers again, the clips come back **online by themselves**: Revolution looks at
the project's files every few seconds and whenever its window comes back to the front
(**Settings ▸ Media ▸ Automatically refresh growing files**, on by default; the same look that
reloads re-rendered files). A message says **3 clip(s) went offline** or **3 clip(s) are back
online**; nothing needs relinking and there is no undo step. With that setting off, use **Link
Media** or reopen the project instead.

Still being built: moving the last few file checks the interface does itself (audio waveforms,
the Link Media preview) to background threads.

## Send a whole project as a package

> id: network-storage.package | does: makes one folder with a project, all its media and a checklist the receiver can verify | resolve: Export Project Archive (.dra) | premiere: Project Manager ▸ Collect Files and Copy | when: handing a project to another editor or computer, or moving it to a server | see: network-storage.saving

**[Fork]** · Resolve's project archive and Premiere's Project Manager do a similar copy; Revolution
adds a manifest with a checksum for every file, so the person receiving it can tell whether
anything went missing or got damaged on the way.

1. On the **Project Manager** home screen, select the project and click **Package** in the bottom
   bar (`projects.package`), or right-click its tile and choose **Package…**
   (`projects.menu.package`). Pick the folder to put it in: a USB disk, a network share,
   anything. (Scripts and agents: `projects.package` with `path` and `destination`.)
2. Revolution makes **`<destination>/<project name> package/`** holding the project file, every
   media file it uses, and (with `"proxies": true`) their proxies. It runs in the background with
   progress in the status bar and can be stopped. Imported LUTs and title fonts are stored inside
   the project file, and voice-over takes are ordinary clips, so they travel too. The Color
   page's saved comparison frames go into a folder next to the project. The packaged project uses the receiver's own scratch disks, not yours (your folders
   would mean nothing on their computer), so those frames are found there after import; one whose
   picture file has gone missing is left out. Custom shader effects (your `.wgsl` files in the
   Effects folder) that the project uses go into **`Effects/`**; the ones it doesn't use stay home.
3. When everything is copied it reads each file once more and writes **`manifest.json`**: every
   file's path inside the package, size and SHA-256 checksum, and a list of what was left out on
   purpose (the render cache, which is rebuilt; downloaded AI models; proxies unless chosen).
   The manifest holds only paths inside the package, nothing about your own disk.
4. If anything fails (the disk fills up, a clip has gone missing, you press Stop), the half-made
   package folder is removed and the job says why. An existing package folder is never
   overwritten.

**Receiving a package.** On the home screen click **Import Package** (`projects.importPackage`)
and choose either the **`.revpkg`** file (or an older `.fcpackage`) or, for a package folder, the **`manifest.json`**
inside it; it goes into the folder of the projects list you are looking at. (Scripts: `projects.importPackage` with `path` and,
optionally, `folder`; `projects.import` of a folder with a Revolution `manifest.json` does the
same.)

1. Revolution first checks every file against the manifest: is it there, the right size, the same
   checksum. This reads the whole package once, in the background.
2. If anything is missing or damaged, the import stops and says which files (for example
   **the package is incomplete (damaged: clip.mov)**), and nothing is copied. Ask the sender for a
   new copy of those files.
3. Otherwise the package is copied into your projects folder and every clip is pointed at its
   copy there, so the project opens with nothing offline and no **Link Media** step. Custom shader
   effects from its `Effects/` folder are added to your own Effects folder; one you already have
   under the same file name is never replaced (yours is kept).
4. A package made by a newer Revolution is imported, but opening it asks you to update (a newer
   package format is refused).

**One file instead of a folder.** `projects.package` with `"format": "file"` writes a single
**`<project name>.revpkg`** (a zip with every file stored as it is, so camera media isn't
recompressed and nothing is re-encoded); the folder it is made from is removed afterwards.
`projects.importPackage` takes the `.revpkg` file (and older `.fcpackage` files) as well as a package folder: it unpacks only
the files the manifest lists, at safe places inside the new project folder, then checks and
relinks them as above. Home screen: **Package as One File…** in a tile's menu
(`projects.menu.package-file`).

To know how big a package will be before making it, scripts can run `projects.package` with
`"dryRun": true`: nothing is written and `estimateBytes` says how much would be copied. The home
screen shows that size in the status line while it packages.


## Smaller packages: only the used parts

> id: network-storage.package-trim | does: shortens MP4 and MOV media in a package to the parts its sequences use, without re-encoding | resolve: Media Management ▸ Trim | premiere: Project Manager ▸ Consolidate and Transcode | when: a package would be too big to send | see: network-storage.package

**[Fork]** · Like Resolve's Media Management trim and Premiere's Consolidate, but Revolution only
ever copies the camera's own compressed data here (Premiere's Consolidate and Transcode makes new
files), so quality is identical.

1. Scripts and agents: run `projects.package` with `"trim": true` and, if you like, `"handles"`
   (extra frames kept on each side, 30 by default).
2. MP4 and MOV media are shortened to the parts the sequences use, starting at the keyframe
   before each part (so a part can begin up to a GOP earlier than asked). Nothing is re-encoded.
3. Other files, and MP4 / MOV files with a layout this can't keep exactly (fragmented files,
   several codecs in one track, complex edit lists), are copied whole.
4. Proxies of shortened media are left out (they no longer line up); make new ones after import.

The home screen doesn't offer this yet.

## Remote locations: a server without mounting it

> id: network-storage.remote-locations | does: saves a WebDAV, SFTP or S3 server as a remote location and keeps its password in the system keyring | resolve: (none: Resolve uses mounted shares or Blackmagic Cloud) | premiere: (none: Premiere uses mounted shares or Team Projects) | when: your projects or media are on a Nextcloud, a Linux server or a MinIO bucket | see: network-storage.drops

**[Fork]** · Neither Resolve nor Premiere talks to these servers directly; you would mount them in
the system first. Revolution connects by itself, so nothing has to be mounted and a dropped
connection can't freeze the file manager.

Why: a mounted share behaves like a local disk until the network hiccups. A remote location knows
it is remote: it can show progress, resume, and keep two computers from overwriting each other.

1. Scripts and agents: add a location with `remote.add`:
   `kind` (`webDav` for Nextcloud / ownCloud / Synology, `sftp` for any SSH server, `s3` for MinIO,
   Garage or Amazon S3), `name`, `url` (`https://…`, or `http://…` on a home network;
   `sftp://host`), `user` (for S3: the access key), `root` (the folder to start in; for S3 the
   bucket, optionally `bucket/folder`) and `secret` (the password, private key or S3 secret key).
2. The secret goes to the **system keyring** (KWallet, GNOME Keyring or KeePassXC through the
   freedesktop Secret Service), never into the preferences, a project file or a log.
   `remote.list` shows where secrets are kept.
3. Check it with `remote.test` (`{"id": …}`): it connects and lists the folder, and says why if it
   can't.
4. Look inside with `remote.browse` (`{"id": …, "path": "folder/inside"}`): folders first, then
   files; names starting with a dot are hidden unless `"hidden": true`.
5. `remote.add` with an existing `id` changes that location (leave `secret` out to keep the stored
   one); `remote.remove` deletes it and its keyring entry.

6. Bring clips in with `remote.import` (`{"id": …, "paths": ["shoot/day 1/clip.mov", …], "wait":
   true}`): each file is downloaded into the media cache's **Remote Media** folder (a stopped
   download continues where it stopped; a complete copy is reused) and imported into the project.
   That folder is not cleaned automatically, so clips in use never disappear.

### In the app: File ▸ Remote Locations…

1. Choose **File ▸ Remote Locations…** (`remote.show`). The window shows the first saved location;
   pick another in the list at the top (`remote.location`).
2. Click a folder to go in, **Up** for the folder above, **Refresh** to read the folder again. Names starting
   with a dot (lock files, kept copies) are hidden.
3. Select clips (Ctrl / Shift for several) and click **Import**: they download into the media cache
   in the background and appear in the project when done.
4. Select a project file and click **Open Project**; if someone else has it open, it opens
   read-only and the status line says who.
5. Select a `.revpkg` and click **Receive Package**: it downloads (and continues if it stopped) and
   then appears in the Project Manager.
6. Type a name and click **Save Project Here** to put the open project into this folder. From then
   on **File ▸ Save** saves it to the server (with its lock and conflict copies), and
   quitting releases the lock.

Playing remote clips without downloading them first is still to come (S2).

## Send and receive a project through a server

> id: network-storage.send-receive | does: packages a project as one .revpkg and uploads it to a remote location, or downloads and imports one | resolve: (none: Resolve exports a .drp and media separately) | premiere: (none: Premiere's Project Manager copies to a folder only) | when: someone else, or your other computer, should get the whole project | see: network-storage.package

**[Fork]** · A package (see above) that travels through your server, so nobody has to carry a
drive.

1. Scripts and agents: `remote.sendProject` (`{"id": …, "path": "<the project file>", "folder":
   "inbox/ana"}`; `proxies` and `trim` as for packages). It makes one `.revpkg` in the data
   folder's outbox, uploads it as a background job (progress and Cancel in the job list; a
   dropped connection is retried three times) and removes the local package afterwards.
2. The upload is all-or-nothing: the server never shows half a package under its name.
3. On the other computer: `remote.receiveProject` (`{"id": …, "path": "inbox/ana/Reel.revpkg",
   "wait": true}`). It downloads in 8 MB pieces into the data folder's inbox; if the connection
   drops, run it again and it **continues where it stopped**. Then the package is checked against
   its manifest and imported with every clip relinked, as Import Project Package does.
4. Without `"wait": true` it downloads as a job and replies with `localPackage`; import that
   with `projects.importPackage` when the job is done.

### In the Project Manager: Send… and Receive…

**[Fork]** · The same transfer with buttons, from the screen where your projects are
(STORAGE-PLAN S2b). Resolve and Premiere have nothing like it: Resolve exports a `.drp` and you
copy the media yourself.

Send a project:

1. In the Project Manager, click the project's tile (or right-click it and choose **Send…**), then
   click **Send…** in the bottom bar (`projects.send`). With no tile selected it sends the project
   that is open.
2. In **Send Project**, pick the server under **To:** (`projects.transfer.location`) and, if you
   like, a folder on it (click a folder to go in, **Up** to go back).
3. Tick **Include proxies** if the other computer should play smoothly straight away, or **Leave
   out unused media** to keep the package small (the parts of clips the timelines don't use stay
   behind). **Size: about …** shows what will be sent with those choices.
4. Click **Send** (`projects.transfer.ok`). The bar shows how much has gone and about how long is
   left; **Cancel** stops it (sending again starts over). It ends with "Sent: <name> to <server>".

Receive a project:

1. Click **Receive…** in the bottom bar (`projects.receive`), or open the sidebar (☰) and click the
   server under **Remote** (`projects.sidebar.remote.<id>`).
2. Packages are listed newest first with their size and date. Click the one you want, then
   **Receive** (`projects.transfer.ok`).
3. It downloads (a stopped download continues where it got to the next time), is checked against
   its manifest and is imported with every clip relinked into the folder the Project Manager is
   showing. The new project's tile is selected: double-click it to open it.

No server yet? **Manage Locations…** (in the window, or at the bottom of the sidebar's **Remote**
group, `projects.manageLocations`) opens the same list as Settings ▸ Remote Locations: add a
server with its kind, address, user name, folder and password, **Test** it, or **Remove** it.

Differs from Resolve: Resolve's Project Manager can only export / import `.drp` files (without
media) or work with a shared project library database; here the whole project travels with its
media, through a plain file server.

Still to come: uploading without a local copy when the disk is nearly full.

## Send to a computer nearby

> id: network-storage.nearby | does: sends a whole project straight to another computer on the same network, with no server and no account | resolve: (none: Resolve exports a .drp and you copy the media yourself) | premiere: (none: Premiere cannot send a project to another computer) | when: your other computer, or a colleague's, is on the same Wi-Fi or office network and you have no server | see: network-storage.send-receive

**[Fork]** · The same package as above, but it goes directly from one computer to the other.
You need no server and no account. Both computers must be on the same network (home Wi-Fi or an
office LAN).

Send a project to a computer nearby:

1. On the receiving computer, open the Project Manager, click **Receive…** (`projects.receive`) and
   choose **Receive from a nearby computer** (`projects.transfer.nearby`). It shows a code such as
   `7-velvet-river-lamp`. The code works for 10 minutes and for one transfer.
2. On the sending computer, select the project's tile and click **Send…** (`projects.send`), then
   choose **Nearby computer** (`projects.transfer.nearby`).
3. Type the code into **Code:** (`projects.transfer.code`). Or click **Find**
   (`projects.transfer.find`), click the computer in the list of computers waiting (that fills in
   its address and the code's number), then type the three words. Click **Send**
   (`projects.transfer.ok`).
4. The receiving computer asks "<computer> wants to send <package> (<size>)". Click **Accept**
   (`projects.transfer.accept`), or **Decline** (`projects.transfer.decline`) if you were not
   expecting it. Nothing is written to disk before you accept.
5. Both computers show progress. When it finishes, the package is checked against its manifest and
   imported into the folder the Project Manager is showing, with every clip relinked. The new
   project's tile is selected: double-click it to open it.

Why a code, and why it looks like that. The number at the start only finds the right computer on
the network. The three words are a secret that proves the person sending is the person who saw the
screen. Someone watching the network cannot work the words out from what they see, and cannot
guess them by trying: after a wrong try the receiver throws the code away and shows a new one
("Someone tried a wrong code. Here is a new one."). Everything that travels is encrypted.

When the other computer is not found:

- Some networks stop computers from finding each other: guest Wi-Fi, some routers and access
  points. Type the receiver's address instead: the receive window shows it as `address:port`
  (for example `<lan-address>:51234`). Type it into **Address:** (`projects.transfer.address`) under the code, or click **Find** (`projects.transfer.find`) to list the computers waiting to receive and pick one (`projects.transfer.receiver.<n>`).
- A firewall may be in the way: the send then says "Couldn't reach … A firewall on the other
  computer may be blocking it". Revolution receives on **TCP 47821** (Settings ▸ Remote Locations ▸
  **Port for receiving from a nearby computer**, `remote.nearbyPort`; 0 = this standard port; if
  another program has it, the receive window says which port it took instead) and computers find
  each other on **UDP 47820**. Open **both** once on the **receiving** computer:
  - **Linux, installed from the .deb / .rpm / Arch package:** the package brings a firewall
    description named Revolution (like KDE Connect's) but never switches it on by itself. The
    receive window shows the command for your firewall (`projects.transfer.firewall`): with ufw
    `sudo ufw allow Revolution`, with firewalld
    `sudo firewall-cmd --permanent --add-service=revolution && sudo firewall-cmd --reload`.
  - **Linux, AppImage / portable folder / Flatpak** (or to allow only your own network):
    `sudo ufw allow from <lan-address>/24 to any port 47821 proto tcp` and
    `sudo ufw allow from <lan-address>/24 to any port 47820 proto udp` (use your network's numbers:
    `ip -4 addr` shows e.g. `<lan-address>/24`, so the network is `<lan-address>/24`).
  - **Windows** asks the first time Revolution receives: allow it on **private networks**. **macOS**
    asks once.
- The Address box takes `<lan-address>:47821`; a `/24` pasted from `ip addr` is ignored.

If the connection drops, send again with the same code within the 10 minutes: the transfer
**continues where it stopped**. After 10 minutes the code expires; make a new one.

For scripts and agents (all commands are in the engine; each returns JSON):

- `nearby.receive` (`{}`): starts waiting and replies at once with the `code`, the `port` and any
  `warnings` (discovery that couldn't start); a job waits for the sender.
- `nearby.wait` (`{"accept": true, "folder": "In"}`): blocks until that receive ends, accepting the
  offer automatically, then imports the project (unattended receive: run it right after
  `nearby.receive`). Without `"accept"` someone answers with `nearby.accept`.
- `nearby.send` (`{"code": "7-velvet-river-lamp", "path": "<project>.fcproj"}`): packages and sends
  (a `.revpkg` goes as it is). Add `"address": "<lan-address>:51234"` when discovery can't find
  the receiver; `proxies` / `trim` as for packages; `"wait": true` blocks until it is sent.
- `nearby.list`: the computers currently waiting to receive (`channel` and `address`).
- `nearby.status`: the code, port, an offer waiting for an answer (computer, package, size) and
  the latest note. Progress is in the job list, as for every transfer.
- `nearby.accept` / `nearby.decline`: answer the offer (no answer within two minutes declines).
- `nearby.stop`: stop waiting or receiving.

Differs from Resolve and Premiere: neither can send a project to another computer directly. Resolve
exports a `.drp` (no media) or needs a shared project server; Premiere copies to a folder.

## Projects on a remote location: open, save, locks

> id: network-storage.remote-projects | does: opens and saves a project on a server with a lock and conflict copies | resolve: (none: Resolve shares projects through a project server or Blackmagic Cloud) | premiere: (none: Premiere uses Productions or Team Projects) | when: you edit the same project from two computers through a server | see: network-storage.remote-locations

**[Fork]** · Resolve and Premiere solve this with their own project servers. Revolution keeps a
normal project file on your server and puts a small lock file next to it.

Why: two computers saving the same file is how a day's work disappears. The lock tells the
second computer who has the project open; a save that would overwrite someone else's work
becomes a separate copy instead.

1. Scripts and agents: open with `remote.openProject` (`{"id": …, "path": "films/cut.fcproj"}`).
   Revolution downloads it to a working copy in its data folder (autosave and the local backup keep
   working on that copy) and takes the lock: a hidden `.cut.fcproj.revlock` next to the project
   with your name, computer and the time.
2. If another computer holds the lock, the project opens **read-only** and the reply says who has
   it (`heldBy`). To take it over (they are gone, or you agreed), open again with
   `"takeOver": true`.
3. Save with `remote.saveProject`: it saves the working copy, then uploads it. The copy that was on
   the server is kept in a hidden folder next to the project (the newest 10).
4. If someone took the project over or changed the file on the server meanwhile, your save is
   uploaded as **`cut (conflict from <computer> <date>).fcproj`** next to it, and theirs stays as it
   is. Open both and copy over what you need.
5. `remote.saveProjectAs` (`{"id": …, "path": …}`) puts the open project on a location (it refuses
   to replace an existing file unless `"overwrite": true`, and then keeps the old copy).
6. `remote.closeProject` releases the lock. A computer that crashed with the project open leaves
   its lock behind: `remote.unlock` removes it. `remote.projectStatus` shows what is open and whether
   it is read-only.
7. Some servers (nginx's WebDAV module, older MinIO / Garage) ignore the "create only if nothing is
   there" request locks rely on. Settings ▸ Remote Locations ▸ Test checks this and warns: on such a
   server don't open one project on two computers at once.

**Still to write (S1):** the rest of project packages: written as each S1 step lands.
