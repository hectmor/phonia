# Audio

What phonia does with a track between TIDAL and the DAC, and how to check it
for yourself: proving the output is bit-perfect, sharing the card with
PipeWire, gapless joins, ReplayGain, play reporting, MPRIS, and shared mode —
the one deliberate, clearly-labelled exception to bit-perfectness. The settings
that select any of it are in [CONFIGURATION.md](CONFIGURATION.md); why each
piece was built this way is in [DECISIONS.md](DECISIONS.md). Start at the
[README](../README.md).

## How to verify the output is bit-perfect

When playing with `phonia play` or `phonia play-file` against a `hw:N,D` device, after the
first audio chunk the contents of `/proc/asound/card<N>/pcm<D>p/sub0/hw_params` are printed,
followed by a verdict:

```
Source: FLAC 24-bit/96000 Hz 2ch → hw:1,0 S24_3LE 96000 Hz  ✔ BIT-PERFECT
```

or, if something doesn't match (a different rate or format than what was negotiated, or the
device is not `hw:N,D`):

```
Source: FLAC 24-bit/96000 Hz 2ch → hw:1,0 S24_3LE  ✖ CONVERTED (the card reports 48000 Hz instead of 96000 Hz)
```

You can also check it by hand in another terminal while something is playing:

```sh
cat /proc/asound/card1/pcm0p/sub0/hw_params
```

If you see `closed`, nothing has the device open at that moment.

