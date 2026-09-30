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
| #24 | Covers in the terminal (`ratatui-image`) | Not started; last of the phase by design (heavier dependency) |

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

#21 (library: favorites and playlists) is code complete, its 4 pull requests
all merged: core catalog methods (#95), IPC/daemon (#96), `phonia ctl
library` (#97), and the TUI's own Library section (#98), which reuses the
same view/stack machinery the album and artist views from #20 already have —
opening a favorite album or a playlist from it works exactly like opening
one from a search result. The one deliberate difference: `Enter` on a
favorite track plays just that track rather than queuing the rest of the
list from there, since a favorites list has no natural order and can run
into the thousands. This closes out everything in Phase 2 except #24
(covers), which was always meant to be last. Favorites and playlists are
fetched with raw HTTP, like search and the album/artist views, since
`tidlers`' own favorites calls have a parameter-name typo that breaks
paging; the protocol stayed additive under 1.6 throughout (no version bump
for the whole of #19-#21). Verified against the real TIDAL API, live against
a real daemon with `ctl library`, and by driving the real TUI over a pty
against that daemon: favorite albums came back correctly; this account has
no favorite tracks or own playlists right now, and `/users/{id}/playlists`
was confirmed to answer with a real, paged `totalNumberOfItems` that
`tidlers`' own model doesn't even capture.

## After #21

#24 (covers) finishes Phase 2. Then Phase 3's remaining issues (#25, #26,
#28, #30, #31), planned one at a time with Opus as they come up, unless the
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
