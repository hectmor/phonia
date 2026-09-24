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
  address bar and paste it into the terminal. Saves the session as described in
  "Where the TIDAL session is kept" below.

- **`phonia logout`** / **`phonia whoami [--check]`** -- forget the TIDAL login / show where it is kept
  and whether there is one (see "Where the TIDAL session is kept").

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
required to exist except the audio device (in exclusive mode):

```toml
[output]
device = "hw:DS2,0"     # a sound card: `phonia devices` lists them. "auto" = the first USB card.
mode = "exclusive"      # exclusive: phonia owns the card, bit-perfect. shared: through PipeWire, any output
sink = "default"        # shared mode only: "default" (the desktop's) or an output's name from `phonia devices`
reserve = true          # ask WirePlumber/PulseAudio to release the card first, and give it back after
release_after_pause = 10   # seconds a pause lasts before the card is handed back; 0 = on every pause, "never" = keep it

[tidal]
max_quality = "hires"   # hires | lossless
session_store = "keyring"  # where the TIDAL login is kept: keyring | file (see below)

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
  secrets (the TIDAL session is kept apart, see below).

### Where the TIDAL session is kept

What has to survive between runs is small: the **refresh token** (TIDAL never rotates it, so it is
the only long-lived credential) and the client id and secret it was issued to. That is all phonia
stores. The access token (it lasts four hours) and your profile (email, birthday, user id...) are
not kept: the first use of TIDAL in each process fetches a new access token, which takes a fraction
of a second.

- **`session_store = "keyring"`** (the default) keeps it in the desktop keyring, through the
  freedesktop Secret Service that GNOME Keyring, KWallet and KeePassXC all provide. phonia speaks
  that D-Bus API itself with the `zbus` it already has for the sound card, so it costs no extra
  crate. If the keyring is locked, `phonia login`, `whoami` and `logout` show its unlock prompt; a
  daemon does not at startup (there is nobody to answer) but does the first time TIDAL is used.
  Look at it with `secret-tool search application phonia` or in Seahorse / KDE Wallet Manager.
- **`session_store = "file"`** keeps it in `~/.config/phonia/session.json`, mode `0600`, replaced in
  one step so a crash never leaves half a file. This is what to use where there is no keyring: over
  SSH, in a system service, on a machine with no desktop. phonia does not guess: with `keyring`
  and no Secret Service it stops and tells you to choose `file`.
- **Moving an old `session.json`.** If you logged in before the keyring existed, the first run copies
  the session into the keyring, reads it back to be sure it is there, then overwrites the file with
  zeros and removes it. If any step fails the file stays and is used, with a warning: the session is
  never lost. (Overwriting is best effort: a journaling filesystem or an SSD may keep old blocks.
  If that matters, `phonia logout` and log in again, and revoke the old one in your TIDAL account
  settings.)
- `phonia whoami` says where the login is kept, which program provides the keyring, and whether
  there is a session; `--check` also asks TIDAL whose it is. It never prints a token.
  `phonia logout` removes the stored session, any old `session.json` and an unfinished login. It
  cannot revoke the token on TIDAL's side (the library has no such call).

What the keyring does and does not protect: the Secret Service does not check which program asks,
so any process running as you can read an unlocked keyring, and it could watch your session bus as
well; that is why the secret goes across it unencrypted (`plain`), which costs nothing. What you
gain over a file is that it is encrypted on disk behind your login password, it doesn't end up in
backups or synced dotfiles, and it is locked while you are logged out. Note that on a machine with
two keyring programs (KDE with both `gnome-keyring` and `ksecretd`, say) whichever owns
`org.freedesktop.secrets` at that moment is the one phonia sees: if it changes, the session seems
to vanish and you log in again.

The login is read when TIDAL is first used, not when the program starts, so a `phoniad` that came
up before the network (or before `phonia login`) works as soon as they are there, without a
restart. Refreshing the access token changes nothing in the store, so `phonia` and `phoniad` never
write over each other.

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

### The protocol

One connection carries requests, their responses and, once subscribed, events, as newline-delimited
JSON, so `socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/phonia/phoniad.sock` is a working client:

```
< {"type":"hello","protocol":{"major":1,"minor":1},"server":{"name":"phoniad","version":"0.1.0","pid":1234},"capabilities":["output_release"]}
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
- Volume, and switching between outputs while playing, come in the next steps of this feature.

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

- **[`zbus`](https://docs.rs/zbus)** -- the D-Bus client that reserves the sound card
  (`org.freedesktop.ReserveDevice1`): phonia must own a bus name, export an object that answers
  `RequestRelease` and ask another program's object to release the card, which is a real D-Bus
  peer and not a one-off call, so shelling out to `busctl` can't do it (a child process can't
  hold the name for us, and the card would go straight back to WirePlumber). Pure Rust, so no
  system library or `pkg-config`; only its `tokio` feature is on, so it runs on the runtime the
  daemon already has, and it also speaks the Secret Service that keeps the TIDAL login (no extra
  crate for that: `keyring`, `secret-service` and `oo7` would each add a dozen or more, for
  something a few hundred lines over `zbus` do). It brings about thirty small crates (`zvariant`, `enumflags2`, ...), the
  price of not writing the D-Bus wire protocol by hand. The alternative, `dbus`, has fewer crates
  but links the C `libdbus` and needs a thread of its own for every reservation.

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
  Linux audio hardware directly without hand-writing FFI. Used to open `hw:N,D` devices directly in
  exclusive mode (never `default`/`plughw`/`dmix`, all of which can resample or mix and would break
  bit-perfectness by definition), negotiate a lossless integer hardware format, and pack the
  decoder's left-justified `i32` samples into that format with pure bit shifts.

- **[`pulseaudio`](https://docs.rs/pulseaudio)** -- a pure-Rust client of the PulseAudio protocol,
  which PipeWire serves (`pipewire-pulse`) and PulseAudio speaks natively. It is what shared mode
  plays through. Chosen against the alternatives by trying them: the `pipewire` crate wraps
  `libpipewire` (25 to 35 crates, `bindgen` and `libclang` at build time, a C library at run time,
  and an API that changed three times in a year), and `libpulse-binding` links the C `libpulse`
  and has no way to pause in its simple API; this one needs no system library and adds five small
  crates (`byteorder`, `enum-primitive-derive`, `futures`, `futures-executor` and itself). It gives
  the stream what phonia needs: pause without losing audio, flush, how much the server holds (for
  the exact position), the list of outputs, a notice when one disappears, and the properties that
  tell PipeWire this is music and to resample well. Its futures need no particular runtime, so
  the audio thread drives them with **`futures-executor`**'s `block_on`.

- **[`anyhow`](https://docs.rs/anyhow)** -- error handling with contextual, human-readable
  messages at every fallible step, from "no saved session, run `phonia login`" to "the device
  doesn't support any lossless integer format for a 24-bit source."

## License

MIT. See the [LICENSE](LICENSE) file.
