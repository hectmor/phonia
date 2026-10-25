# Usage

Every command phonia offers: the one-shot CLI, the `phoniad` daemon and its
`phonia ctl` client, the terminal interface key by key, and the IPC protocol
all three speak. Settings that change this behaviour permanently live in
[CONFIGURATION.md](CONFIGURATION.md); what the audio path actually does with a
track is in [AUDIO.md](AUDIO.md). Start at the [README](../README.md).

## Commands

- **`phonia login`** -- Logs in to TIDAL with the PKCE flow (required: the device-code flow
  never gets granted the `HI_RES_LOSSLESS` entitlement, even if the account has it). Opens a
  URL in the browser (or prints it if it couldn't be opened automatically); after logging in,
  TIDAL redirects to an error page ("oops"), that's expected -- copy the full URL from the
  address bar and paste it into the terminal. Saves the session as described in
  "Where the TIDAL session is kept" in [CONFIGURATION.md](CONFIGURATION.md).

- **`phonia logout`** / **`phonia whoami [--check]`** -- forget the TIDAL login / show where it is kept
  and whether there is one (see "Where the TIDAL session is kept" in
  [CONFIGURATION.md](CONFIGURATION.md)).

- **`phonia play <TRACK_ID>... [--device <device>] [--quality hires|lossless|high|low] [--save-mp4 <path>] [--interactive] [--shuffle] [--repeat off|one|all]`**
  -- Streams and plays tracks by their IDs, one after another. Queries `playbackinfo`, streams
  the DASH manifest (HiRes) or the direct file (Lossless/High/Low), decodes it and outputs it via
  ALSA. `--save-mp4` additionally saves the streamed bytes of the first track to disk (useful for
  inspecting the fMP4). Prints what TIDAL answered for the first track opened, including its
  track and album ReplayGain and true peak when TIDAL reports them (#30) -- not yet applied to
  playback, which is the rest of #30.

- **`phonia play-file <path>... [--device <device>] [--interactive] [--shuffle] [--repeat off|one|all]`**
  -- Decodes and plays local files (FLAC or fMP4), one after another, through the same playback
  engine and ALSA output, without touching TIDAL. Useful for testing the DAC in isolation.

- **`phonia tui [--socket <path>]`** -- the terminal interface, a client of a running `phoniad`. It
  has a sidebar (Home, Queue, Search, Library, Lyrics), a main panel and a bar at the bottom. It
  opens on Home (#139). Its first row, "Continue", picks up whatever the queue was doing: resumes
  a paused track at its exact position, replays a stopped one from the start, or starts the queue
  if nothing has played yet this session; `Enter` on it does that. Next, recently played (#144,
  six of it, most recent first, no "See all" -- it is not a TIDAL list to page through): `Enter`
  on an entry there plays it, `a`/`A` add it without playing, and `o` opens its radio for a TIDAL
  one (a local file has no radio to open). Underneath, up to four more blocks show the library's
  own favorite albums, favorite artists (#144), playlist folders and favorite tracks (six each),
  every one ending in a "See all (N) →" row that jumps into the matching Library tab for the
  rest. `Enter` on a favorite track there plays just that track, the same as it does in Library
  itself; `Enter` on an album, an artist or a folder opens it nested in Home's own stack, exactly
  like opening one from Library does (`a`/`A` add it whole without opening it,
  `h`/`Left`/`Backspace` closes it back
  to Home's own rows). The queue is listed
  live, in queue order (the order `phonia ctl queue list` shows and that edits act on, not the
  shuffled play order), with the one playing marked and the row under the cursor highlighted. With
  the focus on the list (`l`), `j`/`k` select an entry, `Enter` plays it, `d` removes it, `J`/`K`
  move it down or up (the cursor goes with it) and `cc` clears the queue: two presses, so a stray
  `c` never does. The bar shows the state, the track and how it
  is delivered (bit depth, rate, quality, and what was asked for if TIDAL gave less), a progress
  bar with the times, and the volume and the shuffle and repeat modes when they are on. Playback
  keys: `Space` pauses or resumes, `n`/`p` skip, `<`/`>` seek 10 s back or forward, `+`/`-`
  change the volume by 5%, `m` mutes, `s` turns shuffle on or off and `r` goes round off, all,
  one. A shared output always has a volume; an exclusive card has one only if its DAC exposes a
  hardware mixer control (#31), which phonia then drives directly -- the bar says why instead of
  pretending when there is none.
  It connects by itself, and if the daemon is not there yet, or goes away, it keeps trying (after
  0.25 s, then twice as long each time up to 5 s) and shows a countdown; `R` tries at once. It
  stops trying only when what answers is not a compatible phonia daemon, and says why. `/`, from
  anywhere, opens the search and starts typing the query: while typing every key is text, so `q`
  and `j` are letters (`Enter` searches, `Esc` stops typing keeping the text, and the line edits
  with `Left`/`Right`, `Home`/`End`, `Backspace`/`Delete`, `Ctrl-w` and `Ctrl-u`). Results come in
  four lists, tracks, albums, artists and playlists, each with its count, that `[` and `]` step
  through, with `j`/`k` to move in the one shown; a track or an album shows its best quality. The
  track playing, if it is among the results, and the album it belongs to (matched by the album's
  own cover id, since a track carries no album id of its own) are marked the same way the queue
  marks its own current entry (#159); the same marker shows on a track inside an opened album, a
  playlist or an artist's top tracks. It needs a daemon that has a TIDAL login, and says so where
  the results would be when it has not. On a track, `Enter` adds it to the end of the queue and
  starts playing it right away, the same as `a` does without playing; `A` adds it right after the
  track playing instead, also without playing, and the bar says how many tracks went in. On an
  album, a playlist or an artist, `Enter` opens it. An album or a playlist shows its title,
  artists, year, quality and copyright, then its tracks, numbered as on it, with the ones TIDAL
  will not stream dimmed; `a`/`A` there still add the whole thing. An artist's page shows its
  name, the start of its bio, and three tabs, its top tracks, its albums, and its EPs and singles
  (`[`/`]` switch between them), each with its own count; `a`/`A` on the artist itself add its top
  tracks whole. Inside any of these, `Enter` on a track queues the whole of it (the album, the
  playlist, or the artist's top tracks) at the end of the queue -- the same as `a` on the album
  row itself -- and starts at the one under the cursor (skipping, in the count, any track before
  it that TIDAL will not stream); `a`/`A` add just that track, without playing. `Enter` on an
  album from an artist's page opens it in turn, on top (the title becomes a full breadcrumb,
  `Search › Korn › Issues`); `a`/`A` there add it whole. `h`, `Left`, `Backspace` or starting a
  new search closes the view on top, back to what opened it. Every list, results and an open view
  alike, loads more by itself as the cursor nears the end of what came, 50 at a time. The
  **library** section needs no query: the moment it is first shown it asks the daemon for it once,
  and says why not where the lists would be if there is no connection yet or the daemon has no
  TIDAL login. It has four tabs (`[`/`]` switch between them, each with its own count): favorite
  tracks, favorite albums, favorite artists (#144), and the playlists made by the account logged
  in (not ones only followed). A favorite album, artist or playlist opens the same way one from a
  search result does (`Library › Issues`), with the same `Enter`/`a`/`A` and closing behaviour; a
  favorite track is the one exception to how a track inside an opened list behaves elsewhere:
  `Enter` plays just that one track, without queuing the rest of the list too, since a list of
  favorites has no natural order and can run into the thousands. **`--covers
  auto|halfblocks|off`** (default `auto`) controls its covers (#24): an opened album's or
  playlist's header, an opened artist's own page, and the **Queue** section's own currently
  playing track show their cover or picture beside their text (the
  artist's tabs stay full width, under both), at whatever size fits (a third of the panel's
  height, in steps, so it grows with the terminal; square in pixels; never shown at all if the
  panel is too small or the item has none),
  through a real graphics protocol (Kitty, Sixel, iTerm2) when the
  terminal has one; `halfblocks` (24-bit colour, `▀`/`▄` characters) only when the
  terminal says it has true colour, so `theme.rs`'s own 16-ANSI-colours rule is never broken by
  accident (unless a palette is chosen in `theme.toml`, see [CONFIGURATION.md](CONFIGURATION.md));
  with neither, no cover shows, rather than one that looks wrong. A cover is fetched
  straight from TIDAL's public image CDN, which needs no TIDAL session, so this is the one thing
  the interface reaches the network for directly (see [DECISIONS.md](DECISIONS.md), 2026-10-01). The bar's
  own **signal path line** (#28) shows what is playing, through what format, to which device, with
  the same `BIT-PERFECT`/`CONVERTED (reason)`/`SHARED ...` verdict `phonia ctl status` prints, e.g.
  `TIDAL hires 24-bit / 96 kHz → S24_3LE → Fosi Audio DS2 (hw:1,0)  ✔ BIT-PERFECT`; the line is
  always reserved, blank until something is known, so the bar's own height never changes with what
  is playing. The same line also shows a track the DAC refuses (`✖ hw:1,0 cannot play 352800 Hz
  natively; ...`, #25's own precise reason) in place of `phonia`'s otherwise bare "Stopped", until
  the next track actually starts or a fresh verdict arrives. Other keys
  are vim-like: `j`/`k` (or the arrows) move, `gg` and `G` go to the ends, `Ctrl-d`/`Ctrl-u` move
  half a page, `h`/`l`/`Tab` change panel, `1`-`3` jump to a section, `?` shows the keys (and
  scrolls with `j`/`k` when the terminal is too short for it) and `q` (or `Ctrl-c`) quits. The help
  is drawn from the same table the keys are read from, so it cannot go out of date. It needs a
  terminal, and says so when it is run from a pipe.

- **`--shuffle` / `--repeat`** (on `play` and `play-file`): the tracks form a queue. `--shuffle`
  plays them in a random order, each once per cycle; `--repeat one` repeats the track that ends
  (skipping with `n` still moves on) and `--repeat all` starts over after the last one.

- **`--interactive`** (on `play` and `play-file`) reads playback commands from the keyboard; type
  one and press Enter: `p` (or just Enter) pauses/resumes, `f` / `r` seek 10 s forward / back,
  `s <seconds>` seeks to a position, `n` / `b` go to the next / previous track (`b` restarts the
  current track after 3 s), `l` lists the queue, `z` toggles shuffle, `x` cycles repeat
  (off, all, one), `d <n>` removes entry `n` (skipping ahead if it is the one playing), `j <n>`
  jumps to entry `n`, `q` quits, `?` lists them. Seeking works on every source: local
  files are repositioned in place, and a TIDAL HiRes stream is reopened at the right segment and
  trimmed to the exact frame. Ctrl+C stops playback and releases the DAC.

- **`phonia probe-device [--device <device>]`** -- Opens the given ALSA device in playback mode
  (without writing anything), disables automatic resampling the same way real playback does, and
  prints its channel range and, for every rate it accepts (44.1 kHz .. 384 kHz), which lossless
  formats (`S16_LE`, `S24_3LE`, `S24_LE`, `S32_LE`) it accepts *at that exact rate* -- a real USB
  DAC commonly constrains format and rate jointly (24-bit only up to 96 kHz, say), so the two are
  tested together, not independently (#25). `phonia devices` shows something related but weaker,
  with no device opened and nothing taken from PipeWire: for a USB card, an `advertises ...` line
  parsed straight from `/proc/asound/cardN/stream0` -- what the device *claims* in its USB
  descriptors, before any kernel quirk or real negotiation, not what `probe-device`/actual playback
  found. HDA and HDMI cards have no `stream0` at all, so they get no such line. The same entry also
  shows a `hardware volume: ...` line when the card has a usable mixer control (#31), e.g. `PCM
  (-63.0..0.0 dB)` -- also passive, also read without opening the device.

All of these are subcommands of the one `phonia` binary (`phonia login`, `phonia play ...`).
From a checkout, without installing anything, run them through cargo instead:

```sh
cargo run --release -p phonia -- login
cargo run --release -p phonia -- play 12345678 --device hw:DS2,0 --quality hires
cargo run --release -p phonia -- play-file one.flac two.flac --interactive     # the device comes from the config file
cargo run --release -p phonia -- devices
cargo run --release -p phonia -- probe-device
```

`--release` is worth it for playback: decoding and feeding the sound server happen in real time,
and a debug build spends about 5% of a core on a 44.1 kHz stream where a release build spends
about 1%.

## The daemon: `phoniad` and `phonia ctl`

`phonia play` plays in the terminal that started it. `phoniad` is the same playback stack (engine,
queue, TIDAL) running in the background, driven over a Unix socket, which is what a TUI or a media
key handler will talk to.

```sh
phoniad                                      # runs in the foreground; Ctrl+C or SIGTERM stops it cleanly

phonia ctl queue add song.flac 233059491     # a file, or a TIDAL track id (or tidal:<id> / file:/abs/path)
phonia ctl queue list
phonia ctl play                              # or `play 3` for entry 3
phonia ctl pause | resume | toggle | next | prev | stop
phonia ctl output                            # the outputs; `output set <n>` plays through another one
phonia ctl volume 60 | +5 | -5   /   phonia ctl mute   # shared outputs, or an exclusive card with a hardware mixer control
phonia ctl quality                           # the tiers asked for and what the playing track got; `quality lossless` sets the best
phonia ctl search nu metal --kind albums     # search TIDAL; tracks print the `tidal:<id>` that `queue add` takes
phonia ctl queue add album:33723912          # a whole album (or `playlist:<uuid>`), from what `search` printed
phonia ctl release                           # pause and hand the DAC back, so another program can use it
phonia ctl seek 90                           # 1:30; `+10` / `-10` are relative
phonia ctl shuffle on   /   phonia ctl repeat all
phonia ctl queue rm 2 | clear | move 3 1
phonia ctl watch                             # events as they happen: state, position, bit-perfect report...
phonia ctl status --json                     # any command can print the raw protocol JSON
phonia ctl shutdown
```

- The titles and lengths of the tracks are looked up **when they are added** (several at once), so
  `queue list` is meaningful straight away. A track that is wrong (a missing file, a TIDAL id that
  doesn't exist) is refused and named; one whose details couldn't be fetched right now (TIDAL
  unreachable) is added without them.
- `stop` releases the audio device but keeps the queue, so other applications (PipeWire) can use
  the DAC while the daemon is idle.
- **Sharing the DAC.** In exclusive mode phonia holds the card, so nothing else can play on it. To
  use it for something else (a video in the browser, say), pause: after `release_after_pause`
  seconds (10 by default) phonia closes the device and gives the card back, or `phonia ctl release`
  does it at once. The track and the exact position are kept, and the audio that had been queued
  in the DAC but not yet heard is kept in memory, so `resume` takes the card again (and checks
  bit-perfect again) and continues from the very sample where you were, for a file or a TIDAL
  stream alike. If someone else has the card and won't let go, `resume` says who and phonia stays
  paused, ready to try again. `phonia ctl status` shows `paused (DAC released)`. `phonia play
  --interactive` has `o` for the same. A pause shorter than the time keeps the card, so a quick
  pause never makes the next `resume` slower.
- The socket is `$XDG_RUNTIME_DIR/phonia/phoniad.sock` (override with `[daemon] socket` in the config
  file, or `--socket`; `phoniad` and `phonia ctl` read the same file), in a directory only you can enter, mode `0600`, and only connections from your own
  user are served. A second `phoniad` refuses to start while one answers; a socket left by a
  daemon that was killed is replaced.
- `phoniad --verbose` prints the library's own notes and warnings; by default it prints nothing to
  stdout.
- **`phonia tui` starts its own `phoniad`** when nothing answers yet on the socket (same `--socket`,
  and `--config` if one was given), and stops only that one -- on quit, on SIGTERM, or when the
  terminal closes -- never a daemon it didn't start. Its output goes to
  `~/.local/state/phonia/phoniad.log`. Running `phoniad` yourself (by hand, or later as a systemd
  user service) keeps working exactly as above; the TUI never touches it. If a second TUI attaches
  to the first TUI's self-started daemon, it loses that daemon when the first TUI quits -- run
  `phoniad` yourself first if it should outlive any one TUI session.

### The protocol

One connection carries requests, their responses and, once subscribed, events, as newline-delimited
JSON, so `socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/phonia/phoniad.sock` is a working client:

```
< {"type":"hello","protocol":{"major":1,"minor":6},"server":{"name":"phoniad","version":"0.1.0","pid":1234},"capabilities":["output_release","output_select","volume","gapless","quality","catalog"]}
> {"id":1,"request":{"type":"hello","protocol":{"major":1,"minor":0},"client":{"name":"me","version":"0"}}}
< {"type":"response","id":1,"ok":{"type":"ack"}}
> {"id":2,"request":{"type":"subscribe"}}
< {"type":"response","id":2,"ok":{"type":"snapshot","seq":41,"status":{...},"queue":{...}}}
< {"type":"event","seq":42,"event":{"type":"position","position_ms":1234,"duration_ms":300000}}
```

The server speaks first; `hello` must be the first request. `subscribe` answers with the whole
state and then pushes events, numbered consecutively and in the same order for every client; a
client that falls too far behind is sent a `resync` with the current state instead of the events
it missed. The major version must match; minor versions only add things, and unknown fields and
variants are ignored. The exact format of every message is pinned by the golden tests in
`crates/phonia-ipc/tests/golden.rs`, and the `phonia-ipc` crate is a ready-made client
(`Client::connect`, `request`, `subscribe`).
