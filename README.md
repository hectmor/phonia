# phonia -- phase 0

End-to-end CLI spike that validates **phonia**'s audio path (the future TIDAL hi-fi player
with TUI + daemon): PKCE login -> HiRes `playbackinfo` -> DASH segment download -> FLAC
decoding -> *bit-perfect* ALSA output to a USB DAC.

There's no TUI or daemon yet: this is a single-pass CLI to prove that every link in the
chain works with real hardware (a Fosi Audio DS2 during development).

## Commands

- **`phonia login`** -- Logs in to TIDAL with the PKCE flow (required: the device-code flow
  never gets granted the `HI_RES_LOSSLESS` entitlement, even if the account has it). Opens a
  URL in the browser (or prints it if it couldn't be opened automatically); after logging in,
  TIDAL redirects to an error page ("oops"), that's expected -- copy the full URL from the
  address bar and paste it into the terminal. Saves the session to
  `~/.config/phonia/session.json` (`0600` permissions).

- **`phonia play <TRACK_ID>... [--device <device>] [--quality hires|lossless] [--save-mp4 <path>] [--interactive] [--shuffle] [--repeat off|one|all]`**
  -- Streams and plays tracks by their IDs, one after another. Queries `playbackinfo`, streams
  the DASH manifest (HiRes) or the direct file (Lossless/High/Low), decodes it and outputs it via
  ALSA. `--save-mp4` additionally saves the streamed bytes of the first track to disk (useful for
  inspecting the fMP4).

- **`phonia play-file <path>... [--device <device>] [--interactive] [--shuffle] [--repeat off|one|all]`**
  -- Decodes and plays local files (FLAC or fMP4), one after another, through the same playback
  engine and ALSA output, without touching TIDAL. Useful for testing the DAC in isolation.

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
  (without writing anything) and lists which formats (`S16_LE`, `S24_3LE`, `S24_LE`, `S32_LE`)
  and which rates (44.1 kHz .. 384 kHz) it accepts natively.

All commands are run with `cargo run -p phonia -- <command>`, for example:

```sh
cargo run -p phonia -- login
cargo run -p phonia -- play 12345678 --device hw:DS2,0 --quality hires
cargo run -p phonia -- play-file one.flac two.flac --interactive     # the device comes from the config file
cargo run -p phonia -- devices
cargo run -p phonia -- probe-device
```

## Configuration

The settings live in `~/.config/phonia/config.toml` (`$XDG_CONFIG_HOME/phonia/`). Nothing is
required to exist except the audio device:

```toml
[output]
device = "hw:DS2,0"     # a sound card: `phonia devices` lists them. "auto" = the first USB card.
mode = "exclusive"      # phonia owns the card, nothing mixes or resamples: bit-perfect (the only mode so far)

[tidal]
max_quality = "hires"   # hires | lossless

[daemon]
socket = "/run/user/1000/phonia/phoniad.sock"   # default: $XDG_RUNTIME_DIR/phonia/phoniad.sock
verbose = false
```

- **Name the card, not its number.** ALSA numbers cards in the order the kernel finds them, so a USB
  DAC that was `hw:2,0` yesterday is `hw:1,0` today. Its *id* doesn't change: write `hw:DS2,0`
  (or `hw:CARD=DS2,DEV=0`) and phonia turns it into the current number every time it opens the
  device, so a daemon that has been running for days still finds a DAC that was unplugged and
  plugged back in. `phonia devices` lists the cards and the exact text to put in the file; a card
  that isn't there is an error that says which ones are.
- **Precedence** is command line, then the file, then the built-in default. There is no default
  device: with none configured (and no `--device`) phonia refuses to start and says how to set
  one, rather than guess a card and play on the wrong one.
- **Mistakes are errors.** A key it doesn't know (`devise = ...`), a value of the wrong type or a
  word that isn't an option stops it with the file, the line and what was expected, instead of
  being ignored and playing on the wrong card. A missing file just means the defaults; a file you
  named with `--config` (or `PHONIA_CONFIG`) that doesn't exist is an error.
- `phonia config path` prints which file is used, and `phonia config show` prints every setting
  with where its value comes from (the file or the default). `--config <file>` (or the
  `PHONIA_CONFIG` environment variable) selects another file, also for `phoniad`.
- The daemon reads the file once, at startup: restart it to apply a change. The file holds no
  secrets (the TIDAL session is kept apart, in `session.json`).

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
- The socket is `$XDG_RUNTIME_DIR/phonia/phoniad.sock` (override with `[daemon] socket` in the config
  file, or `--socket`; `phoniad` and `phonia ctl` read the same file), in a directory only you can enter, mode `0600`, and only connections from your own
  user are served. A second `phoniad` refuses to start while one answers; a socket left by a
  daemon that was killed is replaced.
- `phoniad --verbose` prints the library's own notes and warnings; by default it prints nothing to
  stdout.

### The protocol

One connection carries requests, their responses and, once subscribed, events, as newline-delimited
JSON, so `socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/phonia/phoniad.sock` is a working client:

```
< {"type":"hello","protocol":{"major":1,"minor":0},"server":{"name":"phoniad","version":"0.1.0","pid":1234},"capabilities":[]}
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

## Important: PipeWire must not have the DAC open

`phonia` opens the ALSA device you configured directly, without going through
`plughw`/`default`/`dmix`, because any of those layers can resample or mix the audio and
break the bit-perfect guarantee. If PipeWire (or another application) already has the DAC
open, `phonia` will fail to open the device with an EBUSY error explaining that it needs to
be released first (for example, by pausing playback to that card from PipeWire, or by
muting/disabling its profile for that card while using `phonia`).

## Phase 0 status

- `cargo build` and `cargo test` pass cleanly; `cargo clippy` has no warnings.
- Since then: DASH segments are streamed on demand, playback runs through an engine with pause,
  seek, next/previous and a heard-position report (issue #10), and tracks are played from an
  in-memory queue with shuffle and repeat (issue #11).
- What's still out of scope (coming in later phases): TUI, daemon, gapless playback, `%0Nd` in
  DASH segment templates, manifest encryption support.

## Tech stack

Every dependency here exists to protect one single guarantee end to end: **no bit gets touched
in a way that isn't reversible**. This section explains what each piece of the stack does and,
more importantly, *why* it was chosen.

- **Rust** -- a systems language with no garbage collector and no hidden allocations was a hard
  requirement for the audio output path: a GC pause or an unexpected allocation on the hot path
  can produce an audible glitch. Rust also makes the "never silently convert through a float"
  invariant (see `decode`/`output::alsa` below) something the type system helps enforce, not just
  a comment.

- **[`clap`](https://docs.rs/clap)** -- parses the CLI's subcommands and flags (`login`, `play`,
  `play-file`, `probe-device`). Declarative, well-tested, and there was no reason to hand-roll
  argument parsing for a project this size.

- **[`toml`](https://docs.rs/toml)** -- reads `config.toml` into `serde` structs. Chosen over
  `basic-toml` because its errors carry the line, the column and the offending key, which is what
  makes "you wrote `devise`" a useful message; and over `toml_edit`, which is for programs that
  rewrite the file (this one only reads it).

- **`phonia-ipc`** (a crate of this workspace) -- the wire protocol and a client for the daemon. It
  depends only on `serde` and `tokio`, not on `phonia-core`, so a terminal UI, an MPRIS bridge or
  an agent server can talk to the daemon without building ALSA or the decoders. The wire types are
  its own (durations in milliseconds, no internal references leaking out) instead of `serde` derives
  on the engine's types, so the format can outlive refactors. The framing is a few lines over
  `tokio` rather than a codec crate.

- **[`tokio`](https://docs.rs/tokio)** -- the async runtime. Needed because talking to TIDAL
  (`reqwest`, `tidlers`) is inherently async I/O. The decode+ALSA-write loop, by contrast, is
  synchronous, blocking work, so the playback engine runs it on a dedicated OS thread instead of
  the async executor's thread pool -- blocking that pool would stall every other async task
  (including the Ctrl+C listener, and the segment downloads the audio thread waits on) for the
  whole playback. Anything slow the engine needs (asking TIDAL for a track) runs on the runtime
  and comes back to the audio thread as a message.

- **[`reqwest`](https://docs.rs/reqwest)** -- the HTTP client used both for TIDAL's REST API
  (`playbackinfopostpaywall`) and for downloading DASH segments / direct audio URLs from TIDAL's
  CDN. One client instance is reused for the whole session (connection pooling, consistent
  `User-Agent`).

- **[`tidlers`](https://docs.rs/tidlers)** -- the Rust TIDAL client. It implements the actual
  OAuth/PKCE dance (building the `code_challenge`, exchanging the authorization code for tokens,
  refreshing them) -- reimplementing OAuth crypto by hand would be pure risk with no upside. Its
  `playbackinfo` response type and `DashManifest`, however, are incomplete for phase 0's needs
  (see `tidal.rs`/`dash.rs` below), so those two pieces are implemented directly against TIDAL's
  API instead of forking the crate's internals (most of what's needed is `pub(crate)`).

- **`base64` + `serde` / `serde_json`** -- TIDAL's `playbackinfo` response embeds its actual
  manifest as a base64-encoded string, which decodes to either JSON (LOW/HIGH/LOSSLESS: a plain
  direct-download URL) or DASH/XML (HiRes: a segmented manifest). `serde_json` deserializes the
  outer response and the JSON-manifest case; `base64` decodes the embedded blob before either
  path can proceed.

- **[`quick-xml`](https://docs.rs/quick-xml)** -- parses the DASH (MPD) manifest XML for HiRes
  tracks in `dash.rs`: `SegmentTemplate`, `SegmentTimeline`, `BaseURL`, segment counting. Chosen
  as a low-level, allocation-conscious streaming XML reader rather than a full DOM parser, since
  all we need is a handful of attributes and text nodes out of a well-known, small manifest.
  Note from hard-won experience: `quick-xml` does **not** unescape XML entities for you by
  default, and it delivers `&amp;`/`&lt;`/etc. inside text content as *separate*
  `Event::GeneralRef` events rather than inline in `Event::Text` -- both attribute values and
  `<BaseURL>` text have to be explicitly unescaped/reassembled, or pre-signed CDN URLs (which
  routinely contain `&`-separated query parameters escaped as `&amp;`) come out corrupted and get
  rejected by the CDN with a 404/403.

- **[`rand`](https://docs.rs/rand)** -- the shuffle of the playback queue. A shuffle has to be
  unbiased (every order equally likely) and each entry has to play once per cycle, which is a
  Fisher-Yates shuffle over a permutation, not "pick a random track each time". Using the
  well-tested implementation instead of a hand-rolled generator avoids subtle bias, and its
  seedable `StdRng` lets the queue's tests assert an exact play order from a fixed seed while
  playback itself is seeded from the operating system.

- **[`symphonia`](https://docs.rs/symphonia)** -- the pure-Rust audio decoder (FLAC standalone
  and FLAC-in-fMP4/DASH, both of which TIDAL uses depending on quality tier). Chosen because it's
  the most mature pure-Rust decoder available (also used by Firefox) and because its FLAC decoder
  decodes each subframe into raw, left-justified `i32` samples -- `copy_to_vec_interleaved::<i32>`
  on that output is a pure identity copy, no rounding, no float round-trip. That property is the
  foundation of the whole bit-perfect guarantee: the moment any sample got routed through `f32`,
  the guarantee would be silently broken.

- **[`alsa`](https://docs.rs/alsa) (Rust bindings for `libasound`)** -- the only way to talk to
  Linux audio hardware directly without hand-writing FFI. Used to open `hw:N,D` devices directly
  (never `default`/`plughw`/`dmix`, all of which can resample or mix and would break
  bit-perfectness by definition), negotiate a lossless integer hardware format, and pack the
  decoder's left-justified `i32` samples into that format with pure bit shifts.

- **[`anyhow`](https://docs.rs/anyhow)** -- error handling with contextual, human-readable
  messages at every fallible step, from "no saved session, run `phonia login`" to "the device
  doesn't support any lossless integer format for a 24-bit source."

## License

MIT. See the [LICENSE](LICENSE) file.
