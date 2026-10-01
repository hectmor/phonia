# Roadmap

Where phonia stands, phase by phase. GitHub's issues and milestones are the
authoritative record of what is open or closed; this file is a short, current
summary of the same thing, meant to be readable in one pass — by a person
picking the project back up, or by another model asked to audit it — without
having to reconstruct it from sixty-odd pull requests.

It is kept up to date as work lands: when a part of an issue merges, this file
is updated in the same session, not as an afterthought. `docs/DECISIONS.md`
is the companion record of *why* things are built the way they are; this file
is only *what* is done and what remains.

## How this project is worked on

One person (@hectmor) directs the project; an AI pair does the planning and
the implementing. See the "Working process" section of the [README](README.md#working-process)
for the full detail (branching, PR conventions, testing discipline). In short:
a harder design question gets planned by a more capable model first, approved
by the person, then implemented by a faster one, one small pull request at a
time, each off `develop`, never stacked on another open one. Both models, and
anyone else, are expected to be able to pick up mid-phase from this file, the
decisions log, and the issue tracker alone.

## Phases

| Phase | Milestone | Status |
|---|---|---|
| 0 — CLI prototype | [Fase 0](https://github.com/hectmor/phonia/milestone/1) | Done (#1–#7, #46) |
| 1 — Daemon and playback engine | [Fase 1](https://github.com/hectmor/phonia/milestone/2) | Done (#8–#16, #47, #52, #53) |
| 2 — TUI base | [Fase 2](https://github.com/hectmor/phonia/milestone/3) | In progress (see below) |
| 3 — Audio quality | [Fase 3](https://github.com/hectmor/phonia/milestone/4) | In progress (see below) |
| 4 — SONE-like features | [Fase 4](https://github.com/hectmor/phonia/milestone/5) | Not started |
| 5 — Extras and packaging | [Fase 5](https://github.com/hectmor/phonia/milestone/6) | Partly started |

Phases 0 and 1 delivered: TIDAL PKCE login, HiRes/DASH streaming, bit-perfect
ALSA output, a playback engine and in-memory queue, the daemon and its IPC
protocol, DAC reservation (`org.freedesktop.ReserveDevice1`), a shared mode
through PipeWire for any output (not bit-perfect), the TIDAL session in the
desktop keyring, and TOML configuration.

### Phase 2 — TUI base

| Issue | What | Status |
|---|---|---|
| #17 | ratatui skeleton, vim-style navigation, a help drawn from the key table | Closed |
| #18 | The TUI as an IPC client: connect, reconnect, subscribe | Closed |
| #22 | Queue view with editing (play, remove, move, clear) | Closed |
| #23 | Playback bar: progress, format/quality, volume, shuffle/repeat | Closed |
| #19 | Search (tracks, albums, artists, playlists) | Code complete (PRs #82–#87 merged); **issue left open on GitHub, worth closing by hand** |
| #20 | Album and artist views, opened from a search result | Closed |
| #21 | Library: favorite tracks/albums and the user's playlists | Code complete (PRs #95–#98) |
| #24 | Covers in the terminal (`ratatui-image`) | In progress: catalog ids (PR #99), IPC/daemon + `phonia_ipc::image::url` (PR #100), and the TUI's own dependencies and fetch/decode pipeline (PR #101) merged; nothing draws a cover yet |

The catalog (search, an album, an artist, and now the library) is served by
the **daemon**, not the TUI process: the TUI depends only on `phonia-ipc`, so
it builds and is tested without ALSA, and the daemon is the single owner of
the TIDAL session. See `docs/DECISIONS.md` for why.

True color is a deliberate, deferred option: the TUI uses only the terminal's
16 ANSI colors today, on purpose, until the rest of Phase 2 (and its covers)
are done.

### Phase 3 — Audio quality

| Issue | What | Status |
|---|---|---|
| #27 | Gapless playback | Closed (verified bit-exact against real TIDAL over `snd-aloop`) |
| #29 | Quality tiers, a floor, and automatic fallback | Closed (parts 1–3); an optional part 4 (AAC decode for the lossy tiers) is not started and not blocking |
| #25 | DAC capability detection | Not started |
| #26 | Per-track sample rate switching | Not started |
| #28 | Signal path indicator in the TUI | Not started |
| #30 | ReplayGain in shared mode | Not started |
| #31 | Hardware mixer volume | Not started |

### Phases 4 and 5

Not started, except CI (#42) and rustfmt-in-CI (#50), both closed. Nothing
here is planned in detail yet; issues #32–#45 hold one-line descriptions each,
to be scoped with Opus when their turn comes.

## Right now

#21 (library) is done; #24 (covers), the last of Phase 2, is in progress, as
an approved 7-PR plan: core catalog ids (#99), IPC/daemon + the
`phonia_ipc::image::url` helper (#100), and the TUI's new dependencies with
its terminal-detection and fetch/decode/encode pipeline (#101) are merged —
with no visible change yet, by design (nothing asks for a cover): what
actually draws one in an album/playlist header, an artist page, and the
queue's now-playing pane is parts 4–7. One adjustment from the plan as
written: wiring the run loop's own cover-fetching (the `--covers` flag,
querying the terminal at startup, the channel that brings a fetch back) is
folded into part 4, its first real caller, rather than built ahead of one
in part 3 — the same "every PR compiles, nothing speculative" rule the rest
of this project already follows.

The key architectural call: the **TUI fetches and decodes covers itself**,
straight from TIDAL's public, unauthenticated image CDN; the daemon's only
job is to put the right image id (a UUID, not a URL) in what it already
sends. This refines, rather than reverses, the #19 decision that the catalog
is served by the daemon: that decision was about owning the *TIDAL session*,
and the image CDN needs none, while decoding an image for a specific
terminal's protocol and cell size has to happen wherever it is drawn anyway.
True color stays a separate follow-up, not part of #24: the interface's
accent colors are a product decision of their own, distinct from being able
to show a picture at all. See `docs/DECISIONS.md` for the full reasoning.

Verified against the real CDN: every size claimed for an album cover and an
artist picture is genuinely served (and one size larger genuinely is not),
confirmed against this account's own real cover and picture ids. Playlist
cover sizes are not yet confirmed the same way, for lack of a playlist on
this account; they are TIDAL's other clients' own choices, unverified here.

## After #24

Phase 2 closes with #24. Then Phase 3's remaining issues (#25, #26, #28,
#30, #31), planned one at a time with Opus as they come up, unless the
person redirects.

## Conventions this file assumes

- Issues are closed by the project owner by hand after their pull requests
  merge, not automatically (pull requests target `develop`, which GitHub does
  not auto-close issues against). A "Code complete" row above with the issue
  still open on GitHub is not a bug in this file; it means the owner has not
  closed it yet.
- Every pull request should carry the milestone of the issue it is part of,
  its assignee, and an `enhancement` (or `bug`/`documentation`) label, so
  GitHub's own filtering stays a second, independent source of truth.
