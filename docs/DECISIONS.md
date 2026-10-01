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

## 2026-09-30 — The library's TUI section gets its own stack, not a shared one

Opening a favorite album or a playlist needed the exact same "push a view,
ask for its tracks" machinery the album and artist views from #20 already
have (`browse::Stack`, `browse::View`), and it was already written as a
reusable, section-agnostic type — nothing about it named search. So rather
than have the library reuse `search_views` (which would tangle two unrelated
things: closing an album opened from a search result would have also had to
know not to disturb one opened from the library, and vice versa), the TUI
state gets a second, independent `library_views: Stack`, and every place
that used to hardcode `state.search_views` now asks a small helper for
"whichever stack the current section owns," `search_views` or
`library_views`. A response to a request still finds its view by checking
both stacks by serial, regardless of which section the person has since
moved to, the same principle the serial/generation staleness scheme
elsewhere already follows.

Building this also surfaced a real, pre-existing bug, unrelated to the
library itself: moving the cursor inside an already-opened album, playlist
or artist page (with `j`/`k`/`gg`/`G`, away from the point where the next
page gets fetched) never told the render loop to repaint, because the
before/after check `apply()` uses to decide whether to redraw did not look
at the cursor of whatever was open on top of the stack — only at which view
was open, not where in it. The same check now also compares that cursor
(wrapped in a small `Snapshot` struct in place of the ad hoc tuple it grew
out of), fixed for the library and for search alike; a regression test
against the search-side case (opening an album, moving within it) is what
caught it, since it is the code path both sections now share. (#98)

## 2026-10-01 — Covers: the TUI fetches and decodes them itself, from TIDAL's public CDN

Issue #24 (the last of Phase 2) needed a real architectural call before any
code: does the daemon hand the TUI an image's bytes, or just enough to get
them itself? Opus's plan, approved as written:

- **TIDAL's image CDN needs no login**, confirmed directly: a plain,
  unauthenticated GET to `https://resources.tidal.com/images/<id with
  dashes turned into slashes>/<size>x<size>.jpg` serves a real cover or
  picture. Only a handful of fixed sizes exist per kind of artwork (not an
  arbitrary one); asking for any other is a 404/403.
- **The daemon's only job is the id**, not a URL and not bytes: `Album.cover`,
  `Artist.picture`, `Playlist.cover` (from TIDAL's `squareImage`, the
  rectangular `image` is not used) ride along on the wire types that already
  exist, as plain `Option<String>` ids, additive under protocol 1.6 like
  everything since #19.
- **`phonia_ipc::image::url(kind, id, min_px)`** turns an id into the actual
  URL, picking the smallest of that kind's fixed sizes that is at least
  `min_px` (or the largest there is, if none is big enough) — a pure
  function any client can call once it knows what size it needs.
- **The TUI fetches and decodes the JPEG itself**, rather than the daemon
  proxying the bytes over the socket. This refines, not reverses, the #19
  decision that the catalog is served by the daemon: that decision was
  about owning the *TIDAL session*, so a client never needs one of its own;
  the image CDN needs no session at all. Meanwhile encoding a cover for a
  terminal's specific graphics protocol depends on that terminal's cell
  pixel size, which only the TUI knows — so the `image`/`ratatui-image`
  dependency has to live there regardless of who fetches the bytes, and a
  daemon proxy would only add cost (base64 JPEG inside a protocol that has
  never carried binary payloads, a second network hop, a cache in the
  daemon) for no real gain. A future MPRIS bridge wants a URL
  (`mpris:artUrl`) anyway, not bytes, which the id-on-the-wire shape gives
  for free.
- **True color stays a separate follow-up, not part of #24.** An image
  rendered through a real graphics protocol brings its own pixels
  regardless of the interface's own palette; only the "halfblocks" fallback
  (no Kitty/Sixel/iTerm2, used only when `COLORTERM` says true color is
  available) touches that question at all, and only inside the image's own
  cells. Taking the TUI's *own* accent colors from a cover (TIDAL's
  `vibrantColor`) changes how the whole interface looks for every album —
  a product decision of its own, not a dependency of showing a picture.

Verified against the real CDN with this account's own real ids (an album
cover and an artist picture, both confirmed via the earlier catalog work):
every size `phonia_ipc::image::Kind` claims for `AlbumCover` and
`ArtistPicture` is genuinely served, and one size past the largest claimed
genuinely is not — the size lists are not guesses. `PlaylistCover`'s sizes
(TIDAL's other clients' own choices) are not yet confirmed the same way,
for lack of a playlist on this account. (#99, #100)

## 2026-10-01 — Covers: the two new dependencies, pared down, and a PR split adjustment

`ratatui-image` and `image`, added to `phonia-tui` with their defaults
turned off:

- `ratatui-image`'s own default features (`chafa-dyn`, on by default) link
  the C library `libchafa` through `pkg-config` — a system dependency this
  project otherwise has none of on the terminal side, and not needed for
  any of the four protocols actually drawn with (Kitty, Sixel, iTerm2,
  halfblocks). Enabled instead: only `crossterm`, the backend this project
  already uses.
- `image`'s own defaults bring in several decoders this project has no use
  for (AVIF's `ravif`/`rav1e` by far the heaviest), since TIDAL serves
  covers as JPEG only. Enabled instead: only `jpeg`. (`ratatui-image`'s own
  dependency on `image` separately asks for `png`, unconditionally, for its
  iTerm2 encoder; Cargo's feature unification means `png` and `flate2` are
  pulled in by that regardless of what `phonia-tui` itself asks for.)

A clean build of `phonia-tui` with both new dependencies, from nothing:
about 24 s wall clock on this machine. No ALSA, zbus or keyring dependency
was pulled in — `cargo tree -p phonia-tui` confirms the TUI's own dependency
boundary (#19, 2026-09-27) still holds; the one new thing it reaches on its
own is the network, for the CDN, nothing else.

One deliberate split adjustment from the approved plan: part 3 (this one)
ships the two dependencies and the fetch/decode/encode pipeline
(`graphics.rs`, `covers.rs`), fully unit-tested on their own, but does
**not** wire them into the run loop (the `--covers` flag, querying the
terminal at startup, the channel a fetch answers on) the way the plan's own
PR 3 described. There is nothing yet to ask either one for a cover — that
is part 4 — so wiring them to nothing to exercise live would have been
speculative, churn to redo once part 4 actually needed something different.
Folding that wiring into part 4, its first real caller, keeps every PR
compiling and testing green with no unused plumbing, the same discipline
already applied throughout #19–#21.
