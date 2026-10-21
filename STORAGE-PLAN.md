# STORAGE-PLAN.md: projects and media on a home server

Written 2026-10-12 (live session, with the owner). Order: FORK-PLAN §1a.6 / §1a.50 / §1a.51 (S1 → S2 after
C2, its AI2 training and AUDIO-PLAN A2, before G3; so Phase 7's soak covers network drops). Every FORK-PLAN rule
applies (never crash, `Tick` time, everything is a command, every widget has an automation id,
layering: network code stays below the UI and never in L0, `cargo xtask ci` per phase, a build
and a `cachyos` publish per phase). Generic, not fork-only: written to be upstream-contributable.
Not in scope: multi-user collaboration (several people editing one project at once, ≈ 100+ h).

## Phase S1: network shares done right, and project packages (≈ 18–35 h)

SMB / NFS shares mounted by Plasma (kio-fuse), fstab or autofs already look like local folders;
this phase makes FilmCraft safe and smooth on them.
1. **Detect network paths** (statfs filesystem type: cifs, nfs, fuse.kio-fuse, sshfs…) and show
   it in the Media panel / Project Settings.
2. **Local media cache:** clips on a network path can be cached locally (whole file in the
   background, or just proxies / render cache, which default to local disk anyway), with a size
   cap and the existing claim-folder rules; playback reads the cache when it's complete.
3. **Server drops:** every read on a network path has a timeout off the UI thread; a dropped
   share turns its clips **offline** (the existing offline badge), never a hang or crash; when
   the share returns they relink by identity automatically.
4. **Safe saves:** write to a temp file on the same share, fsync, rename; verify by re-reading;
   always keep a local backup copy of the last good save (and autosaves locally), because rename
   over SMB isn't always atomic. A save that fails says so and keeps the local copy.

5. **Send a whole project (owner, 2026-10-15, FORK-PLAN §1a.82): a project package.** Today
   `projects.export` writes `<name> archive` (the `.fcproj` plus copies of its media) and Import takes
   only the `.fcproj` (clips then need Link Media). Make it one complete, checked package:
   - **Everything the edit needs:** the `.fcproj` (timeline, audio mix and effects, grade nodes,
     compositions; LUTs are already embedded), all media it uses incl. nested-sequence and comp
     sources, voice-over takes, stills gallery images, title fonts that aren't bundled, user WGSL
     effects from the Effects folder, and subtitles / caption files. Optional: proxies; never the
     render cache (rebuilt on the other side) or downloaded models (named in the manifest instead).
   - **Options:** full media, or **trimmed to the used ranges + handles** (default 1 s; long-GOP
     camera files are cut at keyframes without re-encoding where the container allows, else copied
     whole: never a silent re-encode); one folder or one `.fcpackage` file (zip, stored, so 6.2K
     media isn't recompressed); a size estimate before it starts.
   - **A manifest** (relative paths, sizes, SHA-256 per file, FilmCraft version, what was left out
     and why) so the receiver can verify it; a damaged or partial package says what's missing.
   - **Import a package:** open the `.fcpackage` / folder, verify, unpack to a chosen place, and
     **relink everything automatically** (no Link Media step); a newer project version than the
     app warns instead of failing.
   - Commands `projects.package` (with `trim`, `handles`, `proxies`, `format`, `wait`) and
     `projects.importPackage`; a background job with progress and Stop; never crash on a full
     disk or a vanished source (clean error, partial output removed).
   - Works to any destination folder, so a mounted share (items 1–4) is already a way to send it.

**Accept:** with a local Samba / NFS server in a container or VM (or the Dell as the server): play
and edit a project stored on the share; pull the network mid-playback and mid-save — UI stays
responsive, clips go offline, no data lost (the local backup opens), reconnect relinks; 6.2K
playback from the cache matches local playback (synthetic). Tag `storage-1`.

## Phase S2: built-in remote locations, and sending projects (≈ 35–70 h)

**Owner, 2026-10-17 (FORK-PLAN §1a.104):** S2 runs right after `xps-1`, and it renames the one-file package from
`.fcpackage` to **`.revpkg`** (both open; new packages are `.revpkg`).

Connect without mounting anything.
1. **Backends** behind one `RemoteStore` trait (list, stat, read range, write, rename, lock):
   **WebDAV** (Nextcloud, ownCloud, Synology), **SFTP** (any Linux server; pure-Rust `russh`,
   Apache-2.0) and **S3-compatible** (MinIO, Garage; signed requests over the workspace's
   rustls / ureq). Pure Rust, no system libraries required.
2. **Settings ▸ Remote locations:** add / test / remove a server; **passwords and keys in the
   system keyring** (Secret Service / KWallet over zbus, a file-backed fallback only if the user
   allows it), never in project files or logs.
3. **Media panel:** browse a remote location like a folder; import downloads into the local
   media cache (S1) with progress and resume; or link remote media and stream with read-ahead
   when the connection is fast enough.
4. **Projects on the server:** open / save / Save As to a remote location; a **lock file**
   (holder, machine, time) so two machines can't overwrite each other (read-only open or take
   over, with a warning); conflict copies instead of overwrites; the last N versions kept on the
   server; local backup always (S1.4).
5. Commands `remote.*` (add, test, list, import, open, save, unlock) for CLI / MCP.
6. **Send / receive a project package (FORK-PLAN §1a.82):** "Send Project…" packages (S1.5) and
   uploads to a remote location with progress and resume (large files in chunks; S3 multipart);
   "Receive Project…" downloads, verifies the manifest and opens it relinked. Optional upload
   without a local copy (stream the package) when disk space is short. `remote.sendProject` /
   `remote.receiveProject`.

**Accept:** against local test servers (Nextcloud or another WebDAV server, OpenSSH, MinIO, in
containers): browse, import, open and save on each; a second "machine" (another process) sees the
lock; kill the connection mid-transfer → resume or clean error, never a corrupt project; no
credential appears in any file under the project or log dir (checked by a test). Tag
`storage-2`.

## Phase S2b: send and receive from the Project Manager, and computer to computer (owner, 2026-10-20; ≈ 30–55 h)

**Owner, live, 2026-10-20 (FORK-PLAN §1a.111):** "yes please I want all of this in the project manager", finished
before 0.2, "yes" to direct sending with no server. Runs right after C2; part of Revolution 0.2. Tag `storage-3`.

1. **Send Project…** (Project Manager footer and right-click menu, Edit ▸ / File ▸ as the workflow names it): pick the
   project tile (or the open project), then **where**: a saved remote location (with a folder picker that browses
   it) or **Nearby computer** (item 4). Options: include proxies, trim unused media (S1.5's package options), size
   shown before sending. Upload as a job with progress, speed, time left and Cancel in the screen and the job list;
   resumes after a dropped connection (`remote.sendProject`). Ends with "Sent: <name> to <where>".
2. **Receive Project…** next to it: pick a location, browse to a `.revpkg` (newest first, with size and date), or
   **Receive from a nearby computer** (item 4). Downloads with progress / resume, verifies the manifest, imports
   relinked into the projects folder and selects the new tile (`remote.receiveProject` + `projects.importPackage`).
3. **Locations in the Project Manager:** a **Remote** group in the sidebar listing saved locations (click: browse it
   in the grid, `.revpkg` and project files as tiles; Open, Receive, Send here) and **Manage Locations…**, a
   dialog with what Settings ▸ Remote Locations has today (add with kind / URL / user / folder / password, Test,
   Edit, Remove; passwords to the keyring), the same code (one widget, two places). Empty state: "Add a server to
   send projects through it" with the button.
4. **Nearby computer, no server (LAN, pure Rust):** the receiver clicks **Receive from a nearby computer** and sees a
   short code (e.g. `7-violet-river`, ~ 2^24 strength, valid 10 minutes, one use); the sender clicks Send ▸ Nearby
   computer and types it (or picks the receiver from a list of nearby Revolution computers waiting to receive,
   then types only the code). Design ([ADR 0008](docs/adr/0008-nearby-transfer.md), written first; the code became `7-violet-river-lamp`, 3 words = 24 bits): discovery by mDNS / DNS-SD on the local network
   (`_revolution-recv._tcp`, the code's first part as the instance name, nothing else advertised) with a UDP
   broadcast fallback; a **PAKE** (SPAKE2 over the code; RustCrypto `spake2`, MIT / Apache, new dependency
   checked under §1 rules) derives the key, so a wrong code fails without revealing anything and nobody on the
   network can read or tamper; the stream is ChaCha20-Poly1305 (already in the tree) in 1 MiB records with
   counters; the receiver shows the sender's computer name, package name and size and asks **Accept / Decline**
   before anything is written; resume after a drop (same code, offset); listens only while the receive dialog
   is open, on an ephemeral TCP port (Windows / firewalld prompts noted in the guide; a fixed-port setting for
   strict firewalls). Hostile input: every length / count bounded, the package lands in the inbox and goes
   through the same manifest check as any import, never-crash tests with a fuzzing peer.
5. Commands for agents: `nearby.receive` (returns the code, waits as a job), `nearby.send` (`{"code", "path"}`),
   `nearby.list`; the UI ids registered (`projects.send`, `projects.receive`, `projects.sidebar.remote.<id>`,
   `projects.manageLocations`, `nearby.*`).
6. Guide (network-storage.md: the "still to come" line goes; a new "Send to a computer nearby" section with a
   beginner path and why the code), FEATURE-QUEUE rows, i18n.

**Accept:** two app instances on one machine (and the PC ↔ the Dell over the LAN) send and receive a real project
both ways through each server kind (test containers) and nearby; a wrong code, a decline, a cancel, a pulled
cable mid-transfer each end cleanly (resume where it applies), never a half-imported project; screenshots of
every dialog checked in both workflows; ci green.

