# Configuration

The complete `config.toml` reference, what every setting does, the TUI's own
theme file, and where the TIDAL login is kept. The commands these settings
affect are in [USAGE.md](USAGE.md); the audio behaviour they select is in
[AUDIO.md](AUDIO.md). Start at the [README](../README.md).

## The config file

The settings live in `~/.config/phonia/config.toml` (`$XDG_CONFIG_HOME/phonia/`). Nothing is
required to exist:

```toml
[output]
device = "hw:DS2,0"     # a sound card: `phonia devices` lists them. "auto" = the first USB card.
mode = "auto"           # auto (default, #157): the DAC if plugged in, else shared. exclusive/shared pin one
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
mpris = true            # register an MPRIS2 D-Bus service for media keys and widgets (default on, #34)

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
- **Precedence** is command line, then the file, then the built-in default. The default mode,
  `auto`, picks the device for you: a USB sound card with playback (the DAC) if one is plugged
  in, else the desktop's default output. Pinning `mode = "exclusive"` with no device configured
  (and no `--device`) still refuses to start and says how to set one, rather than guess a card
  and play on the wrong one — only `auto`'s own detection ever falls back by itself.
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
  renders all of these (see `--covers` in [USAGE.md](USAGE.md)), including the now-playing track's own cover
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
  Saved to `$XDG_STATE_HOME/phonia/recently_played.json` (atomically, a temp file then a rename)
  whenever it changes, and loaded back the next time `phoniad` starts, so it survives a restart;
  a missing or unreadable file is simply an empty log, never a reason `phoniad` fails to start.
- **Changing favorites** (#196; the request `set_favorite` of protocol 1.19, under the existing
  `catalog` capability): adds or removes a track, album or artist from the logged-in user's TIDAL
  favorites -- `POST /users/{id}/favorites/{tracks|albums|artists}` to add, `DELETE
  /users/{id}/favorites/{tracks|albums|artists}/{id}` to remove. No standalone `ctl` command yet.
  In the TUI: `fas`/`far`/`faa` add the track/album/artist under the cursor, `fds`/`fdr`/`fda`
  remove it, wherever one is shown (a search result, a Library tab, a Home block, an opened view
  or the queue). Following or unfollowing a playlist (`fal`/`fdl`) answers `CatalogError::Invalid`
  for now -- no endpoint for it has been verified yet (see `docs/DECISIONS.md`, 2026-10-25).
- `phonia config path` prints which file is used, and `phonia config show` prints every setting
  with where its value comes from (the file or the default). `--config <file>` (or the
  `PHONIA_CONFIG` environment variable) selects another file, also for `phoniad`.
- The daemon reads the file once, at startup: restart it to apply a change. The file holds no
  secrets (the TIDAL session is kept apart, see below).

## The theme file

The TUI's colours live in their own file, `~/.config/phonia/theme.toml`, separate from
`config.toml` — `phonia-tui` reads it directly and never touches `config.toml` or anything else
in `phonia-core`. There is one key:

```toml
palette = "nord"   # "gruvbox", "dracula" or "nord"
```

With no file, or one that doesn't set `palette`, nothing changes: the TUI still uses only the
terminal's own 16 colours, `NO_COLOR`-aware, exactly as always. Naming a palette uses that
scheme's real colours instead, on dark-background foregrounds only — it never paints the frame's
background, and it takes priority over `NO_COLOR` (sent as 24-bit colour regardless of whether
the terminal says it supports true colour). An unknown palette name is a startup error that lists
the three real ones. Like `config.toml`, this is read once at startup: restart `phonia tui` to
apply a change.

## Where the TIDAL session is kept

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
