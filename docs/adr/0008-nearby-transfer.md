# ADR 0008: Sending a project to a nearby computer without a server (fork)

- **Status:** accepted for the fork (2026-10-20, S2b step 1; written in a LEAN-usage run without a
  separate designer: **provisional**, open to the owner's change like any §1a choice). Scope set by
  [`STORAGE-PLAN.md`](../../STORAGE-PLAN.md) Phase S2b item 4 and FORK-PLAN §1a.111 (the plan text
  called this "ADR 0010"; 0008 is the next free number).
- **Builds on:** S1.5 (`.revpkg` packages with a manifest, `projects.importPackage`), S2 item 6
  (`remote.sendProject` / `remote.receiveProject`: outbox, inbox `.part` files, resume, jobs).

## Context

The owner wants to send a project from one computer to another on the same network with nothing
to set up: no server, no account, no port forwarding. A local network is hostile ground: anyone on
it can see multicast traffic, answer discovery, connect to an open port, or send garbage. The
transfer must therefore be authenticated and encrypted by something the two people share for a
moment (a short code read off one screen and typed on the other), must fail cleanly and safely on a
wrong code, and must never write anything the receiver hasn't accepted. Everything stays pure Rust
(§1) and runs on Linux, Windows and macOS; nothing in the wasm build.

## Decision

1. **Where it lives.** A `nearby` module in `filmcraft-remote` (L2) behind a new `nearby` feature
   (native only; off in wasm). It has no UI and no engine types: it takes a file path and a code
   and reports progress through callbacks and an `AtomicBool` cancel flag. The engine wraps it as
   jobs and commands (`nearby.receive`, `nearby.send`, `nearby.list`) in
   `crates/engine/src/nearby.rs`; the UI only calls the commands. Plain blocking `std::net` with
   timeouts on worker threads (no async runtime).
2. **New dependencies** (all pure Rust, MIT / Apache, checked 2026-10-20): `spake2` **0.4.0**
   (RustCrypto PAKEs, on `curve25519-dalek` 4 already in the tree; the 0.5 line is a pre-release),
   `mdns-sd` 0.21 (no async runtime; deps `if-addrs`, `socket2`, `socket-pktinfo`, `mio`,
   `flume`, `fastrand`, `log`: OS bindings only, no C), `chacha20poly1305` 0.10 and `hkdf` 0.12
   (already in `Cargo.lock` through other crates), `sha2` 0.10, `getrandom`.
3. **The code.** `<n>-<word>-<word>-<word>`, e.g. `7-violet-river-lamp`. `n` (1–99) is the
   **channel**: it is public and is what discovery advertises. The three words are the **secret**:
   each from an original 256-word list of short, common, distinct English words written for this
   repo (`crates/remote/src/nearby/words.rs`; no two share their first four letters), 24 bits
   together. (The plan's example had two words; two words from a list we can write and check by
   hand would be only 16 bits, and a 4096-word list can't be checked by hand, so: three.) Typing is
   forgiving: case, spaces, and `-` / space / `.` separators are normalised; a word is accepted by
   its first four letters. A code is valid for **10 minutes** and only while the receive dialog is
   open. It is **burned** by a failed key exchange, a Decline, a finished transfer or the expiry.
   One guess per code at 2^-24 is the attacker's whole chance; a burned code means the receiver
   shows a new one ("Someone tried a wrong code. Here is a new one.").
4. **Discovery.** The receiver advertises DNS-SD `_revolution-recv._tcp.local.` with the channel
   number as the instance name, its TCP port, and TXT `v=1`; nothing else (no project name, no user
   name). Fallback when multicast is blocked: the receiver also answers UDP broadcast probes on
   port 47820 (`REVNEAR1?<n>` → `REVNEAR1!<n> <tcp port>`), and the sender broadcasts one when
   mDNS finds nothing within 2 s. If both fail the sender's dialog offers "Enter the other
   computer's address" (host or IP, optional `:port`). `nearby.list` returns receivers currently
   waiting (channel + address), at most 16, so the sender can pick one and type only the words.
   A sender tries at most 8 candidates with the channel number, one after another.
5. **Key exchange.** After TCP connect each side sends a hello (`REVNEAR1`, protocol version, role)
   and its symmetric SPAKE2 message (`Ed25519Group`, identity `revolution-nearby-v1/<n>`, password
   = the normalised code). From SPAKE2's key, HKDF-SHA256 derives four keys: sender → receiver and
   receiver → sender record keys, and two confirmation keys. Each side then sends one AEAD record
   holding its confirmation tag over the transcript (both hellos and both SPAKE2 messages). A wrong
   code makes the tag fail on both sides: the connection closes with "The code didn't match", the
   receiver burns the code, and nothing about the code leaks (that is what a PAKE is for).
6. **Records.** Every message after the handshake is a record: `u32` big-endian ciphertext length
   (≤ 1 MiB + 16) + ChaCha20-Poly1305 ciphertext. The nonce is the record counter (64-bit, starts at
   0, one per direction, never sent; a gap, replay or reorder fails decryption). The associated data
   is the record kind byte. Kinds: `Confirm`, `Offer` (JSON ≤ 4 KiB: sender computer name ≤ 64
   chars, package file name ≤ 255 bytes, size, SHA-256), `Accept { offset }`, `Decline`,
   `Data` (≤ 1 MiB plaintext), `Done`, `Error { message ≤ 512 bytes }`.
7. **Accept / Decline before anything is written.** The receiver shows "<computer> wants to send
   <package> (<size>)" with **Accept** and **Decline**. Nothing touches the disk before Accept.
   The package is written to `<data>/remote/inbox/nearby/<sha256>.part`; on `Done` its SHA-256
   must match the offer, then it goes through the **same manifest check and relinked import** as
   any package (`projects.importPackage`). A failed check deletes the `.part`: never a
   half-imported project.
8. **Resume.** If the connection drops after Accept, the receiver keeps the `.part` and the code
   stays valid (it has already authenticated a peer) for the rest of its 10 minutes. The sender
   retries automatically three times (and on "Try again"); the new handshake uses the same code,
   and when the offer's SHA-256 matches the `.part`, `Accept { offset }` continues from its
   length. A different package with the same code is declined.
9. **Listening.** Only while the receive dialog (or a `nearby.receive` job) is open, on an
   ephemeral TCP port on all interfaces; a **Fixed port** setting (`nearby.port`, 0 = any) for strict
   firewalls. One peer at a time; others get `Error { "busy" }`. Windows Defender Firewall and
   firewalld may ask or block; the guide says how to allow it or use the fixed port.
10. **Hostile input (never crash).** Every length and count is capped before allocating (record
    length, offer size, names, candidate and list counts, package size ≤ `MAX_PACKAGE_BYTES`);
    timeouts on every read (handshake 30 s, idle 60 s, the Accept prompt 2 min); all parsing
    returns `Result`; worker threads run under `catch_unwind`. Tests include a fuzzing peer that
    sends random and truncated bytes at every stage, oversize lengths, replayed / reordered records,
    a wrong code, a decline, a cancel on both sides and a drop mid-transfer followed by resume.

## Consequences

- Two strangers on a café network can't read or alter a transfer, and can't learn the code by
  watching; an active attacker gets one guess per code and is noticed (the code is replaced).
- A network that blocks both multicast and broadcast needs the typed address; one that blocks
  inbound TCP needs the firewall rule or the fixed port. Both are documented, not worked around.
- Works between our own builds only (protocol `REVNEAR1`, versioned); interoperability with
  other tools is a non-goal.
- Rejected: a relay / rendezvous server (the owner asked for no server); TLS with self-signed
  certificates (needs fingerprint comparison, worse for beginners than a code); deriving keys
  from the code directly with a KDF (offline-guessable from one captured transfer: a PAKE isn't).
