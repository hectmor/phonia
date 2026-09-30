# Decisions

Why phonia is built the way it is, in the order the decisions were made. Each
entry is short on purpose: enough for an auditor — human or model — to see
what was chosen, what the alternative was, and why, without reading the pull
request that implemented it. `ROADMAP.md` is the companion record of *what*
is done; this file is only *why*.

New entries go at the bottom, dated, as decisions are made — not batched up
afterwards. A decision that gets revisited gets a new entry saying so; old
entries are never edited to hide that something changed.

## 2026-09-19 — One dedicated audio thread, driven by messages

The playback engine runs on its own OS thread, not inside the async runtime:
ALSA's blocking writes and exact timing needs do not mix well with an
executor that is also doing network I/O. The engine is a small state machine
fed by a channel, and position is reported as *heard* position (frames
written minus the device's own reported delay), not frames handed to it,
so a paused or about-to-underrun stream is not misreported as further along
than it is. (#10)

## 2026-09-21 — The queue lives in memory, not on disk

Shuffle and repeat are computed over an in-memory list built from what was
added; nothing is persisted across a restart of the daemon. Persistence is
real work (what to do with a TIDAL track whose availability changed, a local
file that moved) and is its own later issue (#37), not bundled into getting
a queue working at all. (#11)

## 2026-09-23 — IPC: newline-delimited JSON over a Unix socket, `major.minor` versioned

Simple enough to speak by hand with `socat` for debugging, and to give every
client (the CLI, the TUI, anything else) the same protocol crate
(`phonia-ipc`) without any of them depending on ALSA or the TIDAL session.
Versioning is `major.minor`; a minor version only ever *adds* fields,
requests, events or enum variants (never removes or renames), and unknown
ones deserialize to an explicit "unknown" rather than failing, so an older
client survives talking to a newer daemon and vice versa. As of this writing
the protocol has reached 1.6 without a single tagged release of phonia
existing yet — see the note under 2026-09-27 below. (#12)

## 2026-09-23 — The DAC is reserved, and given back, not held hostage

Exclusive (bit-perfect) mode asks for the card with the desktop's own
`org.freedesktop.ReserveDevice1` D-Bus convention, the same one PipeWire and
PulseAudio already use to hand cards to each other — so phonia is a well-behaved
citizen of the same negotiation, not a special case. It gives the device back
on pause (after a configurable delay), on an explicit release, and when
something else asks for it; it takes it again on resume. (#13)

## 2026-09-23 — The TIDAL session lives in the desktop keyring by default

Refresh tokens are secrets; the desktop's own Secret Service (through `zbus`)
is where a Linux desktop already expects them to live, with a file-based
store as an explicit, documented fallback for a headless daemon with no
keyring. Migration from an older plain-file store is automatic and one-way.
(#15)

## 2026-09-24 — Shared mode is explicitly *not* bit-perfect, and says so

Exclusive mode (a card phonia owns outright) is bit-perfect by construction;
shared mode (through PipeWire's PulseAudio-compatible protocol, via the
`pulseaudio` crate rather than linking `libpipewire` or `libpulse`) mixes and
can resample, and reaches anything the desktop can reach, Bluetooth included.
The interface never blurs this: a route is always labelled with which one it
is, and the README documents the trade-off plainly rather than only in code
comments. (#14)

## 2026-09-23 — A device is named by its card *id*, not its number

ALSA numbers cards in discovery order, which is not stable across a reboot or
a USB replug. The config file and `--device` take `hw:DS2,0`-style names
(`phonia devices` prints them), resolved to the current number each time a
card is opened, so a long-running daemon keeps working after the DAC is
unplugged and plugged back in under a different number. (#16)

## 2026-09-26 — Gapless: prefetch and an exact-sample join, not a reopen

The next track is opened ahead of time (a configurable lead) while the
current one is still playing, and the join at end-of-track writes the next
track's first sample immediately after the last one, with no drain, no flush
and no reopen in the common case (same format, no output change). This was
verified bit-exact against a real TIDAL album over an ALSA loopback
(`snd-aloop`), not only with synthetic fixtures. A format change, an output
that changed, or a track that failed to prefetch each fall back to the
ordinary drain-and-reopen path; none of that is gapless, and it is not meant
to be. (#27)

## 2026-09-26 — Quality is tiered, floored, and falls back on its own

Tiers are `hires > lossless > high > low`; `tidal.min_quality` (default
`lossless`) is a floor the daemon will not play below without being told to.
TIDAL's own silent downgrade (answering a lower tier than asked) is accepted
above the floor and reported; an HTTP 4xx or an unreadable manifest is
retried one tier down (never on a network error, a timeout, or a 5xx, so an
outage is never mistaken for a quality problem). The best tier can be changed
while the daemon runs, over IPC, without a restart. AAC (`high`/`low`) is not
decoded yet — that is an explicit, optional, not-yet-started part of the same
issue, since nothing so far has needed it. (#29)

## 2026-09-26 — The TUI depends only on `phonia-ipc`, never on `phonia-core`

The terminal interface is a client like any other: it never touches ALSA,
`zbus`, or the TIDAL session directly, only the wire protocol. This is
enforced by the crate graph, not just convention (`cargo tree -p phonia-tui`
has no ALSA in it), so the TUI builds and its logic is tested without any
audio hardware, a D-Bus daemon, or a network connection. (#17, #18)

## 2026-09-26 — The TUI is a small Elm: pure `update`, pure `view`, one loop at the edge

`app::update(state, msg) -> Effects` and `view::draw(state, frame)` are both
pure functions, tested with plain values and ratatui's `TestBackend`; only
the outermost run loop touches the real terminal or a real socket. Requests
that need their answer carry a `Tag`; a `generation` (search) or a per-view
`serial` (an opened album, playlist or artist page) tells a late answer to
something no longer current apart from one that still matters, so a slow
network reply can never clobber a newer screen. (#18, #19, #20)

## 2026-09-26 — Colors: the terminal's own 16, true color deferred on purpose

Every color the TUI uses lives in one `theme.rs` module and is one of the
terminal's own 16 ANSI colors, so the interface follows whatever palette the
person already has (solarized, gruvbox, the default) instead of imposing one,
and `NO_COLOR` is honored. True color (and pulling colors from an album's
cover) is an explicit, deferred option for the *end* of the TUI work, once
covers (#24) make it worth having — a decision the project owner made
explicitly, not an oversight.

## 2026-09-27 — TIDAL browsing (search, an album, an artist, the library) is served by the daemon

The alternative was letting the TUI process talk to TIDAL directly, since
`tidlers` (the TIDAL client library) already exists as a dependency. That was
rejected: it would have pulled `phonia-core` (and so ALSA, `zbus`, the
keyring) into the TUI, given the interface a second, independent holder of
the TIDAL session and its token refresh, and meant any *other* future client
(an MPRIS bridge, an agent) would have to reimplement browsing on its own.
Instead the daemon owns the catalog behind new IPC requests, and the wire
types are the protocol's own (`TrackSummary`, `AlbumSummary`, ...), not
`tidlers`' types serialized directly — so `tidlers` (or its bugs) can change
without changing what a client sees. A related, narrower decision: those
requests bypass `tidlers`' own search, favorites and listing calls entirely
and speak TIDAL's v1 API with plain HTTP through the same authenticated
session, because `tidlers`' types make fields required that TIDAL sometimes
omits (failing a whole search over one missing field), and its favorites
calls carry a parameter-name typo (`ofset`) that breaks paging past the first
page. Only an item's id is ever required on this side; an item that cannot be
read is left out of its page instead of failing it. (#19, #20, #21)

## 2026-09-27 — Protocol versioning has not needed a major bump, or even much minor discipline, yet

Every addition since 1.3 (gapless, quality, the whole catalog) has been
additive under whatever minor version was current, on the reasoning that no
version of phonia has ever been tagged or released, so there is no real
consumer yet to break. This is a conscious deferral, not an oversight: real
release discipline (a changelog, a policy for when 1.x becomes 2.0) is
expected to be worked out before Phase 5 packaging (#43, #44), not before
then.

## 2026-09-28 — Adding or removing TIDAL favorites is out of scope for #21

Issue #21 asks to *show* the library (favorite tracks and albums, the
person's own playlists); changing what is favorited is left for a later,
separate issue, once showing the library has landed and the shape of that
interaction can be judged against how it is actually used.

## 2026-09-30 — The library's remaining shape: protocol, ownership, and ordering

The rest of Opus's #21 plan, approved together with the item above:

- **Protocol.** No new capability: the library lives under the existing
  `catalog` one. `CatalogRef::FavoriteTracks` and `AlbumListRef::FavoriteAlbums`
  extend the existing paged `tracks`/`albums` requests (and `queue_add_from`)
  rather than inventing new ones, since an artist's top tracks and albums
  already page the exact same way. A playlist listing had no request to
  extend, so it gets its own: a new `PlaylistListRef` (currently just `Mine`)
  behind `Request::Playlists`/`Payload::Playlists`. `Request::Library` answers
  the first page of all three at once, the same shape `Request::Artist`
  already uses for its bio, top tracks, albums and singles together.
- **"Your playlists" means only the ones you created**, not ones you follow.
  `/users/{id}/playlists` is asked for the same as before, but every entry's
  creator id is compared to the logged-in user's own; a playlist with a
  different creator (or none) is left out. (#95)
- **Favorites are asked for newest first** (`order=DATE&orderDirection=DESC`),
  since that is what someone paging their own library actually wants to see
  first, and TIDAL's API supports the parameter for this endpoint.
- **Enter on a favorite track plays just that track**, not the whole list
  queued at once — unlike an album or a playlist, a favorites list has no
  natural queue order and can run into the thousands, so queuing it whole by
  accident would be a much easier way to end up with an unwanted, huge queue.
  (TUI-side; lands with #21's fourth pull request.)

Verified against the real TIDAL API with this account's login: favorite
albums came back correctly (2 of 2); this account has no favorite tracks or
own playlists right now, and `/users/{id}/playlists` was confirmed to answer
a genuine paged `{"totalNumberOfItems":0,...}` — a real field `tidlers`'s
own `UserPlaylistsResponse` model doesn't capture at all, so this would have
gone unnoticed without checking the raw response directly. (#95, #96)