**`phoniad` carries the same verdict over IPC** (`Event::SinkReport`, and, since protocol 1.7,
`Status.sink_report`, so it is there the moment a client connects or resyncs, not only at the
next sink open): `phonia ctl status` shows it as a `Verdict:` line (`S24_3LE BIT-PERFECT`), and
`phonia ctl watch`/the event log print the same `BIT-PERFECT`/`CONVERTED (reason)`/`SHARED ...`
wording `phonia play` does (#28). The verdict is dropped once the sink that produced it closes
(released, or playback stopped) or changes format or output, so it never outlives what it
describes.

## Sharing the DAC with PipeWire

`phonia` opens the ALSA device you configured directly, without going through
`plughw`/`default`/`dmix`, because any of those layers can resample or mix the audio and
break the bit-perfect guarantee. That means only one program can have the DAC at a time, and on
a desktop that is normally WirePlumber (PipeWire's session manager) or PulseAudio.

They share cards with each other through the `org.freedesktop.ReserveDevice1` D-Bus protocol,
and phonia speaks it: before opening the card it asks whoever holds it to let go, and it keeps
the card reserved for as long as it is playing, so the desktop can't grab it in the middle of a
track or between two tracks of different formats. When phonia stops, pauses for longer than
`release_after_pause`, is told to `release`, or dies (even with `kill -9`), the card goes back to
the desktop by itself. There is nothing to configure and no `pactl set-card-profile ... off` to
run.

- While phonia has the DAC, the DAC is not an output of the desktop any more: what was playing
  through it moves to another output (the laptop's speakers, say), exactly as it does when you
  turn the card's profile off by hand.
- If the holder refuses, the error names it: `the DAC hw:2,0 (DS2) is held by <program>, which
  refused to release it to phonia`. If nothing answers on D-Bus at all (no session bus, as over
  SSH or in a system service) phonia says so and tries to open the card anyway, which works if
  nothing else has it.
- A program that does not use the protocol and has the card open makes the open fail with
  EBUSY; the message says how to find it (`fuser -v /dev/snd/*`).
- Another program that matters more (higher priority than phonia's 10, such as JACK) can ask
  phonia for the card: phonia pauses, gives it up and reports `paused (DAC released to <program>)`.
  It never resumes by itself; `phonia ctl resume` takes the card again.
- `reserve = false` under `[output]` turns all of this off: phonia never touches D-Bus and opens
  the card as it always did, so the desktop must not be using it (mute the card's profile in
  PipeWire while using phonia).

## Gapless playback

Albums that run into each other (live records, DJ mixes, classical movements) need the next track to
start on the very sample after the last one. With `[playback] gapless = true` (the default) phonia
opens the next track about 30 seconds before the current one ends (asking TIDAL, probing the
stream and decoding its first chunk) and, when the current one runs out, writes the next one's first
sample right behind the last one's: the sound card is not drained, stopped or reopened, and in
shared mode the 100 ms of silence that follows a restart is not added either. Nothing about the
samples changes, so it is as bit-perfect as before.

- **The boundary is announced when it is heard**, not when it is written: the sound card still holds
  up to half a second of the old track after phonia has started writing the new one, so the old
  track ends, and the new one starts, at the moment the listener crosses from one to the other, and
  positions until then are the old track's. `Next`, `Stop` and a seek during that moment act on the
  track that is being heard, and moving to another output or handing the card back in the middle of
  it loses and repeats nothing.
- **When it can't be gapless it says so and falls back**, never plays wrong: if the next track has a
  different sample rate or bit depth the card is drained and reopened for it (a short gap, but no
  wait for the network, since it was opened ahead); if TIDAL hasn't answered by the time the last
  audio plays out, the engine waits for that answer instead of asking twice; a track that fails to
  open ahead is tried again the ordinary way. A shuffled queue that starts a new round reshuffles,
  and that one boundary is a normal one.
- **Skipping** (`next`, or a seek past the end) while the next track is already open starts that one
  as it is instead of asking TIDAL for it again, so it begins at once. `phonia ctl watch` and
  `phonia play` mark a track that was joined to the one before it (`[gapless]` / `(gapless)`), and
  over the socket `track_started` carries `"gapless": true` with no `state_changed` around it.
- Repeat-one loops a track without a gap. Removing or moving the next track while it is being
  opened just opens the right one instead.
- Gapless is exact for FLAC (local files and TIDAL's DASH streams, which decode to exactly the
  frames their manifest declares). Lossy formats have encoder padding at the ends that phonia does
  not trim.
- `gapless = false` gives the old behaviour: drain, then load the next track.

## ReplayGain

`[playback] replaygain` decides whether TIDAL's own loudness measurement is applied: `off`
(default), `track` (always the track's own gain), `album` (always the album's, falling back to
the track's when TIDAL has none for the album) or `auto` (the album's gain when the track sits
next to another entry of the same album in play order and shuffle is off, the track's own
otherwise). A boost is capped so the track's own reported true peak never clips; a cut is never
capped.

The gain is applied **in shared mode only**, by scaling samples before they reach the sound
server -- exactly the one mode that already isn't bit-perfect. Exclusive mode gets no new code at
all: `AudioSink::set_gain` defaults to doing nothing, and `AlsaSink` never overrides it, so a
sound card phonia owns outright stays bit-perfect-or-refuse exactly as before, whatever
`replaygain` says. A gapless join between two tracks with different gains switches exactly at the
sample boundary between them, even if the output is switched mid-join.

When a gain is actually in effect (shared mode, and `replaygain` is not `off`), `phonia ctl status`
shows a `Gain:` line (`RG -2.9 dB (album)`) and the TUI's bar shows it next to the volume. Nothing
is shown in exclusive mode, even though the same gain is still *decided* for an exclusive-mode
track (TIDAL's loudness data doesn't depend on the output) -- showing it there would claim an
effect that was never applied.

## Play reporting

`[tidal] report_plays` (default on) reports a track to TIDAL once it has actually been heard for
30 seconds, so it shows up in Recently Played on the account -- the same mechanism TIDAL's own
apps use: there is no simpler, documented "mark this played" endpoint, only a generic analytics
event (`playback_session`) sent through TIDAL's own internal event pipeline. This is undocumented
and best-effort: a failure to send it is always silent and never affects playback, and TIDAL could
change the contract without notice. See [DECISIONS.md](DECISIONS.md) for the full reasoning, including
what is still provisional about it (#120).

A play counts once it has actually been heard for 30 real seconds (paused time doesn't count),
whatever reason it ends for -- completed, skipped or failed -- and however long the track is;
every play is reported as a plain track, not attributed to the album or playlist it was queued
from (a seam left for a later issue). Only `phoniad` sends these, through the same TIDAL login it
already uses for everything else.

## MPRIS: media keys and desktop widgets

`[daemon] mpris` (default on) registers `org.mpris.MediaPlayer2.phonia` on the session bus, so
GNOME/KDE media keys, lock-screen widgets and `playerctl` all see and control `phoniad` the same
way they do any other player -- falling back to `...phonia.instance<pid>` if a second instance is
already running, or skipping MPRIS entirely (one line on stderr, nothing else affected) if even
that name can't be had. A missing session bus -- headless, a minimal install -- never blocks or
delays startup.

Full transport (play/pause/stop/next/previous/seek) and the writable `Volume`/`LoopStatus`/
`Shuffle` properties all route through the exact same request path `phonia ctl` itself uses, so
nothing behaves differently depending on who asked. `Raise`, `Quit` and `OpenUri` are
deliberately not implemented (`CanQuit`/`CanRaise` are always false): phonia has no window to
raise, no notion of opening an arbitrary URI, and a desktop widget's own "close" button must
never be able to kill the daemon the TUI depends on.

## Shared mode: any output, not bit-perfect

Exclusive mode is the point of phonia, but it only works on a sound card that phonia can have for
itself. **`mode = "shared"`** plays through the desktop's sound server instead (PipeWire, through
its PulseAudio-compatible interface, or PulseAudio itself), so any output the desktop has works:
Bluetooth speakers, HDMI, the laptop's speakers, or the DAC while other programs also use it.

```toml
[output]
mode = "shared"
sink = "bluez_output.AA_BB_CC_DD_EE_FF.1"   # from `phonia devices`; or "default" for the desktop's
```

- **It is not bit-perfect, and phonia says so** each time a track starts: `✖ SHARED (not
  bit-perfect)`, and for Bluetooth `✖ SHARED, LOSSY CODEC (SBC)`. The server mixes phonia with
  everything else and converts the audio; the decoding is still full quality, and phonia hands
  the server the exact samples it decoded (24 bits, at the track's own rate), so PipeWire's only
  job is one resampling to the rate the output runs at (48 kHz by default) and the Bluetooth
  codec, if any, which is lossy whatever TIDAL sent.
- `phonia devices` lists both worlds: the sound cards for exclusive mode and the server's
  outputs for shared mode, with Bluetooth ones marked and the codec named.
- **`sink = "default"` follows the desktop** when you change its default output. **A named output
  is never swapped**: if that Bluetooth speaker switches off, playback stops with an error instead
  of quietly moving to the laptop's speakers.
- `--device` on the command line means exclusive mode for that run, whatever the file says.
- To make PipeWire follow the track's sample rate instead of resampling to 48 kHz on an idle
  output, allow the rates in its configuration (`default.clock.allowed-rates`). phonia doesn't
  change anything in the system.
- Shared mode never reserves the card and never touches D-Bus for it. It also does not hand the
  card back after a pause (`release_after_pause` is for exclusive mode): a paused stream blocks
  nobody.
- **Switching while playing.** With the daemon running, `phonia ctl output` lists every output
  (the sound cards, marked bit-perfect, and the sound server's, marked shared, Bluetooth ones with
  their codec) and `phonia ctl output set <n>` (a number, an id such as `shared:default`,
  `exclusive:hw:DS2,0` or `shared:<name>`, or part of a name) moves playback there from now on.
  The track and the exact position are kept: phonia pauses, sets aside the audio the old output had
  not played yet, closes it (a card is handed back to the desktop) and carries on from the same
  sample on the new one. If the new output can't be opened, playback stays paused on the track, says
  why, and `phonia ctl resume` tries again. `phoniad --output <id>` and `phonia play --output <id>`
  choose the output for one run, whatever the config file says.
- **When the output goes away** (a Bluetooth speaker is switched off, the server stops) playback
  pauses on the spot, with the position kept, and reports `DAC released (the output went away)`.
  It does not move to another output and does not resume by itself; when the speaker is back,
  `phonia ctl resume` carries on from where it was, or `phonia ctl output set` picks another.
  `phonia ctl watch` shows outputs appearing and disappearing.
- One limit: while phonia holds a card in exclusive mode the desktop has no output for it, so the
  card's shared output is not in the list. `phonia ctl release` gives the card back and it
  reappears.
- **Volume and mute**: `phonia ctl volume` shows it, `phonia ctl volume 60` sets 60%, `volume +5` /
  `volume -5` change it, and `phonia ctl mute [on|off|toggle]` mutes without losing the level. The
  scale is the one the desktop's mixers show: 100% is unity gain (never more) and it is cubic in
  amplitude, so 50% is about -18 dB -- the same curve whichever of the two mechanisms below applies.
  - In **shared mode** it is digital: it scales phonia's own stream before the sound server mixes
    it, never touching any hardware. The level stays across tracks (a new format is a new stream),
    across pauses, and when you switch to another shared output; and if you move phonia's slider in
    the desktop's mixer, `phonia ctl status` and `watch` follow.
  - In **exclusive mode**, phonia drives the DAC's **own hardware mixer control** directly when it
    has one (#31) -- the stream itself stays untouched and bit-perfect either way. A card with no
    such control stays fixed at 100%, and `volume`/`mute` say so, suggesting the DAC's own knob or a
    shared output instead. A hardware volume is never carried in from a previous output, or seeded
    to any particular value: it is read fresh and only ever changed when you explicitly ask, so
    `phonia ctl status` always shows whatever the card's own control is *actually* set to, even if
    something else (the DAC's own knob, or the desktop reclaiming the card) changed it since: a
    background watcher notices and `phonia ctl watch`/the TUI follow, the same as the desktop's own
    mixer already does in shared mode.
