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
| #24 | Covers in the terminal (`ratatui-image`) | Code complete (PRs #99–#105, all 7 parts); **issue left open on GitHub, worth closing by hand** |

The catalog (search, an album, an artist, and now the library) is served by
the **daemon**, not the TUI process: the TUI depends only on `phonia-ipc`, so
it builds and is tested without ALSA, and the daemon is the single owner of
the TIDAL session. See `docs/DECISIONS.md` for why.

True color is a deliberate, deferred option: the TUI uses only the terminal's
16 ANSI colors today, on purpose, even now that Phase 2 (and its covers) are
done — it is its own product decision, not a leftover.

### Phase 3 — Audio quality

| Issue | What | Status |
|---|---|---|
| #27 | Gapless playback | Closed (verified bit-exact against real TIDAL over `snd-aloop`) |
| #29 | Quality tiers, a floor, and automatic fallback | Closed (parts 1–3); an optional part 4 (AAC decode for the lossy tiers) is not started and not blocking |
| #25 | DAC capability detection | In progress: approved 4-PR plan; part 1 (real joint rate/format probing, `output/caps.rs`, `probe-device` fixed) merged |
| #26 | Per-track sample rate switching | In progress, same plan as #25 (see below) |
| #28 | Signal path indicator in the TUI | Not started |
| #30 | ReplayGain in shared mode | Not started |
| #31 | Hardware mixer volume | Not started |

### Phases 4 and 5

Not started, except CI (#42) and rustfmt-in-CI (#50), both closed. Nothing
here is planned in detail yet; issues #32–#45 hold one-line descriptions each,
to be scoped with Opus when their turn comes.

## Right now

Phase 2 is fully closed (#17–#24, tagged `v0.2.0`). Work has moved to Phase
3: **#25 (DAC capability detection) and #26 (per-track sample rate
switching)**, planned together with Opus since they are two sides of one
mechanism, as an approved 4-PR plan.

What the investigation behind that plan found: the "reopen the PCM when a
track's format changes" mechanics #26 asks for **already exist** at the
engine level (`start_track`/`open_sink`/`join_next` in
`engine/audio_thread.rs`), tested against a fake sink. The real gap is #25:
today `AlsaSink::open` picks a format by blindly trying a hardcoded priority
list against the device and asks for the track's exact rate on faith, so an
unsupported one surfaces as a raw ALSA error instead of a clear refusal, and
a refused track's failure was never even reported as `TrackEnded` to
clients (a bug found along the way, fixed in part 3).

Approved design: live `HwParams` probing (on the PCM already being opened)
decides everything, not parsing `/proc/asound/cardN/stream0` as the issue
literally suggests — `stream0` is USB-only, pre-quirks, and blind to live
device state (another substream holding the rate, a replugged DAC), so it
is relegated to a passive, display-only addition to `phonia devices` (part
4). No capability cache: probing is cheap (microseconds) and a cache would
go stale in exactly the cases live probing handles for free. The policy
stays **bit-perfect or refuse** in exclusive mode — shared/PipeWire remains
the one deliberate, clearly-labelled non-bit-perfect exception, untouched
by this work — what improves is the refusal *message* (precise: what was
asked, what the device actually offers), not the policy. See
`docs/DECISIONS.md` for the full reasoning and the plan's own open
decisions as the person settled them.

Part 1 (`output/caps.rs`: pure, unit-tested capability types and probing;
`phonia probe-device` fixed — it used to report "yes" to everything on a
`plughw:`/`default` device, since it never disabled automatic resampling)
is merged. Remaining: part 2 (the best format per track, and a precise
refusal, in `AlsaSink::open`), part 3 (the engine reports a refused track's
`TrackEnded` correctly), part 4 (`phonia devices` shows what a USB DAC
advertises via `stream0`, passively).

Verified against the real Fosi Audio DS2: it accepts every TIDAL rate in
`S16_LE`, `S24_3LE` and `S32_LE` (not `S24_LE`) — confirmed both via the new
`probe-device` and a dedicated `#[ignore]`d test, run by hand with the DAC
freed from PipeWire first. Because of that, the *refusal* path (a rate or
format this DAC can't do) cannot be exercised against it and is tested with
fakes instead.

## Conventions this file assumes

- Issues are closed by the project owner by hand after their pull requests
  merge, not automatically (pull requests target `develop`, which GitHub does
  not auto-close issues against). A "Code complete" row above with the issue
  still open on GitHub is not a bug in this file; it means the owner has not
  closed it yet.
- Every pull request should carry the milestone of the issue it is part of,
  its assignee, and an `enhancement` (or `bug`/`documentation`) label, so
  GitHub's own filtering stays a second, independent source of truth.
