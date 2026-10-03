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
| #25 | DAC capability detection | Code complete (PRs #106–#109, all 4 parts); **issue left open on GitHub, worth closing by hand** |
| #26 | Per-track sample rate switching | Code complete, same plan and PRs as #25; **issue left open on GitHub, worth closing by hand** |
| #28 | Signal path indicator in the TUI | Code complete (PRs #110–#112, all 3 parts); **issue left open on GitHub, worth closing by hand** |
| #30 | ReplayGain in shared mode | Code complete (all 4 parts); **issue left open on GitHub, worth closing by hand** |
| #31 | Hardware mixer volume | In progress (approved 3-PR plan; parts 1–2 done) |

### Phases 4 and 5

Not started, except CI (#42) and rustfmt-in-CI (#50), both closed. Nothing
here is planned in detail yet; issues #32–#45 hold one-line descriptions each,
to be scoped with Opus when their turn comes.

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

#25, #26, #28 and #30 are all code-complete but still open on GitHub
(see "Conventions" below) — close them by hand when convenient.

## Conventions this file assumes

- Issues are closed by the project owner by hand after their pull requests
  merge, not automatically (pull requests target `develop`, which GitHub does
  not auto-close issues against). A "Code complete" row above with the issue
  still open on GitHub is not a bug in this file; it means the owner has not
  closed it yet.
- Every pull request should carry the milestone of the issue it is part of,
  its assignee, and an `enhancement` (or `bug`/`documentation`) label, so
  GitHub's own filtering stays a second, independent source of truth.
