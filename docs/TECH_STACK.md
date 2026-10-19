# Tech stack

Why every dependency in phonia is there, including the alternatives that were
tried and rejected. The dated record of the decisions themselves is
[DECISIONS.md](DECISIONS.md); how to build and test what they add up to is
[CONTRIBUTING.md](../CONTRIBUTING.md). Start at the [README](../README.md).

Every dependency here exists to protect one single guarantee end to end: **no bit gets touched
in a way that isn't reversible**. This file explains what each piece of the stack does and,
more importantly, *why* it was chosen.

- **Rust** -- a systems language with no garbage collector and no hidden allocations was a hard
  requirement for the audio output path: a GC pause or an unexpected allocation on the hot path
  can produce an audible glitch. Rust also makes the "never silently convert through a float"
  invariant (see `symphonia` and `alsa` below) something the type system helps enforce, not just
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
  depends only on `serde` and `tokio`, not on `phonia-core`, so a terminal UI or an agent server
  can talk to the daemon without building ALSA or the decoders (MPRIS itself lives inside
  `phoniad` -- see "MPRIS" in [AUDIO.md](AUDIO.md) -- not as a separate bridge over this protocol). The wire types are
  its own (durations in milliseconds, no internal references leaking out) instead of `serde` derives
  on the engine's types, so the format can outlive refactors. The framing is a few lines over
  `tokio` rather than a codec crate.

- **`phonia-tui`** (a crate of this workspace) -- the terminal interface. Like `phonia-ipc` it does
  not depend on `phonia-core`, so it builds without ALSA, zbus or a keyring; it talks to the daemon
  through `phonia-ipc` only, and how it connects is a parameter, so the reconnection is tested
  against a scripted daemon over an in-memory pipe with `tokio`'s paused clock. It is an Elm-style
  loop: a pure `update(state, message)` and a pure `view(state)`, tested with plain values and a
  test backend, and only `run` touches the terminal. All colours live in one `theme` module and are
  the terminal's own sixteen, so it follows the user's palette; with `NO_COLOR` set it uses none.
  Its one exception to having no network of its own: fetching a cover (#24) straight from TIDAL's
  public image CDN, which needs no TIDAL session, so it does not have to go through the daemon
  (see [DECISIONS.md](DECISIONS.md), 2026-10-01).

- **[`ratatui`](https://docs.rs/ratatui)** -- draws the terminal interface: layout, widgets and a
  `TestBackend` that lets the screens be tested without a terminal. Chosen because it is the
  maintained standard for Rust TUIs and the one `ratatui-image` (covers) builds on; it installs the
  panic hook that gives the terminal back.

- **[`crossterm`](https://docs.rs/crossterm)** -- reads the keyboard and the terminal's size
  changes. Its `event-stream` feature gives an async stream that goes in the same `select!` as the
  daemon's events. It is the backend `ratatui` uses, in the same version, so it adds nothing new.

- **[`ratatui-image`](https://docs.rs/ratatui-image)** -- draws a cover through whichever graphics
  protocol the terminal actually supports (Kitty, Sixel, iTerm2), detected once at startup
  (`Picker::from_query_stdio`, called right after entering the alternate screen and before reading
  terminal events, per its own requirement), with a "halfblocks" fallback (half-height block
  characters in 24-bit colour) when none is there. Pulled in with `default-features = false,
  features = ["crossterm"]`: its defaults turn on `chafa-dyn`, which links the C library `libchafa`
  through `pkg-config` -- a system dependency this project otherwise has none of on the terminal
  side, and not needed for Kitty, Sixel, iTerm2 or the halfblocks fallback, the four protocols this
  project actually draws with.

- **[`image`](https://docs.rs/image)** -- decodes the JPEG TIDAL's image CDN serves covers as.
  Pulled in with `default-features = false, features = ["jpeg"]`: its defaults also bring in, among
  others, AVIF decoding (`ravif`/`rav1e`, by far the heaviest of them) that this project has no use
  for, since TIDAL serves covers as JPEG only. (`ratatui-image` itself still turns on `png`, for its
  iTerm2 encoder; that comes along either way.)

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
  `playbackinfo` response type and `DashManifest`, however, were incomplete from the start
  (see `crates/phonia-core/src/tidal.rs` and `dash.rs`), so those two pieces are implemented directly against TIDAL's
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
  messages at every fallible step, from "no saved session, run `phonia login`" to "hw:0,0 (DS2)
  cannot play 352800 Hz natively; for 24-bit audio it can do 44100, 48000, ... Hz" (#25).
