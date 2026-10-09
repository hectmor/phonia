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
  if nothing has played yet this session; `Enter` on it does that. Underneath, up to four blocks
  show the library's own favorite albums, favorite artists (#144), playlist folders and favorite
  tracks (six each), every one ending in a "See all (N) →" row that jumps into the matching
  Library tab for the rest. `Enter` on a favorite track there plays just that track, the same as
  it does in Library itself; `Enter` on an album, an artist or a folder opens it nested in Home's
  own stack, exactly like opening one from Library does (`a`/`A` add it whole without opening it,
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
  through, with `j`/`k` to move in the one shown; a track or an album shows its best quality. It
  needs a daemon that has a TIDAL login, and says so where the results would be when it has not.
  On a track, `Enter` plays it (right after the one playing, and starts it); `a` adds it to the
  end of the queue and `A` right after the track playing, and the bar says how many tracks went
  in. On an album, a playlist or an artist, `Enter` opens it. An album or a playlist shows its
  title, artists, year, quality and copyright, then its tracks, numbered as on it, with the ones
  TIDAL will not stream dimmed; `a`/`A` there still add the whole thing. An artist's page shows
  its name, the start of its bio, and three tabs, its top tracks, its albums, and its EPs and
  singles (`[`/`]` switch between them), each with its own count; `a`/`A` on the artist itself add
  its top tracks whole. Inside any of these, `Enter` on a track queues the rest of that list right
  after the track playing and starts at that one (skipping, in the count, any track that could not
  be added); `a`/`A` add just that track. `Enter` on an album from an artist's page opens it in
  turn, on top (the title becomes a full breadcrumb, `Search › Korn › Issues`); `a`/`A` there add
  it whole. `h`, `Left`, `Backspace` or starting a new search closes the view on top, back to what
  opened it. Every list, results and an open view alike, loads more by itself as the cursor nears
  the end of what came, 50 at a time. The **library** section needs no query: the moment it is
  first shown it asks the daemon for it once, and says why not where the lists would be if there is
  no connection yet or the daemon has no TIDAL login. It has four tabs (`[`/`]` switch between
  them, each with its own count): favorite tracks, favorite albums, favorite artists (#144), and
  the playlists made by the account logged in (not ones only followed). A favorite album, artist
  or playlist opens the same way one from a search result does (`Library › Issues`), with the
  same `Enter`/`a`/`A` and closing behaviour; a favorite track is the one exception to how a
  track inside an opened list behaves elsewhere: `Enter` plays just that one track rather than
  queuing the rest of the list from there, since a list of favorites has no natural order and can
  run into the thousands. **`--covers auto|halfblocks|off`** (default `auto`) controls its covers
  (#24): an opened album's or playlist's header, an opened artist's own page, and the **Queue**
  section's own currently playing track show their cover or picture beside their text (the
  artist's tabs stay full width, under both), at whatever size fits (a third of the panel's
  height, square in pixels, never shown at all if the panel is too small or the item has none),
  through a real graphics protocol (Kitty, Sixel, iTerm2) when the
  terminal has one; `halfblocks` (24-bit colour, `▀`/`▄` characters) only when the
  terminal says it has true colour, so `theme.rs`'s own 16-ANSI-colours rule is never broken by
  accident; with neither, no cover shows, rather than one that looks wrong. A cover is fetched
  straight from TIDAL's public image CDN, which needs no TIDAL session, so this is the one thing
  the interface reaches the network for directly (see `docs/DECISIONS.md`, 2026-10-01). The bar's
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
max_quality = "hires"   # hires | lossless | high | low: the best tier to ask for
min_quality = "lossless" # the worst tier phonia will play (default lossless)
session_store = "keyring"  # where the TIDAL login is kept: keyring | file (see below)
report_plays = true     # report finished plays to TIDAL, for Recently Played (default on, #120)

[daemon]
socket = "/run/user/1000/phonia/phoniad.sock"   # default: $XDG_RUNTIME_DIR/phonia/phoniad.sock
verbose = false

[playback]
gapless = true          # join a track to the next one of the same format with no gap
replaygain = "off"      # off | track | album | auto: apply TIDAL's ReplayGain? (default off, #30)
autoplay = false        # fetch more tracks from TIDAL when the queue runs dry? (default off, #33)
```

- **Name the card, not its number.** ALSA numbers cards in the order the kernel finds them, so a USB
  DAC that was `hw:2,0` yesterday is `hw:1,0` today. Its *id* doesn't change: write `hw:DS2,0`
  (or `hw:CARD=DS2,DEV=0`) and phonia turns it into the current number every time it opens the
  device, so a daemon that has been running for days still finds a DAC that was unplugged and
  plugged back in. `phonia devices` lists the cards and the exact text to put in the file; a card
  that isn't there is an error that says which ones are.
- **Quality tiers** go `hires > lossless > high > low` (`high` and `low` are lossy AAC). phonia asks
  TIDAL for `max_quality`; when a track doesn't exist at that tier TIDAL answers with a lower one
  by itself, and phonia plays it and says so (a warning) as long as it is not below
  `min_quality`. Below that the track fails with a message saying so, so playback never turns
  lossy without you agreeing to it. `--quality` and `--min-quality` (on `phoniad` and
  `phonia play`) override the two settings for one run, and `phonia ctl quality <tier>` changes
  the best one while the daemon runs (not written to the file), from the next track opened on: the one playing, and one
  already opened ahead, keep theirs. Over the socket, `track_started` and `status` say what TIDAL
  delivered (`quality` with `requested` and `delivered`, protocol 1.5), and `phonia ctl watch`
  and `status` show it, with what was asked for when it was more. When TIDAL answers a tier with an HTTP 4xx (other than 401,
  408 and 429) or with a manifest that can't be read, phonia asks again one tier lower, down to
  `min_quality`; network errors, timeouts and 5xx are not retried, so an outage never shows up as
  a lower quality. A track that is seeked keeps the tier it started in. phonia
  can't decode AAC yet, so `high` and `low` only make sense once that is added.
- **Precedence** is command line, then the file, then the built-in default. There is no default
  device: with none configured (and no `--device`) phonia refuses to start and says how to set
  one, rather than guess a card and play on the wrong one.
- **Mistakes are errors.** A key it doesn't know (`devise = ...`), a value of the wrong type or a
  word that isn't an option stops it with the file, the line and what was expected, instead of
  being ignored and playing on the wrong card. A missing file just means the defaults; a file you
  named with `--config` (or `PHONIA_CONFIG`) that doesn't exist is an error.
- **Searching TIDAL** (`phonia ctl search <words> [--kind tracks|albums|artists|playlists]...
  [--limit N] [--offset N]`, and the request `search` of protocol 1.6, advertised as the `catalog`
  capability by a daemon that has a TIDAL login): one page of each kind asked for, up to 300 each
  (50 by default), with the total so a client can ask for the next page. Results say when a track
  or an album is available in hi-res, which TIDAL tells apart from plain lossless only in a
  separate field. A kind that was not asked for is absent, not an empty page. The daemon talks to
  TIDAL's API directly, not through `tidlers`' search calls, which fail a whole search if one
  field is missing. A search can take a second or two, so the daemon runs it beside the rest of
  the connection's requests (at most four at once per connection, then it answers
  `rate_limited` at once) instead of holding up the play and pause behind it. Failures come with
  their own error codes: `not_logged_in`, `unavailable` (TIDAL or the network) and
  `rate_limited`.
- **Adding a whole album or playlist** (`phonia ctl queue add album:<id>` or
  `playlist:<uuid>`, on its own, with `--next` or `--at` as for tracks; `ctl search` prints the ids;
  the request `queue_add_from` of protocol 1.6): the daemon lists the tracks from TIDAL itself, a
  hundred at a time, so their titles and lengths come with them instead of being asked about one by
  one, and answers like `queue_add`. At most 1000 tracks at once; a track TIDAL lists but does not
  stream where you are is refused and the rest is added. Like a search it runs beside the
  connection's other requests.
- **Albums and artists** (the requests `album`, `artist`, `tracks` and `albums` of protocol 1.6):
  `album` answers with the album's details (title, artists, release date, whether it is an album,
  an EP or a single, its copyright, its best quality) and the first page of its tracks; `artist`
  answers with everything a view of it shows in one go: its bio as plain text (absent when TIDAL
  has none, and never a reason to fail the rest), its most listened to tracks, its albums, and its
  EPs and singles, a page of each. `tracks` and `albums` give the next pages of those lists.
  A track carries its disc number, so an album of several discs can show where each begins (one
  disc is also numbered, so only more than one is worth a heading). `queue_add_from` also takes
  an artist's top tracks. Like a search, each of these runs beside the connection's other
  requests, and takes one of its four slots however many calls it makes to TIDAL.
- `phonia ctl album <id>` and `phonia ctl artist <id>` print those views: an album's details, copyright and
  tracks numbered as on the album (with a heading per disc when there is more than one), or an artist's bio,
  top tracks, albums, and EPs and singles, each row ending with the id that `queue add` takes (`tidal:<id>`,
  `album:<id>`). `--limit` sets the size of a page (at most 100). When TIDAL has one, a `Cover:`/`Picture:`
  line gives the image's own URL at a size fit for opening by hand, not for a terminal cell.
- **Cover art**: albums, playlists and artists each carry an opaque TIDAL image id (`cover`/`picture`
  in the wire types, since protocol 1.6), not a URL -- `phonia_ipc::image::url(kind, id, min_px)` turns
  one into the actual URL, at the smallest of TIDAL's own fixed sizes for that kind that is at least
  `min_px` (or the largest there is, if none is big enough). The CDN needs no TIDAL login. The TUI
  renders all of these (see `--covers` above), including the now-playing track's own cover
  (`QueueItem`/`Track`'s `cover`) above the Queue section's list -- the last piece of #24.
- **The library** (the requests `library` and `playlists` of protocol 1.6, and `phonia ctl library`):
  favorite tracks and favorite albums (`favorite_tracks`/`favorite_albums`, newest favorited first),
  and the playlists you created yourself, not ones you only follow (`/users/{id}/playlists`, kept
  to your own by comparing each entry's creator to you). `library` answers the first page of all
  three at once, the same way `artist` does for its own lists; `tracks`/`albums`/`playlists` give
  the next pages. `phonia ctl library` prints all three, `--limit` sets the size of a page (at most
  100).
- **Lyrics** (the request `lyrics` of protocol 1.10, advertised as the `lyrics` capability
  alongside `catalog`): synced lines (LRC format) when TIDAL has them, plain text otherwise, pulled
  only for the track playing now and only once a client actually asks -- `phonia ctl lyrics
  [<id>]` (defaults to the current track) for the CLI, and the TUI's own Lyrics section, which asks
  the moment it is opened (or the track changes while it is) and follows the line being sung as it
  plays; `j`/`k` (and the other movement keys) scroll it manually, overriding that until the track
  changes. A local file has no TIDAL id to ask with, so its lyrics are never looked up.
- **Playlist folders** (the request `playlist_folder` of protocol 1.11, under the existing
  `catalog` capability): one page of a folder's own contents, sub-folders and playlists alike,
  from TIDAL's real "My Collection" folder tree (`folder` is the id to open, or the root when
  left out). Unlike the library's own "your playlists" list, a folder's playlists may be ones you
  only follow, not ones you created -- it mirrors TIDAL's own app exactly. Read-only for now:
  creating, renaming or moving a folder is not supported. `phonia ctl folder [<id>]` prints a
  folder's contents, sub-folders first, each row ending with the id to open it (`folder <id>`) or
  to add it to the queue (`playlist <id>`, via `queue add playlist:<id>`). The TUI's own library
  "Your playlists" tab is this same root, browsable exactly like an opened album or artist: Enter
  on a sub-folder nests into it (and nests further from there), Enter on a playlist opens it; `h`
  (or Esc) backs out one level at a time, down to the root.
- **A track's radio** (`CatalogRef::TrackRadio` since protocol 1.12, reusing the existing
  `tracks`/`queue_add_from` requests -- no new request type): tracks TIDAL picks to follow a
  track, seeded by it, with the seed itself always filtered out of the answer (TIDAL's own API
  lists it first). `phonia ctl radio <id>` prints it (`--limit`, at most 100); `queue add
  radio:<id>` adds it whole, the same way `album:<id>`/`playlist:<id>` already do. In the TUI,
  `o` on a track (a search result, a favorite track, or one inside an already-open album,
  playlist, artist page or radio) opens its radio the same way Enter opens an album; opening a
  queue entry's or the now-playing track's radio is a deliberate follow-up, not covered yet.
- **Autoplay** (`[playback] autoplay`, default off; `Request::SetAutoplay`/`Queue.autoplay` since
  protocol 1.13, capability `autoplay` advertised alongside `catalog`): `phonia ctl autoplay
  [on|off]` (no argument flips it). With repeat off, once the last entry in the queue starts,
  phonia fetches its TIDAL radio and appends about 10 tracks on its own, so playback never just
  stops -- the engine's existing gapless prefetch picks them up the same as any other queued
  track. Each refill re-seeds from the newest track added, so the "station" drifts naturally as
  it plays on; a local file has no radio of its own, so it falls back to the last TIDAL track
  that played. `Repeat::One`/`Repeat::All` never trigger it. `O` in the TUI flips it on or off,
  shown next to shuffle/repeat in the bar whenever it is on.
- **Favorite artists** (the request `artists` of protocol 1.14, under the existing `catalog`
  capability): the logged-in user's favorite artists, newest favorited first, the same
  `/users/{id}/favorites/artists` shape favorite tracks and albums already use. No standalone
  `ctl` command (favorite albums and tracks have none either, beyond `library`); shown as a
  fourth tab in the TUI's Library section and a block on Home (#144), Enter opening the artist's
  page, `a`/`A` adding its top tracks whole.
- **Recently played** (#144; the request `recently_played` of protocol 1.15, its own
  `recently_played` capability -- not gated on `catalog`, since it needs no TIDAL call and covers
  local files too): a log the daemon keeps itself, most recent first, deduplicated by source (
  playing something again moves it to the front with a fresh time rather than listing it twice),
  capped at 50. A track counts as played once actually heard for 30s (TIDAL's own rule for its
  own Recently Played), or, if shorter, once it plays to the end. `phonia ctl recent` prints it.
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
change the contract without notice. See `docs/DECISIONS.md` for the full reasoning, including
what is still provisional about it (#120).

A play counts once it has actually been heard for 30 real seconds (paused time doesn't count),
whatever reason it ends for -- completed, skipped or failed -- and however long the track is;
every play is reported as a plain track, not attributed to the album or playlist it was queued
from (a seam left for a later issue). Only `phoniad` sends these, through the same TIDAL login it
already uses for everything else.

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

## Project status

Phase-by-phase status (what's done, what's in progress, what's next) lives in
[`ROADMAP.md`](ROADMAP.md), kept current as work lands, rather than here,
where it would go stale the moment this file wasn't the thing being edited
alongside a merge (which is exactly what happened to this section before it
was replaced by a pointer). [`docs/DECISIONS.md`](docs/DECISIONS.md) is the
companion record of *why* things were built the way they were.

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

- **`phonia-tui`** (a crate of this workspace) -- the terminal interface. Like `phonia-ipc` it does
  not depend on `phonia-core`, so it builds without ALSA, zbus or a keyring; it talks to the daemon
  through `phonia-ipc` only, and how it connects is a parameter, so the reconnection is tested
  against a scripted daemon over an in-memory pipe with `tokio`'s paused clock. It is an Elm-style
  loop: a pure `update(state, message)` and a pure `view(state)`, tested with plain values and a
  test backend, and only `run` touches the terminal. All colours live in one `theme` module and are
  the terminal's own sixteen, so it follows the user's palette; with `NO_COLOR` set it uses none.
  Its one exception to having no network of its own: fetching a cover (#24) straight from TIDAL's
  public image CDN, which needs no TIDAL session, so it does not have to go through the daemon
  (see `docs/DECISIONS.md`, 2026-10-01).

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
  messages at every fallible step, from "no saved session, run `phonia login`" to "hw:0,0 (DS2)
  cannot play 352800 Hz natively; for 24-bit audio it can do 44100, 48000, ... Hz" (#25).

## Development

CI (`.github/workflows/ci.yml`) runs on every push and pull request to `develop` and `main`:
formatting, clippy with warnings as errors, build and tests, and the D-Bus tests against a private
bus. The same checks locally:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Building needs the ALSA development files (`libasound2-dev` on Debian and Ubuntu, `alsa-lib` on
Arch, `alsa-lib-devel` on Fedora) and `pkg-config`. Everything else is Rust.

Tests that need something the CI machine does not have are marked `#[ignore]` and never run by
plain `cargo test`:

- `cargo test -p phonia-core -- --ignored output::dbus:: auth::secret_service::` -- the device
  reservation and the keyring store, against a private `dbus-daemon` the tests start themselves
  (needs only that binary; also run by CI).
- `cargo test -p phonia-core -- --ignored shared::pulse` -- shared mode against the real sound
  server, using a null sink they create and remove (needs PipeWire or PulseAudio, `pactl`, `parec`).
- `cargo test -p phonia-core -- --ignored a_tidal_dash_track` -- streams a whole HiRes track from
  TIDAL (about 90 MB, a minute, no sound) and checks that it decodes to exactly the number of
  frames its manifest declares, which gapless playback relies on. Needs your TIDAL login;
  `TIDAL_TRACK=<id>` picks another track.
- The tests that open a sound card (`output::alsa`) are ignored too, and are run by hand with the
  DAC connected. **Do not run `cargo test --workspace -- --include-ignored`**: it runs those.

## Working process

This project is meant to be picked up from the repository alone — by its owner on a different
machine, or by a model other than whichever one wrote a given part of it — without depending on
any single assistant's own memory of past sessions. That is what this section, `ROADMAP.md` and
`docs/DECISIONS.md` are for together: the roadmap is the current *what*, the decisions log is the
*why*, and this section is the *how*, so an audit can start here and check the other two against
the issue tracker and the git history.

**The people and the process.** One person (@hectmor) directs the project and makes every product
and architectural call; an AI pair does the planning and the implementing. A harder design
question (a new subsystem, a protocol change, a UI decision with real trade-offs) is planned first
by a more capable model, presented as a short written plan with the open decisions called out
explicitly, and only implemented once the person has approved it; a well-scoped, already-decided
piece of work goes straight to implementation. Nothing is committed, pushed, or opened as a pull
request without the person's explicit go-ahead in that session — approval for one piece of work
does not carry over to the next.

**Branching and pull requests.** Every issue (or a part of one, when it is split into several) gets
its own branch off `develop`, never off another open feature branch (no stacking): a stacked branch
can't be merged independently, and a base branch that gets deleted before retargeting orphans it.
Pull requests target `develop`, not `main`. Each one is assigned to the project owner, carries the
milestone of the issue it belongs to, and an `enhancement` (or `bug`/`documentation`) label — GitHub's
own filtering is meant to stay a second, independent source of truth alongside this file. Commits
and pull request descriptions carry no `Co-Authored-By` trailer and no "Generated with"/session-link
footer, by the project owner's explicit standing instruction. Issues are closed by hand after their
pull requests merge (merging to `develop` does not auto-close anything on `main`), so an issue still
open on GitHub does not necessarily mean the work isn't done — check its pull requests.

Pull requests are also meant to be added to a GitHub Project board named "phonia"; as of this
writing that step is blocked because the `gh` CLI token in use lacks the `project`/`read:project`
scope (`gh auth refresh -s project` would fix it) — worth doing once, by the project owner, since a
CLI login can't grant itself a new scope.

**Testing discipline.** `cargo test --workspace` must stay safe to run anywhere, including CI: it
never touches real hardware or a real account. Tests that need something a plain CI machine doesn't
have (a real sound card, the person's real TIDAL login, a real D-Bus session/PulseAudio server) are
marked `#[ignore]` and run by hand, deliberately, as documented above — in particular, **never**
`cargo test --workspace -- --include-ignored`, which would open the real sound card. Anything that
plays audio to verify a change is checked through a silent file, a PipeWire null sink, or an ALSA
loopback (`snd-aloop`), never through real speakers unless the person explicitly asks to hear it.

**Keeping this current.** `ROADMAP.md` is updated in the same session a part of an issue merges, not
as a later cleanup pass; `docs/DECISIONS.md` gets a new, dated entry for a decision worth recording
at the time it's made, never edited afterwards to hide that something changed — a decision that gets
revisited gets a new entry saying so.

## License

MIT. See the [LICENSE](LICENSE) file.
