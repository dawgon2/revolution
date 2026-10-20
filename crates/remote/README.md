# filmcraft-remote

Remote locations for projects and media (STORAGE-PLAN.md Phase S2; layer L2, network code below the UI).

- `RemoteStore`: the one trait every backend implements: `list`, `stat`, `read_range`, `write`,
  `create_new` (write only if nothing is there: the primitive lock files are built on), `rename`,
  `delete`, `make_dir`.
- `RemotePath`: a checked, `/`-separated path relative to a location's root. `..`, empty
  segments, control characters and over-long paths are refused, so a hostile listing or project
  file can't point outside the location.
- `lock`: project lock files (holder, machine, time) on any backend: acquire, read, take over,
  release.
- `MemoryStore`: an in-memory backend for tests (and a reference for the trait's semantics).
- `WebDav` (feature `net`): PROPFIND listings (every server `href` decoded and re-checked as a
  `RemotePath`, anything outside the folder skipped), ranged GETs, uploads as PUT to a temporary
  `.<name>.part-<random>` then MOVE, `create_new` as PUT with `If-None-Match: *`; folders are
  never DELETEd. Pure-Rust TLS (ureq + rustls/RustCrypto); plain `http://` works for LAN servers.
- `S3` (feature `net`): Amazon S3 and compatible servers (MinIO, Garage): Signature V4,
  path-style by default, folders as key prefixes (+ `dir/` markers), multipart uploads above
  64 MiB that can be resumed (`S3::write_resumable` / `MultipartState`), `create_new` as PUT
  with `If-None-Match: *`; rename = copy + delete (not atomic).
- `sftp` (feature `sftp`): any SSH server via `russh` 0.52 + `russh-sftp` on RustCrypto only (no
  ring / aws-lc / OpenSSL). Password or OpenSSH/PEM private key (no passphrases yet); host keys
  trusted on first use and pinned in a known-hosts file the caller names; uploads go to a
  temporary sibling and are renamed over the target.

Credentials never live in a `Location` (it is serialised into settings); they come from the
system keyring at connect time.
