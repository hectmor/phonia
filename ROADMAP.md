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
| 2 — TUI base | [Fase 2](https://github.com/hectmor/phonia/milestone/3) | Done (#17–#24, tagged `v0.2.0`) |
| 3 — Audio quality | [Fase 3](https://github.com/hectmor/phonia/milestone/4) | Done (#25–#31) |
| 4 — SONE-like features | [Fase 4](https://github.com/hectmor/phonia/milestone/5) | In progress (see below) |
| 5 — Extras and packaging | [Fase 5](https://github.com/hectmor/phonia/milestone/6) | Partly started |
| 6 — Layout and appearance | [Fase 6](https://github.com/hectmor/phonia/milestone/7) | In progress (see below) |
| 7 — Home screen and discovery | [Fase 7](https://github.com/hectmor/phonia/milestone/8) | Code complete (see below) |

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
| #25 | DAC capability detection | Closed (PRs #106–#109, all 4 parts) |
| #26 | Per-track sample rate switching | Closed, same plan and PRs as #25 |
| #28 | Signal path indicator in the TUI | Closed (PRs #110–#112, all 3 parts) |
| #30 | ReplayGain in shared mode | Closed (all 4 parts) |
| #31 | Hardware mixer volume | Closed (all 3 parts) |

### Phase 4 — SONE-like features

| Issue | What | Status |
|---|---|---|
| #120 | Play reporting: finished plays reach TIDAL's own Recently Played | Closed (verified live against a real account) |
| #32 | Letras sincronizadas: lyrics synced to playback in the TUI | Code complete, all 3 parts merged |
| #39 | Carpetas de playlists: browse TIDAL's own playlist folders in the TUI | Code complete, all 3 parts merged |
| #33 | Mixes, radio y autoplay de pistas similares | Code complete, all 6 parts merged |

The rest of Phase 4 and all of Phase 5 are not started, except CI (#42) and
rustfmt-in-CI (#50), both closed. Nothing else is planned in detail yet;
issues #34–#45 hold one-line descriptions each, to be scoped with Opus when
their turn comes.

### Phase 6 — Layout and appearance

| Issue | What | Status |
|---|---|---|
| #124 | Layout and appearance modifying for better user experience | Closed (both parts merged) |

Brand new milestone, started right after #120. The owner's concrete ask
(from a screenshot of TIDAL's own web player): a visually bigger, bolder
track title with the artist on its own quieter line underneath, instead
of today's single `Artist - Title` string — see `docs/DECISIONS.md` for
the full design discussion and the decisions made.

### Phase 7 — Home screen and discovery

| Issue | What | Status |
|---|---|---|
| #139 | Home screen in the TUI (local data only) | Code complete (see below) |
| #144 | Favorite artists and Recently played on Home | Code complete (see below) |

Brand new milestone, started right after #33. A lightweight home
screen using only data phonia already fetches (resuming, favorites,
playlist folders) — a real TIDAL-style editorial/personalized home
(mixes, new releases) is a deliberately separate, not-yet-investigated
future issue, the same territory #33 left "My Mixes" in. #144 adds two
more blocks to that same lightweight home: favorite artists (a straight
copy of the existing favorites pattern) and recently played (a real new
mechanism, since nothing today stores or exposes local play history).
"Albums you'll enjoy" (TIDAL's own editorial/recommendation feed)
stays out of #144 too, for the same reason it stays out of #139.

## Right now

Phase 2 is fully closed (#17–#24, tagged `v0.2.0`). **#25 (DAC capability
detection) and #26 (per-track sample rate switching) are also done**,
planned together with Opus since they are two sides of one mechanism, as
an approved 4-PR plan, all merged (#106–#109).

What the investigation behind that plan found: the "reopen the PCM when a
track's format changes" mechanics #26 asks for **already existed** at the
engine level (`start_track`/`open_sink`/`join_next` in
`engine/audio_thread.rs`), tested against a fake sink. The real gap was
#25: `AlsaSink::open` used to pick a format by blindly trying a hardcoded
priority list against the device and ask for the track's exact rate on
faith, so an unsupported one surfaced as a raw ALSA error instead of a
clear refusal; and a refused track's failure was never even reported as
`TrackEnded` to clients, a second bug found along the way and fixed in
part 3.

Approved design: live `HwParams` probing (on the PCM already being
opened) decides everything, not parsing `/proc/asound/cardN/stream0` as
the issue literally suggests — `stream0` is USB-only, pre-quirks, and
blind to live device state (another substream holding the rate, a
replugged DAC), so it stayed a passive, display-only addition to `phonia
devices` (part 4) instead. No capability cache: probing is cheap
(microseconds) and a cache would go stale in exactly the cases live
probing handles for free. The policy stays **bit-perfect or refuse** in
exclusive mode — shared/PipeWire remains the one deliberate,
clearly-labelled non-bit-perfect exception, untouched by this work — what
improved is the refusal *message* (precise: what was asked, what the
device actually offers), not the policy. See `docs/DECISIONS.md` for the
full reasoning and the plan's own open decisions as the person settled
them.

Part 1 added `output/caps.rs` (pure, unit-tested capability types and
probing) and fixed `phonia probe-device` (it used to report "yes" to
everything on a `plughw:`/`default` device, since it never disabled
automatic resampling). Part 2 added `caps::choose`, which picks the
tightest lossless container a device actually offers at a track's exact
rate (`AlsaSink::open` now probes before committing anything and refuses
precisely instead of surfacing a raw ALSA error; the old hardcoded
`pick_format` is gone). Part 3 fixed `start_track` to emit the refused
track's own `TrackEnded { Failed }` before the error reaches `fail()` —
previously, a track refused with nothing playing before it left no trace
at all beyond a bare `Event::Error`, since neither `self.current` nor
`self.outgoing` ever held it. Part 4 made `phonia devices` show, for a
USB card, an `advertises ...` line parsed from `stream0` — what the
device *claims* in its USB descriptors, before any kernel quirk or real
negotiation; purely informational, and the one piece that can be read
without taking the card from PipeWire at all.

Verified against the real Fosi Audio DS2 throughout: it accepts every
TIDAL rate in `S16_LE`, `S24_3LE` and `S32_LE` (not `S24_LE`) — confirmed
via `probe-device`, the real `stream0` text (captured as part 4's own test
fixture), and a dedicated `#[ignore]`d test, each run by hand with the DAC
freed from PipeWire first. Because this DAC accepts everything TIDAL can
send it, the *refusal* path (a rate or format a device can't do) cannot be
exercised against it and is tested with fakes instead.

Phase 3 continues with **#28 (signal path indicator in the TUI)**, planned
with Opus as an approved 3-PR plan. The investigation behind it found that
the wire path already existed end to end — `phonia-core`'s `SinkReport`
already reached `phoniad`, which already broadcast it as
`Event::SinkReport` to every client, `phonia ctl`'s event log included —
the TUI just silently dropped it. The only real gap: `Status` carried no
*persistent* verdict, so a client that connects, reconnects or resyncs
mid-album (gapless tracks of the same format share one `SinkReport`, sent
once per sink *open*, not once per track) saw nothing until the next
format change. Part 1 (merged) closes that gap: protocol 1.7 adds
`SinkReport.output` (the route id of the output that produced it, stamped
where it is known for certain — the per-output factory closure in
`phoniad/main.rs` — since the report can otherwise race ahead of
`Event::OutputChanged` on a switch) and `Status.sink_report`, gated by a
new `SinkReport::applies_to(status)` (same format, and same output when
both sides know it) so a stale verdict is never shown as current. The
verdict's own text (`BIT-PERFECT`/`CONVERTED (reason)`/`SHARED ...`) moved
out of `phonia ctl` into a shared `phonia_ipc::fmt::verdict`, reused by
part 2 instead of a third reimplementation; `phonia ctl status` already
showed it as a `Verdict:` line from part 1 alone.

Part 2 (merged) is the indicator itself: the TUI's bottom bar gains a
dedicated, always-reserved fourth line — `TIDAL hires 24-bit / 96 kHz →
S24_3LE → Fosi Audio DS2 (hw:1,0)  ✔ BIT-PERFECT` — present and blank in
every connection state (even disconnected or still connecting), so the
bar's height, and the main panel's own row count, never depend on what is
playing. The first line drops the `(24-bit / 96 kHz, hires)` it used to
show, since that moved to the new line next to the device it actually
reached. A width-aware `fit()` keeps the verdict whole and truncates the
path first, only cutting the verdict itself if it alone would not fit. A
new `theme.warn` (yellow; no modifier with `NO_COLOR`, since the SHARED
wording already says it is not an error) colours a shared-mode "not
bit-perfect" apart from a real `CONVERTED`/refusal, which stays
`theme.error`. Verified live against the real Fosi Audio DS2 over a
PipeWire null sink with a real TIDAL track: the line rendered correctly
end to end (`92 kHz → S32_LE 48 kHz → phonia_test Audio/Sin…  ✖ SHARED
(not bit-perfect, resampled to 48000 Hz)`), confirmed from the captured
pty bytes rather than a clean quit — this terminal harness has a known,
pre-existing quirk (confirmed against `develop` itself, not a regression)
where a `q` keypress sent through a forked pty is never seen to exit the
process within the harness, even though the key handling itself is
unit-tested and unrelated to this change.

Part 3 (merged) closes #28 out: the same signal-path line shows a
refused or failed track's own reason (`✖ hw:1,0 cannot play 352800 Hz
natively; ...`, #25's own precise `caps::Unsupported` text) instead of
the bare "Stopped" the TUI used to show. A new, TUI-only
`State.playback_error` (not on the wire) is set by the daemon's
`Event::Error` and cleared once something newer replaces it — a track
actually starting, or a fresh `SinkReport` — deliberately *not* reusing
`last_error` (that field means "a request *you* sent just failed," and
clears on the next key; this is an async failure from the daemon itself,
with nothing to do with a key the person pressed). It takes priority over
whatever `status` itself says, since `TrackEnded` has usually already
cleared the track the error was about by the time it arrives.

Deliberately **not** done anywhere in #28, per the plan: #25's probed
`Capabilities` are not attached to `SinkReport` (the issue only asks for
source/format/device plus a verdict, which it already has in full; that
seam stays open for a future, separate "what can my DAC do" view) and no
structured "unsupported format" error code was added (the human text is
enough for display; a client that would act on its own belongs to a
future "automatic output selection" issue instead).

**#30 (ReplayGain in shared mode)** is next, planned with Opus as an
approved 4-PR plan. The investigation found TIDAL already sends
everything needed — `playbackinfopostpaywall` (fetched on every track
open already) carries `trackReplayGain`/`trackPeakAmplitude`/
`albumReplayGain`/`albumPeakAmplitude`; phonia's own `RawPlaybackInfo`
just didn't declare those fields. The central design question — where to
apply the gain — was settled against the investigation's own leaning
(folding it into shared mode's existing PipeWire volume call): shared
mode's ring buffer is roughly 0.65s deep, so a volume command at a track
boundary would land on the wrong audio during a gapless join. Instead,
gain is scaled in process, confined entirely to `SharedSink`, via a new
`AudioSink::set_gain` trait method that defaults to a no-op — exclusive
mode gets no new code at all, so bit-perfect-or-refuse stays true by
construction. Track vs. album gain follows play order (album when
shuffle is off and the adjacent track shares an album); clip protection
via the reported peak is always on; the config defaults to `off`
(`[playback] replaygain = off|track|album|auto`, file-only); the gain is
shown only when actually applied (shared mode), never in exclusive mode
or #28's own signal-path line. See `docs/DECISIONS.md` for the full
reasoning.

Part 1 (merged) adds the four loudness fields to `tidal.rs`'s
`PlaybackInfo`, a new `replaygain.rs` module holding the pure `Loudness`
type, and `TrackMeta.loudness`, filled by `TidalOpener::open` from the
playback info already being fetched — no behavior change. Verified
against a real TIDAL track at both LOSSLESS and HI_RES_LOSSLESS tiers.

Part 2 (merged) adds the decision logic, still not wired to any audio:
`replaygain.rs` gains `Mode` (the `[playback] replaygain` config key,
default `off`), `Kind` (which of the two gains was actually used) and
`choose(loudness, mode, same_album_neighbor)`, which picks track or album
gain and applies the peak-based clip cap (a boost is capped at
`-20·log10(peak)`, never clipping the track's own reported true peak; a
cut is never touched; a missing peak caps a boost at 0 dB). `QueueTrack`
and `SourceInfo` gain `album_id` (mirroring `cover`'s own precedent from
#21/#24), and `Inner::album_context(id)` decides whether an entry sits
next to another of the same album in play order — always `false` while
shuffled, whatever the shuffled order happens to put next to what.
`Queue::set_replay_gain(mode)` is called once at daemon startup from
config; there is no runtime IPC setter, on purpose (#30's own scope
excludes one). `Queue::applied_gain(id, loudness)` is what the engine
calls (from `Queue::open_entry`, once per open, alongside the existing
`cover`/`record_meta` fill-ins) to decide `TrackMeta.gain`.

Part 3 (merged) is the first audible change. A new `AudioSink::set_gain`
defaults to doing nothing, so `alsa::AlsaSink` needs no changes at all —
exclusive mode stays bit-perfect by construction, not by a flag someone
has to remember. Only `shared::SharedSink` overrides it, scaling every
sample in `write` by the gain in force. `engine::audio_thread::play_step`
calls `sink.set_gain` before every write, from `TrackMeta.gain_linear()`
of whichever track that write's samples actually belong to — normally
the one playing, except right after a device reopen interrupts a gapless
crossing (an output switch, a release): `set_aside_unheard` can then hand
back a buffer that mixes the tail of the outgoing track with the head of
the next one, so `play_step` caps that write at the `lead_in` boundary
and picks the outgoing track's gain for the part before it. A new
`a_boost_saturates_instead_of_wrapping`-style `scale_samples` helper
(`output/mod.rs`) rounds and clamps so a boost near full scale saturates
instead of wrapping; `FakeSink` gained the same scaling (recording what
gain was in effect, like a real `SharedSink` would) so the engine's own
gain-selection logic is unit-tested without a real sound server. Verified
for real against PipeWire (a null sink, `parec`): a known ramp halved
exactly by `set_gain(0.5)`, and a gain change landing on the exact frame
boundary between an unscaled and a scaled half.

Part 4 (merged, last) is wire protocol 1.8: `dto::Track` and
`Event::TrackStarted` both gain `replay_gain: Option<ReplayGain>`
(`{ kind, millibels }`, an integer since every IPC type derives `Eq` and
floats don't), filled by a new `convert::replay_gain` from
`TrackMeta.gain`. A new `phonia_ipc::fmt::replay_gain(track, route)`
decides whether to show anything at all: `None` unless the track has a
gain *and* `route.mode` is `Shared` — a gain is decided the same way
whatever the output, but only actually applied in shared mode, so
showing it for an exclusive-mode track would claim an effect that never
happened. `phonia ctl status` gained a `Gain:` line next to `Quality:`;
the TUI's bar shows the same text next to the volume. Nothing was added
to #28's own signal-path line, on purpose — that line is about the
format and the device, not about loudness.

This closes #30: all 4 parts of the approved plan are merged.

**#31 (hardware mixer volume)** is next, planned with Opus as an
approved 3-PR plan. Exclusive mode has had no volume at all until now —
the DAC's own hardware mixer applies, phonia never scales the audio.
#31 drives that hardware control programmatically when a card has one,
through ALSA's Selem (simple mixer) API, which the `alsa` crate already
wraps safely (no new dependency); a card with none stays locked at
100%, exactly as today. This is a completely different mechanism from
#30's `AudioSink::set_gain` (an in-process sample scaler): the stream
itself is never touched, so exclusive mode stays fully bit-perfect.
Central design principle: **a hardware volume belongs to the card, not
to phonia** — it is read and set only when the user explicitly asks,
never seeded, restored, or carried into a control on startup or an
output switch. Real checks run during planning found the development
machine's own Fosi Audio DS2 does have a usable control (a `PCM` Selem,
-63..0 dB in exact 1 dB steps, currently driven by WirePlumber for
shared mode), and that `phoniad`'s `Outputs` had two real hazards this
work needed to fix: it seeded a fresh output at 100% instead of reading
the hardware's actual level (risking a loud jump on the first relative
volume change), and it carried a level into *any* newly attached
output, which would double-attenuate a card already driven by
PipeWire. See `docs/DECISIONS.md` for the full plan and reasoning.

Part 1 (merged) adds `output/mixer.rs`: pure, unit-tested functions
(percent↔dB curve — the same cubic-in-amplitude one shared mode's own
percent already implies, so a number means the same loudness change
wherever it came from — and which control to prefer when a card
offers several) plus `HardwareVolume`, an ALSA Selem-backed
`VolumeControl` that re-resolves the card and re-opens the mixer on
every call (replug-safe, the same by-id precedent `AlsaSink` itself
follows for the PCM device; also sidesteps the `alsa` crate's `Mixer`
not being `Sync`). Not wired into `AlsaSinkFactory` yet — no behavior
change in this part. Verified for real against three physical cards:
the DS2's `PCM` control (read, round-tripped down to silence and back,
muted and unmuted, restored to its exact starting point afterward —
it was genuinely in use, at -10 dB, not a throwaway default), the
internal `sof-hda-dsp` card's `Master` control (a second real "has a
control" case), and an NVidia HDMI output (confirmed to correctly
report no usable control at all, read-only, nothing audible).

Part 2 (merged) is the first audible change: `AlsaSinkFactory::volume()`
now returns a working `HardwareVolume` whenever the card has one
(re-probed fresh on every call, never cached). `phoniad`'s `Outputs`
had its two hazards fixed, both by the same underlying change:
`Outputs::volume()` and the base `set_volume` computes a relative
change from are now always the control's own current value
(`control.get()`), never a value `Outputs` cached itself — this erases
the "seeded at 100%" bug outright, since there is no cache left to seed
badly. `switched_to` now carries the level forward only between two
shared outputs; attaching to anything else (any exclusive card, with a
hardware control or without) never writes a value into it at attach
time, no matter what `Outputs` last had asked for. Every stale
"exclusive has no volume" message, doc comment and help text across
`mod.rs`, `proto.rs`, `outputs.rs`, `daemon.rs`, `ctl.rs` and the TUI
was reworded to say "no hardware mixer control" instead, without
changing the wire format (the refusal stays `ErrorCode::Unsupported`,
`Status.volume` stays `None`).

Verified for real end to end against the DS2: started a scratch
daemon on it and watched `phonia ctl status` show its *actual* live
level (68%, matching -10 dB, not a seeded 100%) before any volume
command was ever sent; a relative `volume -5` landed at exactly 63% on
both phonia's own report and a separate `amixer` read; mute and unmute
each left the level untouched; switching to a shared output (the
built-in speaker, nothing DS2-related) started fresh at 100% rather
than carrying the DS2's 63% over; switching back to the DS2 showed
63% again, confirming the hardware was never touched during the
detour; and the card was set back to its exact original value
(84%/-10 dB/on) before the scratch daemon was stopped.

Part 3 (merged, last) adds the live watcher: a background thread,
started the first time anything calls `on_change`, that keeps its own
`Mixer` open (the only place in this module that does — `get`/`set`
still always open a fresh one) so it can block on `Mixer::wait` for
real events instead of polling blindly, and announces a change only
when the value actually differs from what it last reported. Found and
fixed a real bug while writing the test for this, not after: the first
version announced the control's starting value as if it were a
"change" the instant it attached, because the comparison began from
`None`; fixed by reading the starting value silently first, and only
comparing against it from the next tick on. Also adds a passive
`phonia devices` hint — `hardware volume: PCM (-63.0..0.0 dB)` — read
the same way the USB `advertises` line already is: without opening the
device, costing nothing whether or not anything is playing. Kept out
of `catalog::Entry`/the daemon's own output list on purpose: this is a
standalone-CLI, read-only probe, not something that needs a running
daemon or the wire protocol, the same boundary the `stream0` line
already draws.

Verified for real: the watcher test writes to the exact same DS2
control a second, independent way and confirms the handler fires with
the right value, restoring the card afterward; `phonia devices` was
run live and correctly showed `PCM (-63.0..0.0 dB)` under the DS2 and
`Master (-65.2..0.0 dB)` under the internal `sof-hda-dsp` card, with
no line at all under the NVidia HDMI card. The real hardware tests
must be run one at a time (`--test-threads=1`) when run together: two
of them writing to the same physical control at once look to each
other exactly like an outside change, which is itself a sign the
watcher's detection is working as meant.

This closes #31: all 3 parts of the approved plan are merged.

Phase 3 is now fully closed: #25 and #26 were the last two issues left
open on GitHub after being code-complete for a while, closed by hand
on 2026-10-04 alongside this update.

**Phase 4 starts with #120 (play reporting)**, planned with Opus. Its one-line
body ("Reports finished plays to TIDAL so Recently Played reflects what you
listen to in SONE") turned out to name a *different* open-source Linux TIDAL
client (`lullabyX/sone`), not phonia itself — its README is where that exact
wording came from, and reading its play-reporting module during planning
(GPL-3; read only for the protocol facts it had already live-verified, no
code copied) corrected and filled in several gaps a first reading of TIDAL's
own open-source SDKs (`tidal-sdk-web`, `tidal-sdk-android`) had left open.
There is no documented "mark this played" endpoint: official apps send a
`playback_session` event (group `play_log`) through TIDAL's internal event
pipeline, `https://ec.tidal.com/api/event-batch` — undocumented, and TIDAL
could change it without notice, but a failure here is always silent and
never touches playback. See `docs/DECISIONS.md` for the full reasoning,
including why the event must use the *mobile/Android* body shape rather than
the plainer one a literal web-SDK reading would suggest (phonia's own TIDAL
login is a native, not a browser, client), why `sourceType`/`sourceId` are
not optional (a sourceless play is accepted but never shows up in Recently
Played), and the full list of what is still `PROVISIONAL (#120)`.

Decided with the user: implement it for real (accepting the undocumented-
contract risk); `[tidal] report_plays`, file-only, **on by default** (the
one place the user went against the recommended off-by-default); a 30-second
flat "actually heard" threshold, TIDAL's own rule, not a guess; fire-and-
forget with one in-memory retry, no disk-persisted outbox; ship reporting
every play as `ITEM` + the track's own id for now, real album/playlist
attribution left for a later issue.

Part 1 (merged) adds `phonia-core`'s `play_log.rs`: the event's
body/headers/SQS-batch-form encoding as pure, unit-tested functions,
`PlayLog::send` (through the one TIDAL session the process already owns,
`TidalOpener::play_log()`), and `SessionTracker`, a pure, clock-injected
state machine that turns playback events into a finished `PlaybackSession`
once 30 real seconds have been heard. `[tidal] report_plays` exists (default
`true`) but nothing reads it outside a test yet — no engine, daemon or wire
protocol changes in this part.

Part 2 (merged, last) wires it into `phoniad`: `Daemon::note_play_log` feeds
every relevant engine event (`TrackStarted`, `Position`, `Seeked`,
`StateChanged(Paused|Playing)`, `TrackEnded`) to the tracker from inside
`fan_in`, resolving the real TIDAL id and delivered quality from the queue
snapshot already fetched there (`resolve_tidal_track`, new) — doing this at
`TrackStarted` time, not later, matters for a gapless join, where the queue
can advance before the outgoing track's own `TrackEnded` is even converted.
A finished session is sent in the background (one in-memory retry after
30s, then given up, both logged only with `verbose`) so reporting never
blocks playback or the event fan-out. `main.rs` only builds a `PlayLog` at
all when `report_plays` is on. No wire protocol change: this stays entirely
a `phoniad`-side effect, same as planned.

**Verified live against a real account**, with a scratch daemon on a
PipeWire null sink (`report_plays` at its default, on): a track played past
the 30-second threshold and then skipped was accepted by TIDAL's event
endpoint and showed up in that account's real Recently Played shortly
after, for two different tracks; a third, skipped after only ~10 seconds,
correctly produced no event at all. This resolves the open PROVISIONAL
question of whether a bare `playback_session` is enough on its own — it
is, with no need for a correlated `x-tidal-streamingsessionid` header or
the separate `streaming_metrics` events (the PR3-conditional fallback the
plan had set aside never had to be built). The pinned client identity in
`play_log.rs` stays marked `PROVISIONAL (#120)` regardless: it worked today,
but nothing stops TIDAL from tightening what it accepts later.

This closes #120: both parts of the approved plan are merged.

**#124 (layout and appearance) is next**, planned with Opus from a
screenshot of TIDAL's own web player. Decided with the user (recommended
options throughout): bold-weight hierarchy for now, not a literal big-text
widget (`tui-big-text`'s `font8x8` glyphs don't cover enough of TIDAL's
real catalog — no Cyrillic/CJK beyond hiragana, no curly quotes/em dashes
without normalizing, and at 4 columns per character most real titles
wouldn't fit anyway); the 16-ANSI-color deferral stays in place, untouched;
the bigger title lives in the header beside the cover (reserved even with
no picker/cover), never in the fixed-height bottom bar, so `BAR_HEIGHT`
and #28's own invariant are untouched; a real `artist` field is added to
the protocol as its own prerequisite PR, not folded into the visual work.

Part 1 (merged) is that prerequisite: protocol 1.8 → 1.9. `artist:
Option<String>` is new on `Track`, `QueueItem` and `TrackStarted`
(`#[serde(default)]`, so an older 1.8 client still works — it just shows
the title alone); `title` itself no longer includes the artist.
`TidalOpener::describe()` (`openers.rs`) keeps them separate from TIDAL's
own response instead of joining them immediately, threaded through
`SourceInfo`/`QueueTrack`/`TrackMeta` the same way `cover`/`album_id`
already are. **No visible change in this part**: a new
`phonia_ipc::fmt::track_name(title, artist, source)` helper recombines
them as `Artist - Title` everywhere that used to show the joined string
(`phonia ctl status`/`queue`/`watch`, the no-daemon player, and the TUI's
header/bar/queue list), so the display reads exactly as before until
part 2 changes it on purpose.

Part 2 (merged, last) is the visual work itself: `now_playing_header` now
shows the title alone (bold, `theme.accent`, already the project's
existing bold-and-colored style -- no new one needed), the artist on its
own quieter line beneath it (`theme.dim`, prefixed `◉ `), and the quality
tier, each line shown only when known. That header is now reserved by
screen size alone whenever a track is playing, cover or no cover (a new
`HEADER_TEXT_ROWS = 4` fixed-height path alongside the existing
cover-sized one) -- the same "reserved on content existing, never on
content having arrived" discipline #24's own cover reservation already
established, just extended to "no cover at all." `BAR_HEIGHT` and #28's
own fixed-bar-height invariant are untouched: the bar's own
`connected_line()` still shows one compact `Artist - Title` line via
part 1's `fmt::track_name`, unchanged. No protocol change, no new
dependency, 16-color theme untouched -- all as decided.

This closes #124: both parts of the approved plan are merged. Verified
live against the real daemon and a real TIDAL track over a pty capture.

**#32 (synced lyrics) is next**, planned with Opus. TIDAL's `GET
/tracks/{id}/lyrics` (same host as `playbackinfopostpaywall`) answers
`lyrics` (plain text) and `subtitles` (LRC-format, time-synced) — `tidlers`
wraps this endpoint (`TidalClient::get_track_lyrics`) but its
`LyricsResponse` type has no field for `subtitles` at all, so it is
silently dropped by serde; phonia reads the response raw instead, the
same reason `catalog/remote.rs` already avoids `tidlers`' listing calls
for `search`/`album`/`playlist`. Decided with the user (recommended
throughout): **pull**, not push — a client asks for lyrics
(`Request::Lyrics`) only when it actually opens the Lyrics panel, rather
than the daemon fetching them for every track whether or not anyone
looks; plain-text fallback when there's no synced version, a clear "no
lyrics" message when there's none at all, nothing attempted for local
files (no TIDAL id to look up); a fourth TUI section,
`Section::Lyrics`, following the current line automatically when synced,
scrollable like plain text otherwise. See `docs/DECISIONS.md` for the
full reasoning and the rest of the decisions (caching, protocol shape,
right-to-left text).

Part 1 (merged) is `phonia-core` only, no protocol change: a new
`Catalog::track_lyrics` method, a raw-HTTP implementation in
`catalog/remote.rs` (`RawLyrics`, `lyrics_from`, mirroring the existing
`artist_bio`/`bio_from` 404-means-`None` pattern), and a new, pure
`catalog/lrc.rs` module parsing LRC text into time-ordered lines
(multiple timestamp tags per line, `[offset:±ms]`, metadata tags
ignored, malformed lines skipped without losing the rest). **Verified
live against a real account**: a real track's `subtitles` parsed into 39
correctly time-ordered lines, provider `MUSIXMATCH`, the first line's
text and timestamp matching the real song; a genuinely instrumental
track correctly answered `None`.

Part 2 (merged) is the wire protocol: protocol 1.9 → 1.10, a new
`CAP_LYRICS` capability (announced alongside `CAP_CATALOG`), `Request::Lyrics
{ id }` answered with `Payload::Lyrics { id, lyrics }` (`id` repeated so a
client can discard a stale answer once the track has moved on), and new
`Lyrics`/`LyricLine` DTOs (`at_ms` rather than a `Duration`, so the wire
stays plain JSON). `phoniad` caches `Some`/`None` answers in memory (a
small FIFO-bounded cache, lost on restart) but never a failure, so a
transient TIDAL error can be retried instead of sticking; the request runs
beside a connection's others (like `search`/`album`), since TIDAL may be
slow to answer. A new `phonia ctl lyrics [<id>]` defaults to the track
playing now. **Verified live** against the real daemon and the real
account: `ctl lyrics 233059491` printed all 39 lines timestamped
`[m:ss]`, credited `MUSIXMATCH`; the instrumental id answered "TIDAL has
no lyrics for this track"; asking with no id while Sultans of Swing was
playing resolved to the same track automatically.

Part 3 (merged, last) is the TUI panel: a fourth sidebar section,
`Section::Lyrics` (key `4`), asking the daemon the moment it is opened or
the track changes while it is (pull, never pushed for a track nobody is
looking at), following the current line automatically as it plays
(`current_line`/`centered_first` in the new `lyrics.rs`, the same
`theme.accent`-marked-row styling `queue_lines` already established for
"the current one") -- overridable by scrolling manually (`j`/`k` and the
rest), which then stays put until the track changes, since the user asked
for that rather than synced lyrics being auto-follow-only. Plain text (no
sync) shows a notice and scrolls the same way; no lyrics at all, a local
file, a missing capability, and a failed fetch (retried by leaving the
panel and coming back) each say so plainly. Right-to-left text is
right-aligned, no bidi shaping. A stale answer (the track moved on before
it arrived) is dropped through the same `Tag`/generation pattern the
rest of the TUI's panels already use, not a new mechanism. **Verified
live against the real daemon, the real account and a track actually
playing** (as a second, read-only client of a daemon already in use, so
as not to disturb it): Watain's "De Profundis" showed its real synced
lyrics, auto-centered
on the line being sung, credited "Lyrics via MUSIXMATCH" -- this live
check caught a real bug (scrolling when there was nothing yet to scroll
silently turned off auto-follow the moment the terminal was resized
smaller later), fixed before merging.

This closes #32: all three parts are merged.

Phase 4 continues with **#39 (playlist folders)**, planned with Opus as a
3-part plan. Nothing in the codebase or in `tidlers` (the TIDAL client
library) wraps TIDAL's real "My Collection" folder API, so the plan's own
investigation found it directly: `GET /my-collection/playlists/folders`
on TIDAL's v2 host (`folderId=root` or a folder's id, `order`,
`orderDirection`, paged by `offset`/`limit`, but — unlike every v1 listing
phonia already uses — the answer never echoes an offset back, so the
caller's own requested offset is kept instead).

Three decisions, all taken as recommended: this stays read-only for now
(creating/renaming/moving a folder is a separate future issue, the same
scoping #21 used for favorite-editing); a folder's contents show a
playlist the user only *follows*, exactly like TIDAL's own app, not
filtered down to owned-only the way the flat "Your playlists" list is;
the existing "Your playlists" library tab becomes the root of the folder
tree rather than a new tab; folders sort before playlists, both by name.

Part 1 added `catalog::FolderEntry` and `Catalog::playlist_folder` to
`phonia-core`, verified live against a real test folder on this account
(one playlist followed, not owned, confirming why this view does not
apply `my_playlists`' owned-only filter). Part 2 is the wire protocol
(`Request`/`Payload::PlaylistFolder`, 1.10 → 1.11, additive, no new
capability) and the daemon handler, plus `phonia ctl folder [<id>]`,
verified live end to end against the real account. Part 3 (last) is the
TUI: the library's own "Your playlists" tab is now the root of the
folder tree, nested through the same `browse::Stack` an album or artist
already nests through (a new `browse::View::Folder`) — opening a
sub-folder, and nesting further from there, falls out of that stack for
free. The root is fetched by its own request (the old flat
`Payload::Library.my_playlists` field stays on the wire for an older
client, just unread here), so the Playlists tab loads and can fail
independently of the other two. **This closes #39**, all 3 parts merged.

Phase 4 continues with **#33 (mixes, radio and autoplay of similar
tracks)**, planned with Opus as a 6-part plan, scoped down (after an
explicit decision) to on-demand track radio plus autoplay; personal
"My Mixes" are left for a future issue, since they need endpoints this
plan did not investigate.

Part 1 added `Catalog::track_radio` to `phonia-core`: TIDAL's
`/tracks/{id}/radio`, confirmed live to return bare tracks (reusing the
existing `parse_track_items`, which already tolerates that shape) with
the seed track itself always listed first — filtered out locally
(`parse_radio`), since nothing that asks for a track's radio wants that
same track back. The two-step `/tracks/{id}/mix` + `/mixes/{id}/items`
alternative was also confirmed live (first item was, as far as checked,
the same list `radio` already gives in one request) and was not used,
since one request beats two for what autoplay will call repeatedly.

Part 2 added the wire/daemon/`ctl` side: `CatalogRef::TrackRadio`
(protocol 1.11 → 1.12) needed no new request or payload type at all —
it reuses the existing `Request::Tracks`/`Request::QueueAddFrom`
machinery through the same `track_page` dispatcher every other list
already goes through. `phonia ctl radio <id>` and `queue add
radio:<id>` verified live against the real account. Part 3 added the
TUI's own on-demand action: `o` on a track (a search result, a favorite
track, or one inside an already-open album, playlist, artist page or
radio) opens its radio, reusing the existing `TrackListView`/`Stack`
machinery an album or a playlist already opens into — nesting a radio
from inside another falls out for free. Opening a queue entry's or the
now-playing track's own radio is a deliberate follow-up, not covered
yet: neither has anywhere to show an opened radio today.

Part 4 added the autoplay setting itself, no behavior yet: `[playback]
autoplay` (default off) supplies the startup value of a new,
runtime-changeable `Queue.autoplay` flag — modeled like
`shuffle`/`repeat` (a `Request::SetAutoplay`, protocol 1.12 → 1.13),
not like `replaygain` (config-only, no runtime setter), since the
approved design wants it toggleable while the daemon runs. `phonia ctl
autoplay [on|off]` (no argument flips it) verified live against the
real account.

Part 5 added the actual behavior, in `phoniad` only — the engine
itself needed no change at all. Reading the real code corrected part
1's own plan: the engine's existing gapless prefetch already re-checks
the queue on every tick and does nothing special when it finds nothing
next, so appending tracks while the last entry is still playing is
picked up on its own (confirmed by a new engine test). The daemon
notices on `TrackStarted`/`QueueChanged` that nothing follows the
current entry (`Queue::autoplay_due`, repeat off only), fetches that
entry's TIDAL radio (or the last TIDAL track's, if the newly-last entry
is a local file) without holding up other clients, then re-validates
and appends up to 10 tracks, skipping duplicates — all before
`Event::QueueExhausted` would otherwise release the DAC. If a very
short track or a slow fetch loses that race anyway, playback resumes
on the first added track rather than staying stopped. Verified live
against the real account: ~10 real tracks appeared within ~2 seconds
of a seed track starting; `repeat all`, `autoplay off`, and an
immediate Stop each correctly added nothing.

Part 6 (last) added the TUI's own `O` key, flipping autoplay on or
off (paired with part 3's lowercase `o`, which opens a track's radio
on demand) -- a thin addition reusing the exact same request/dispatch
shape shuffle (`s`) and repeat (`r`) already have, shown next to them
in the bar whenever it is on. **This closes #33**, all 6 parts merged:
on-demand track radio (browse, queue, and the TUI) plus autoplay (the
setting, its real behavior, and the TUI toggle). Personal "My Mixes"
remain a deliberately separate future issue.

Phase 7 starts with **#139 (a Home screen in the TUI)**, planned with
Opus as a 4-part plan: a new `Section::Home` holding no data of its
own (it reads `state.status`/`state.queue`/`state.library`, the same
state Queue and Library already hold), showing a "Continue" row plus
three 6-item blocks (favorite albums, playlist folders, favorite
tracks) each ending in a "See all" row into the matching Library tab.
Opening an album, a playlist or a folder from Home nests inside Home's
own stack, the same way Library already nests its own.

Part 1 (merged) is a real, pre-existing bug found while planning, not
tied to Home itself but in its path: Enter on a track inside an album,
a playlist, a folder or a radio opened from the *library* read
`search_views` unconditionally (a leftover from before the library got
its own stack) instead of whichever stack actually opened that view —
so it silently did nothing whenever no search view happened to be
open, or used the wrong list's data if one was. Fixed to read the
active stack generically, with regression tests opening from both an
album and a radio.

Part 2 adds the `Section::Home` shell itself: a new first variant of
`Section` (so it is both key `1` and, via `Cursor::default()`, the
startup section -- everything else shifts up one key, Queue/Search/
Library/Lyrics now `2`-`5`), showing only the "Continue" row for now.
`home::continuation` reads `state.status` and `state.queue` directly
rather than holding anything of its own: a track still in
`status.track` (playing, paused, loading or seeking) resumes exactly
where it is; a real Stop clears `status.track` but never the queue's
own `current`, so a stopped-but-current entry replays from the start
instead; a queue with items but no `current` yet starts it; an empty
queue has nothing to continue. No new field was needed on `State` for
this single row -- a per-row cursor is deferred to part 3, once there
is more than one row to move between.

Part 3 adds the three content blocks: favorite albums, playlist folders, favorite tracks, each
capped at 6 and closed with a "See all (N)" row into the matching Library tab. Visiting Home now
asks for the library at once (the same request Library itself makes), rather than waiting for
Library to be opened first, since Home reads the exact same data. `home::rows` builds the full
row list fresh each time (nothing cached), skipping a block entirely while it has not loaded yet
or once loaded it turns out to be empty -- two states that would otherwise need two different
"nothing here" messages for no real benefit. `home_cursor` (new on `State`) moves only between the
*selectable* rows (everything but a header or the spacer before one); Enter plays a favorite
track directly (the same one-track behavior Library's own Favorite Tracks tab already has) or
jumps into Library on a "See all" row -- opening an album, a playlist or a folder is still part
4's job, nested inside Home's own stack.

Part 4 (last) wires that nested opening, **closing #139**: a new `home_views: Stack` field, added
to `active_stack_mut`'s match alongside Search's and Library's own -- at which point essentially
all of the existing "browsing an opened view" machinery (`act_in_view`, `act_on_track_row`,
`browsed_row`, `move_cursor`'s browsing branch, `pop_view_if_browsing`, the artist-tab switch)
turned out to need no Home-specific code at all, since every one of them is already written
generically against whichever stack `active_stack_mut` returns. Enter on an album or a folder row
now opens it (an album via `open_track_list`, a folder/playlist via the existing
`act_on_folder_entry`, unchanged); `a`/`A` add without opening; `o` opens a favorite track's radio.
A real bug was caught while testing this, the same shape as #140: `find_view` (which a `Request::
Tracks`/`Request::PlaylistFolder` answer uses to find the view it belongs to by serial) checked
only `search_views` and `library_views`, so an answer to a view opened from Home was silently
dropped, leaving it stuck on "Loading" forever. Fixed by trying `home_views` too, same as #140's
fix taught: a hardcoded two-stack list quietly breaks the moment a third one exists.

**#144 (favorite artists and recently played on Home)** adds two more blocks to that same
lightweight home. Part 1 (merged) is favorite artists' core/wire/daemon side, a straight copy of
the existing `favorite_tracks`/`favorite_albums` pattern: `Catalog::favorite_artists` hits
`/users/{id}/favorites/artists` with the same `favorites_query`/`parse_favorited_items` helpers
already used for the other two; a new `Request::Artists`/`Payload::Artists` pair (protocol 1.14,
under the existing `CAP_CATALOG`) rather than folding into `Payload::Library`, so a problem with
the artists endpoint can't take the favorite-tracks/favorite-albums blocks down with it (those
three already fail or succeed together through one `tokio::join!`). No TUI change yet. Next: the
Library "Favorite artists" tab (part 2), then Home's own block (part 3); recently played (a real
new mechanism — nothing today stores or exposes local play history) follows in parts 4-6.

Part 2 adds the Library "Favorite artists" tab: `LibraryTab` grows to four
(tracks, albums, artists, playlists), `LibraryState` gains its own
`favorite_artists`/`artists_phase` loaded by `artists_request()`, fired at
the same moment as the library and the playlist-folder requests (a third,
independent fetch -- the same reason the Playlists tab already has its
own). Enter on a favorite artist opens its page nested on the library's
own stack, exactly like a favorite album does; `a`/`A` add its top tracks
whole. Found and fixed a real bug along the way: `open_artist_view`
pushed onto `search_views` unconditionally instead of through
`active_stack_mut`, harmless only because Search was its one caller --
opening an artist from Library (now possible) would have put the view on
the wrong stack, the same shape of bug #140 and `find_view` (#139 part 4)
already taught to watch for.

Part 3 adds the Home "Favorite artists" block, right after favorite
albums: `home::Row::Artist`, a block gated on `library.artists_phase ==
Done` like the others, Enter opening the artist's page nested on Home's
own stack (not Library's), `a`/`A` adding its top tracks whole. No
`Row::Header`/`Block` generalization yet -- the plan's own suggestion to
split `Header(LibraryTab)` into a `Block` enum is deferred to part 6,
when recently played (which has no matching `LibraryTab`) actually needs
it; adding that abstraction now, for a block that still maps onto a real
tab, would be scaffolding for a need that is not there yet.

Part 4 starts recently played's own core/daemon side, in memory only
(no TUI yet): a new `phonia-core/src/recent.rs` with a bounded,
deduplicated `RecentlyPlayed` log and a `Tracker` turning engine events
into entries -- simpler than `play_log::SessionTracker` on purpose,
since it only has to answer "heard 30s yet" (`engine::Event::Position`
already stalls while paused, so a plain position check is enough; a
seek forward past the threshold counts, accepted as fine for a local
list). Hooked into `phoniad`'s `fan_in` beside `note_play_log`, for the
same reason: resolving the source has to happen against the exact
queue snapshot at that moment, since a gapless join can move the queue
on first. New `Request::RecentlyPlayed`/`Payload::RecentlyPlayed`/
`Event::RecentlyPlayedChanged` and `CAP_RECENTLY_PLAYED` (protocol
1.15, not gated on `catalog`), plus `phonia ctl recent` so the feature
is usable before the TUI catches up in part 6.

Part 5 saves the log across a `phoniad` restart: `recent::state_dir`
(`$XDG_STATE_HOME/phonia`), `recent::load`/`save` (atomic temp-file-
then-rename, the same precaution `auth::store`'s own session file
takes), loaded at `Daemon::start` and saved by a background
`spawn_blocking` task whenever the log actually changes. `recent_path`
moved into `DaemonParts` rather than resolved inside `Daemon::start`
itself, matching how every other filesystem/network dependency
(`catalog`, `play_log`, the session store) already arrives pre-resolved
-- also what makes it possible to test at all: a test fixture can now
point two separate daemons at the same temp file and confirm the
second one starts already knowing what the first played, a real
round-trip through the filesystem, not just the pure `load`/`save`
functions in isolation. Every other existing test fixture passes `None`
on purpose, so running the test suite never touches a real machine's
actual state directory.

Part 6 (last) adds the Home block and **closes #144**: `State.
recently_played` (fetched once a connection with the `recently_played`
capability exists, whatever section is showing -- unlike the library,
which waits for its own section -- and kept current by `Event::
RecentlyPlayedChanged` from then on), `home::Row::Played`/`Selected::
Played`, right after Continue and before the library's own blocks.
Enter plays an entry, `a`/`A` add it without playing, `o` opens its
radio for a TIDAL one (nothing for a local file, which has no id to
seed one with). This is also where `home::Row::Header(LibraryTab)`
finally generalizes to `Row::Header(Block)`, deferred from #144's part
3 specifically until recently played -- the one block with no matching
`LibraryTab` and no "See all" row -- actually needed it.

**#144 is now fully done, all 6 PRs merged.** Favorite artists (a real
fourth Library tab, straight copy of the existing favorites pattern)
and recently played (a genuinely new mechanism: tracked via a 30s-heard
threshold off the engine's own `Position` events, saved across a
`phoniad` restart) both show on Home now, alongside #139's existing
Continue row and favorite albums/playlists/tracks blocks. "Albums
you'll enjoy" (TIDAL's own editorial feed) remains the deliberately
separate, not-yet-investigated future issue it always was.

## Conventions this file assumes

- Issues are closed by the project owner by hand after their pull requests
  merge, not automatically (pull requests target `develop`, which GitHub does
  not auto-close issues against). A "Code complete" row above with the issue
  still open on GitHub is not a bug in this file; it means the owner has not
  closed it yet.
- Every pull request should carry the milestone of the issue it is part of,
  its assignee, and an `enhancement` (or `bug`/`documentation`) label, so
  GitHub's own filtering stays a second, independent source of truth.
