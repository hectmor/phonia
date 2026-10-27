# Roadmap

Where phonia stands, phase by phase. GitHub's issues and milestones are the
authoritative record of what is open or closed; this file is a short, current
summary of the same thing, meant to be readable in one pass — by a person
picking the project back up, or by another model asked to audit it — without
having to reconstruct it from a hundred-odd pull requests.

It is kept up to date as work lands: when a part of an issue merges, this file
is updated in the same session, not as an afterthought. This file is only
*what* is done and what remains. The companion records are
[`docs/DECISIONS.md`](docs/DECISIONS.md) for *why* things are built the way
they are, [`CONTRIBUTING.md`](CONTRIBUTING.md) for *how* the project is worked
on, and the [README](README.md) for what phonia is.

## Releases

| Tag | Date | What |
|---|---|---|
| [`v1.0.0`](https://github.com/hectmor/phonia/releases/tag/v1.0.0) | 2026-10-14 | First release off `main`: phases 0–3 and 6 complete, 4 and 7 code complete |
| [`v1.1.0`](https://github.com/hectmor/phonia/releases/tag/v1.1.0) | 2026-10-23 | Additive, no breaking changes: the responsive TUI (#175), the dropped sound-server and debug-build fixes (#184, #186), dual MIT OR Apache-2.0 with third-party notices (#173), and the document split (#169) |

Earlier tags (`v0.2.0`–`v0.4.0`) were cut on `develop` before `main` was kept
current, and the workspace version was not raised with them; `v1.0.0` is the
first tag where the tag, `main` and `Cargo.toml` agree.

## Phases

| Phase | Milestone | Status |
|---|---|---|
| 0 — CLI prototype | [Fase 0](https://github.com/hectmor/phonia/milestone/1) | Done (#1–#7, #46) |
| 1 — Daemon and playback engine | [Fase 1](https://github.com/hectmor/phonia/milestone/2) | Done (#8–#16, #47, #52, #53) |
| 2 — TUI base | [Fase 2](https://github.com/hectmor/phonia/milestone/3) | Done (#17–#24) |
| 3 — Audio quality | [Fase 3](https://github.com/hectmor/phonia/milestone/4) | Done (#25–#31) |
| 4 — SONE-like features | [Fase 4](https://github.com/hectmor/phonia/milestone/5) | In progress (#196 part 2; #35, #36, #37 unscoped) |
| 5 — Extras and packaging | [Fase 5](https://github.com/hectmor/phonia/milestone/6) | In progress (CI, licensing, the process rules and #192 done; packaging open) |
| 6 — Layout and appearance | [Fase 6](https://github.com/hectmor/phonia/milestone/7) | Done (#124, #159, #160, #175, #188, #198, #207) |
| 7 — Home screen and discovery | [Fase 7](https://github.com/hectmor/phonia/milestone/8) | Done (#139, #144, #169, #171, #194) |

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
| #19 | Search (tracks, albums, artists, playlists) | Closed (PRs #82–#87) |
| #20 | Album and artist views, opened from a search result | Closed |
| #21 | Library: favorite tracks/albums and the user's playlists | Closed (PRs #95–#98) |
| #24 | Covers in the terminal (`ratatui-image`) | Closed (PRs #99–#105, all 7 parts) |

The catalog (search, an album, an artist, the library) is served by the
**daemon**, not the TUI process: the TUI depends only on `phonia-ipc`, so it
builds and is tested without ALSA, and the daemon is the single owner of the
TIDAL session. See `docs/DECISIONS.md` for why.

True color is a deliberate, deferred option: the TUI uses only the terminal's
16 ANSI colors today, on purpose, even now that Phase 2 and its covers are
done — it is its own product decision, not a leftover.

### Phase 3 — Audio quality

| Issue | What | Status |
|---|---|---|
| #27 | Gapless playback | Closed (verified bit-exact against real TIDAL over `snd-aloop`) |
| #29 | Quality tiers, a floor, and automatic fallback | Closed (parts 1–3); an optional part 4 (AAC decode for the lossy tiers) is not started and not blocking |
| #25 | DAC capability detection | Closed (PRs #106–#109, all 4 parts) |
| #26 | Per-track sample rate switching | Closed, same plan and PRs as #25 |
| #28 | Signal path indicator in the TUI | Closed (PRs #110–#112, all 3 parts) |
| #30 | ReplayGain in shared mode | Closed (all 4 parts) |
| #31 | Hardware mixer volume | Closed (all 3 parts) |

### Phase 4 — SONE-like features

| Issue | What | Status |
|---|---|---|
| #120 | Play reporting: finished plays reach TIDAL's own Recently Played | Closed (verified live against a real account) |
| #32 | Synced lyrics, following playback in the TUI | Closed (all 3 parts) |
| #39 | Playlist folders: browse TIDAL's own folder tree in the TUI | Closed (all 3 parts) |
| #33 | Mixes, radio and autoplay of similar tracks | Closed (all 6 parts) |
| #34 | MPRIS integration: media keys and desktop widgets | Closed (all 5 parts) |
| #156 | `ga` on a queue entry opens its artist | Closed |
| #164 | `ga` on any track row: search results, track rows, inside an opened album | Closed |
| #166 | `gr` opens the album of the track under the cursor | Closed |
| #196 | Add/remove TIDAL favorites from the TUI (`fas`/`far`/`faa`/`fal`, `fds`/`fdr`/`fda`/`fdl`) | In progress (part 1 of 2) |
| #35 | Scrobbling to Last.fm and ListenBrainz | Open, not scoped |
| #36 | Discord Rich Presence | Open, not scoped |
| #37 | Persistent queue and state across restarts | Open, not scoped |
| #38 | Configurable themes in TOML | Closed (`theme.toml`, `palette = "gruvbox"`, `"dracula"` or `"nord"`) |

#156, #164, #166 and #196 were unplanned: they came out of using the TUI day
to day, not from the phase's own plan. The three issues still open (#35, #36,
#37) hold a one-line description each and are to be scoped with Opus when
their turn comes.

### Phase 5 — Extras and packaging

| Issue | What | Status |
|---|---|---|
| #42 | CI with GitHub Actions (build, test, clippy, fmt) | Closed |
| #50 | Format the whole tree with rustfmt and check it in CI | Closed |
| #173 | Licensing: dual MIT OR Apache-2.0, third-party notices shipped and guarded in CI | Closed |
| #176 | Document the commit, branch and pull request rules | Closed (`docs/COMMITS.md`) |
| #178 | Require the phonia Project board on every pull request | Closed |
| #184 | A dropped sound-server connection is cached forever, and shared output never recovers | Closed |
| #186 | A debug build spends ~40x the CPU playing, and the docs tell you to use one | Closed |
| #192 | `phonia tui` starts and stops its own `phoniad` | Closed |
| #43 | RPM / COPR package for Fedora | Open, not scoped |
| #44 | AUR package | Open, not scoped |
| #45 | User documentation | Open, not scoped |
| #40 | MCP server for AI agents | Open, not scoped |
| #41 | Parametric equalizer in shared mode | Open, not scoped |
| #157 | Automatic output detection: the DAC if present, speakers or Bluetooth if not | Done |

### Phase 6 — Layout and appearance

| Issue | What | Status |
|---|---|---|
| #124 | Layout and appearance modifying for better user experience | Closed (both parts) |
| #159 | Highlight the track playing in search, and the album it belongs to | Closed (PR #161) |
| #160 | `Enter` inside an opened album queues it at the end of the queue | Closed |
| #175 | Making the TUI responsive: the cover and the whole interface fit the terminal | Closed (all four parts) |
| #188 | Give the cover room to breathe: columns beside it, a floor under it | Closed (two columns, one row) |
| #198 | Scrolling lists jump with every cursor move instead of staying put until the edge | Closed |
| #207 | Show artist and album in every track list, not just title and duration | Done (both parts) |

Started right after #120. The owner's concrete ask, from a screenshot of
TIDAL's own web player: a visually bigger, bolder track title with the artist
on its own quieter line underneath, instead of a single `Artist - Title`
string. #159 and #160 came out of daily use rather than the phase's plan, and
#175 reopened the phase: its first part fixed a cover that was invisible at
twelve terminal heights, and the other three made the cover grow with the
terminal, fit the bar's hints and tab rows to the room they have, and reflow a
narrow terminal. #188 and #198 came out of that same daily use, after it.

### Phase 7 — Home screen and discovery

| Issue | What | Status |
|---|---|---|
| #139 | Home screen in the TUI (local data only) | Closed (all 4 parts) |
| #144 | Favorite artists and Recently played on Home | Closed (all 6 parts) |
| #169 | Split the documentation; make the README an entry point | Closed |
| #171 | Re-record the screenshots: they showed a phonia without Home or Lyrics | Closed |
| #194 | `ga`/`gr` reach Home's recently played rows and Library's favorite albums | Closed |

A lightweight home screen using only data phonia already fetches (resuming,
favorites, playlist folders, recently played). A real TIDAL-style editorial or
personalized home — mixes, new releases, "albums you'll enjoy" — is a
deliberately separate, not-yet-investigated future issue, the same territory
#33 left "My Mixes" in.

## Right now

`v1.1.0` is out. Phases 0–3, 6 and 7 are closed again (#207 done); phase 4 is
code complete except for work nobody has scoped yet. #157's own follow-up
bugs surfaced a new one, #206, still open.

What is actually left, in no committed order:

- **Phase 6** — #207 (artist and album in every track list) is done, both
  parts: an opened playlist, radio and an artist's Top Tracks tab (part 1),
  and the queue and Home's recently played, which also needed the album
  title to actually reach them over protocol 1.20 first (part 2). #206 (a
  second USB DAC disconnect fails the track instead of pausing, a gap
  #157/#204/#205 left open) has a full approved plan (4 fixes in
  `alsa.rs`/`device.rs`/`audio_thread.rs`), not started.
- **Phase 4** — part 2 of #196 is the one thing in flight: adding and removing
  favorite tracks, albums and artists works, while following and unfollowing a
  playlist (`fal`/`fdl`) answers with an error until TIDAL's own endpoint for
  it is verified. Behind it, three issues nobody has scoped: scrobbling (#35),
  Discord Rich Presence (#36), and a queue and state that survive a restart
  (#37). Each is a one-line description today; each needs planning before any
  code.
- **Phase 5, packaging and extras** — the two package issues (#43 RPM/COPR,
  #44 AUR) are what would let someone install phonia without a Rust
  toolchain, and user documentation (#45) is now partly answered by the
  document split this file's own #169 entry describes. #157 (pick the output
  automatically: the DAC when it is plugged in, speakers or Bluetooth when it
  is not) is done.
- **Nothing beyond #206 is planned in detail.** No other issue above has an
  approved plan, so the next step for any of them is a planning pass, not an
  implementation.

The historical narrative that used to live in this section — what each merged
issue decided and why, part by part — was removed in #169: every one of those
decisions already has its own dated entry in
[`docs/DECISIONS.md`](docs/DECISIONS.md), which is where that record belongs.

## Conventions this file assumes

- Issues are closed by the project owner by hand after their pull requests
  merge, not automatically (pull requests target `develop`, which GitHub does
  not auto-close issues against). A "Closed" row above with the issue still
  open on GitHub is not a bug in this file; it means the owner has not closed
  it yet.
- Every pull request should carry the milestone of the issue it is part of,
  its assignee, and an `enhancement` (or `bug`/`documentation`) label, so
  GitHub's own filtering stays a second, independent source of truth.
- The milestone titles on GitHub are still in Spanish ("Fase 4 — Funciones
  extras"), as are the titles of the issues opened before 2026-10-19. Only the
  titles as quoted in this file are translated; everything written from
  2026-10-19 on — issues, pull requests, commits, tags and files — is in
  English.
