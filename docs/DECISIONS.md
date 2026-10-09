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

## 2026-10-01 — The first cover: an opened album's or playlist's header, and how it is laid out

The first visible part of #24: an opened album's or playlist's header shows
its cover beside the title, copyright and track count that were already
there. A few decisions this needed:

- **The layout never jumps.** Whether a cover's space is reserved depends
  only on things known the instant the header is about to draw — a picker
  exists (covers are not off, and one was detected), the item actually has
  a cover id, and the header's own area is big enough — never on whether
  the image has actually finished fetching. A cover still loading, or one
  that failed, leaves that same reserved rectangle blank; it is filled in,
  without reflowing anything else, the moment it is ready. Getting this
  gate exactly right mattered: an early version reserved the space whenever
  a picker existed, regardless of whether the item had a cover id at all,
  which would have shown a blank gap forever next to anything TIDAL has no
  artwork for. A regression test (`with_covers_off_or_no_cover_id_the_layout_is_exactly_as_before`)
  pins the fix: that case must lay out byte-for-byte like covers never
  existed.
- **The size:** a third of the header's own height, clamped to 6–12 rows,
  with the width computed from the terminal's reported font size so the
  image comes out square in pixels, not just in character cells (usually
  about twice as tall as wide). Below a minimum, or with no room left for
  the header's text beside it, no cover shows at all rather than a
  cramped one.
- **`Covers` owns the `Picker`,** not a separate parameter threaded
  alongside it: both are one resource for drawing, so `view::draw` only
  grows the one new parameter it needs. A `None` picker (covers off, or
  none detected) and "nothing to show a cover of" are kept as two different
  reasons for the same visible outcome (no cover), so a future case that
  can tell them apart (a settings line explaining why, say) is free to.
- **The run loop's own cover-fetching** (`--covers`, querying the terminal
  once via `graphics::setup` right after entering the alternate screen,
  and a fetch's result arriving on its own channel) is not modeled as an
  `app::Msg`: a fetch finishing redraws directly, the same reasoning that
  keeps `Covers` itself out of `app::State` (2026-10-01, the part 3 entry
  above). The main panel's own area, needed to decide how big a cover to
  ask for, is approximated from the terminal's raw size rather than
  threading the exact bordered, inner area out of the view layer into the
  run loop: close enough to pick a sane request size, not required to be
  exact, and avoids a second, parallel way to compute layout.

## 2026-10-01 — The second cover: an artist's picture, and generalizing "what has a cover"

An opened artist's page shows its picture the same way an opened album's or
playlist's header shows its cover — same reservation rule, same fallback —
but an artist is not a `Header` (only an album or a playlist is), so
deciding "what cover does the thing on top of the stack have" had to stop
being header-specific: `State::open_header() -> Option<&Header>` became
`State::open_cover() -> Option<(image::Kind, &str)>`, which an `ArtistView`
answers from its own new `picture` field (known from the search result that
opened it, the same way its name already was, and refreshed once the full
artist loads) and a `TrackListView` still answers from its `Header`. The
drawing side (`view/browse.rs`) is unaffected, since it already had the
specific view in hand either way; only `covers::wanted`, which has to work
from `app::State` alone before anything is drawn, needed the more general
shape.

The artist page's tabs row stays full width, under the picture and the
header text alike, rather than squeezed beside the picture the way the
header text is: a navigation row split in two beside an image would read
oddly, where an album's copyright and track count reading next to its own
cover does not.

The album cover's own first draft reserved space whenever a picker existed,
regardless of whether the item had a cover id at all (see the entry just
above) — this one was written to require an actual picture id from the
start, and `an_artist_with_no_picture_or_with_covers_off_lays_out_as_before`
pins that it does.

## 2026-10-01 — The now-playing track's cover reaches the wire, ahead of drawing it

Part 6 of 7 carries a cover id for the *track that is actually playing*,
not just for an opened album, playlist or artist page, all the way from
TIDAL to the wire — with nothing drawn from it yet (that is part 7). Doing
this as its own PR, ahead of any TUI rendering code that uses it, follows
the same "every PR compiles, nothing speculative" rule as folding the run
loop's wiring into part 4 rather than part 3: the data plumbing and the
drawing are two independently reviewable, independently testable changes,
and the plumbing does not need the drawing to exist first to be correct.

The id travels the same path the title and the duration already do:
`SourceInfo` (what describing a source learns before it plays),
`TrackMeta` (what the engine reports once it is loaded or playing), and
`QueueTrack`/`QueueItem` (what the queue remembers about an entry) each
grew a `cover: Option<String>` field, additive under IPC 1.6 like every
other field #24 has added. `TidalOpener::describe` and `TidalOpener::open`
populate it from `tidlers`'s own `Track.album.cover` and
`phonia_core::catalog::AlbumRef.cover` respectively — the same field
`phonia-core`'s own catalog types gained in part 1 (2026-10-01, "Covers:
the TUI fetches and decodes them itself") for album/playlist headers; a
local file always has `None`.
`phoniad`'s `queue_add_from` (adding a whole album or playlist at once,
where the listing already carries each track's album cover) and
`queue_add` (adding tracks one at a time, resolved through `describe`)
both fill it the same way title and duration already were — the
`Accepted` tuple internal to `daemon.rs` grew a matching field rather than
inventing a second shape for "a track on its way into the queue."

One extra wrinkle this part surfaced: `Queue::open_entry` fills in a
loaded track's title and duration from what the queue already knew,
*only if the opener itself did not report one* (`loaded.meta.title =
loaded.meta.title.or(item.track.title)`, and the same for duration) — a
track opened fresh always knows its own cover from TIDAL already, so the
same `.or()` pattern was the natural fit for `cover` too, rather than a
special case.

## 2026-10-01 — The last cover: the now-playing track, above the Queue section's list

Part 7 of 7, the last of #24's plan: the Queue section's own panel shows
the currently playing track's album cover the same way an opened album's
or playlist's header, or an opened artist's page, shows theirs — the same
reservation rule (space is reserved only when a picker exists, the
daemon's last-reported track has a cover id, and the panel is big enough;
never on whether the fetch has actually finished), the same `cover_size`
formula, the same fallback to no cover at all rather than a layout that
could jump.

Fitting this into the existing shape took two small generalizations,
both in favor of reusing what #24 already built rather than parallel
code paths:

- **`app::State::open_cover`**, which already answered "what does the main
  panel's current section want to show a cover of" for the search and
  library sections' stacks, grew a third arm for `Section::Queue`: the
  daemon's last-reported `status.track.cover`, as a `Kind::AlbumCover`
  (it *is* one — a track's own album cover — regardless of whether the
  track came from a search result, the library, or a bare `queue add
  tidal:<id>`). This one function already drives the run loop's own
  proactive fetching (`covers::wanted`, called from `lib.rs` after every
  state update), so the Queue section's cover is fetched the same way,
  with no separate wiring needed there.
- **`covers::track_cover_url`**, a third URL-builder alongside `cover_url`
  (for a `Header`) and `picture_url` (for an artist), taking a raw
  `Option<&str>` cover id the same way `picture_url` does, since "the
  now-playing track" has no header or view type of its own to read a
  `cover()` method from — it is just whatever `phonia_ipc::Status::track`
  says right now.

The drawing itself (`view::draw_queue`, replacing the Queue section's old
inline `(title, lines)` match in `draw_main`) follows `browse.rs`'s own
layout exactly: a reserved rectangle at the top-left for the cover, the
track's name (and its quality tier, when it is streamed from TIDAL) beside
it, and the queue's list filling the rest — restructured, like the search
and library sections already were, so the panel's block is rendered once
in `draw_main` and the inner area is handed down, rather than building the
whole bordered `Paragraph` in one call the way the old Queue-only code
did.

`#24` is now code-complete: all 7 parts merged (#99–#105). True color
stays deliberately out of scope, same as every entry above has said.

## 2026-10-02 — #25/#26: live `HwParams` probing, no cache, refuse-not-resample

Phase 2 closed (tagged `v0.2.0`). Opus planned #25 (DAC capability
detection) and #26 (per-track sample rate switching) together, since a
read-only investigation first found they are two sides of one mechanism,
not two independent features — both issues' GitHub bodies were one-liners,
scoped properly here the same way every Phase 3+ issue is meant to be.

**What already existed, found before any design work started:** the
engine already reopens the sink whenever a track's `SourceSpec` differs
from the one currently open (`start_track`/`open_sink`/`join_next` in
`engine/audio_thread.rs`), tested against `FakeSinkFactory`. So #26's
literal ask ("reopen the PCM when rate/format changes") was mostly already
true. Two real gaps were found instead: `AlsaSink::open` picks a format by
trying a hardcoded priority list against the device and asks for the
track's exact rate on faith, with no idea beforehand whether either will
work, so a mismatch surfaces as a raw ALSA `EINVAL` rather than a clear
refusal (the existing post-hoc "does not support N Hz" read-back check
almost never fires — `HwParams::set_rate(_, ValueOr::Nearest)` actually
requests an exact rate under the hood, so the real failure happens
earlier); and a track whose open failed was never reported to clients as
`TrackEnded { Failed }`, only as a bare `Event::Error`, which is a bug
unrelated to either issue's literal ask, found along the way and fixed in
part 3.

**The design, approved as follows:**

- **Live `HwParams` probing, on the PCM already being opened, decides
  everything** — not parsing `/proc/asound/cardN/stream0` as #25's own
  wording suggests. `stream0` is USB-only (HDA and HDMI cards have none),
  describes the device before kernel quirks are applied, and knows nothing
  about live state (another substream holding the rate, a DAC replugged as
  a different model); live probing is exactly what ALSA will enforce on
  this open, costs on the order of microseconds, and needs no extra
  reservation since the PCM is already open for the real attempt. `stream0`
  is kept as a passive, read-without-opening-the-device extra (part 4,
  `phonia devices`), since it is the only way to show *something* about a
  USB DAC's capabilities without taking it from PipeWire first — it never
  drives a decision.
- **No persisted or cross-open capability cache.** A cache would only help
  an engine pre-check or a closed-device display, and it would go stale in
  exactly the cases live probing handles for free (a replug, a different
  DAC with the same configured id, a rate another substream has locked).
  Probing at daemon startup was also rejected: it would mean reserving the
  DAC from the desktop before anything needs to play, which contradicts
  the 2026-09-23 "given back, not held hostage" reservation decision.
- **No engine-level pre-check either** (a `SinkFactory::check(spec)` a
  prefetched track's readiness could consult before committing to it was
  considered and rejected): the outcome would be identical to refusing
  inside `AlsaSink::open` itself (the current track still plays to its end,
  the next one is still refused), it would need the cache just rejected,
  and it adds a second path that could disagree with the real open. The
  only real gain, an earlier warning, belongs with #28 if anyone wants it.
- **The policy stays bit-perfect-or-refuse in exclusive mode.** Padding a
  source into a wider lossless container (16-bit into `S24_3LE`/`S32LE`,
  which already happened for 24-into-32) is not resampling and is fine;
  generalized here to "any container with at least as many significant
  bits as the source," which also makes a 20-bit FLAC playable on more
  DACs. Changing the *rate*, or truncating *depth*, is not attempted —
  shared mode remains the one deliberate, clearly-labelled exception, and
  is out of scope for both issues entirely (`SharedSinkFactory` already
  lets PipeWire resample to whatever the real sink runs at, and always
  reports itself as not bit-perfect). What improves is the refusal
  *message*: precise, informed by what the device was just found to
  accept, naming the shared-mode escape hatch, instead of a raw ALSA
  errno or a silent wrong-rate guess.
- **A refused track stops playback**, like every other failure today,
  rather than skipping to the next queue entry — skipping would need loop
  guards against `repeat: one` and against a queue where every remaining
  track is unplayable on this DAC, which stopping avoids needing at all.
- **`catalog.rs`'s `bit_perfect: true` for every exclusive card stays as
  it is.** Under refuse-not-resample, an exclusive card genuinely either
  plays bit-perfect or does not play at all, so the flag already describes
  that route honestly; there is no way to measure it passively without
  taking the card from PipeWire, so a doc comment is the only change.
  Separately, the person noted a future product wish for phonia to play on
  any output device, bit-perfect only once a real DAC is actually in use —
  which this project's shared/exclusive split already is; worth revisiting
  if an "automatic output selection" issue is ever filed, but it changes
  nothing here.
- **No IPC changes in #25/#26.** Device capabilities and a structured
  "unsupported format" error code are left for #28 (signal path indicator
  in the TUI), the natural seam: `AlsaSink` will hold its probed
  `Capabilities`, so `report()` can attach them to `SinkReport` once #28
  wants to show them.

**PR split (4 parts, all approved up front, folded into the plan rather
than discovered part by part the way #24's was — the investigation this
time was thorough enough that no mid-plan deviation was expected):** part
1, `output/caps.rs` (pure types: `SampleFormat`, `Capabilities`,
`probe_with`) plus a real `probe` in `output/alsa.rs` and `probe-device`
rewritten on top of it (also fixing it to disable resampling before
probing, which it never did before — on a `plughw:`/`default` device it
was reporting "yes" to everything, since the plug layer was silently
converting under it); no playback behavior change. Part 2: `caps::choose`
and a typed `Unsupported` error, `AlsaSink::open` rewritten to probe
before committing hw_params and refuse precisely when nothing fits (the
old hardcoded `pick_format` retired). Part 3: the engine emits
`TrackEnded { Failed }` for a track whose open failed (the bug above).
Part 4: `phonia devices` gains a passive per-USB-card line parsed from
`stream0`.

**Verification:** the development machine's Fosi Audio DS2 accepts every
rate TIDAL serves in `S16_LE`, `S24_3LE` and `S32_LE` (not `S24_LE`) —
confirmed two ways, both while the DAC was freed from PipeWire by hand
(`pactl set-card-profile ... off`, restored after): the rewritten
`phonia probe-device` CLI, and a new `#[ignore]`d test
(`hardware_capabilities`, following the existing `hardware_pause_resume_*`
test's own pattern exactly — `PHONIA_TEST_DEVICE`, run by hand, never in
CI). Because this DAC accepts everything TIDAL can send it, the *refusal*
path cannot be exercised against real hardware and is covered by unit
tests (`caps.rs`'s own suite, joint rate/format constraints modelled with
fake closures) and, in part 3, engine tests against a refusing fake sink.

## 2026-10-02 — #25 part 2: `AlsaSink::open` probes before it commits, and refuses precisely

`caps::choose(source, &capabilities) -> Result<SampleFormat, Missing>`
picks the tightest lossless container a device actually offers at a
track's exact rate, replacing the old `pick_format`, which just tried a
hardcoded priority list against the device with no idea beforehand
whether any of it would work. `Missing` is one of `Depth` (the source's
own bit depth is not 1–32 bits: not a device limitation at all),
`Channels`, `Rate`, or `DepthAtRate { offered }` (the rate is accepted,
but every format offered there is narrower than the source needs) —
checked in that order, since an invalid depth is wrong regardless of any
device, and a depth that doesn't fit at an otherwise-accepted rate is the
most specific thing to say. A typed `Unsupported` error (device, source,
`Missing`, and the `Capabilities` that produced the verdict) carries
enough to format a precise message naming exactly what the device offers
instead of a bare refusal, e.g. "hw:0,0 (DS2) cannot play 352800 Hz
natively; for 24-bit audio it can do 44100, ..., 192000 Hz. phonia does
not resample in exclusive mode; to hear it resampled, play through the
sound server (`phonia ctl output set shared:default`)."

`AlsaSink::open`'s new order: set access and disable resampling, probe
the device's real capabilities (the track's own rate is added to the
probe's standard list, in case it is an unusual one), `choose` a format
or fail with the precise `Unsupported` right there — *before* touching
`set_channels`/`set_rate`/`set_format`/`hw_params` at all. Only once
`choose` has succeeded are those actually committed. The old post-hoc
"does not support N Hz" read-back check (which, it turns out, almost
never fired — `HwParams::set_rate(_, ValueOr::Nearest)` requests an exact
rate under the hood despite the name) stays as a defensive check: `choose`
already knows the rate is accepted, so by the time `hw_params` runs this
should be unreachable, but a stale answer must never silently play at the
wrong rate regardless.

This also generalizes which depths a device can play at all: padding a
narrower source into a wider lossless container (16-bit into `S24_3LE`,
which already happened for 24-into-`S32LE`) is not resampling, so
`SampleFormat::ALL`'s significant-bits check accepts any format wide
enough, not just a hardcoded per-depth list — a 20-bit FLAC now plays
correctly on a 24-bit-capable device, and a 16-bit track is no longer
refused by a device that only offers 24-bit containers.

One simplification from the plan as written: `AlsaSink::open` does not
do a separate upfront `test_channels` step before probing (as the plan's
own step-by-step sketch suggested) — `choose`'s own `Missing::Channels`
check, against the channel range `probe` already reads via
`get_channels_min`/`get_channels_max`, covers exactly the same case with
one less step, so there is no second, parallel channel check to keep in
sync. `caps` is also not stored on the sink yet (the plan's step 7): #28
is the first thing that will actually read it, so it is added then rather
than carried as an unread field in the meantime.

Not done here, on purpose: the engine does not yet report a refused
track's `TrackEnded` event (part 3), and the device label in `Unsupported`
messages is the raw device string `AlsaSink::open` already had (e.g.
`hw:1,0`), not the nicer `hw:1,0 (DS2)` form `probe_device` prints —
threading that label in would mean the `open_pcm`/resolve deduplication
the plan sketched for part 1, deliberately deferred (see that entry):
still not needed, since this PR touches none of that resolve/reserve
boilerplate.

## 2026-10-02 — #25/#26 part 3: a refused track is reported as `TrackEnded`, always

A second real bug the planning investigation found, independent of #25's
own capability work: `start_track` destructures its `Prepared` into
`meta`/`source`/etc. and only builds `self.current` (or, on the gapless
join path, `self.outgoing`) *after* `open_sink` succeeds. When it fails
instead, `fail()` is the only thing that runs next, and `fail()` only
knows how to report a track that is `self.current` or `self.outgoing` --
for a track that never got that far, especially the very first one the
engine is ever asked to play, neither is set, so the refusal reached
clients as a bare `Event::Error` with no matching `TrackEnded` at all: the
track simply vanished, leaving no record that it had ever been attempted.

The fix is narrow and keeps `fail()` itself untouched: `start_track` now
emits `Event::TrackEnded { meta, reason: Failed }` for its own track right
where `open_sink` fails, before the error is returned to whichever of its
two callers (the ordinary `Msg::Loaded` handler, or `start_opened_ahead`
on the gapless-prefetch path) goes on to call `fail(error)` as before.
Since this track was never installed as `self.current`/`self.outgoing`,
`fail()`'s own cleanup can't double-report it -- the two simply don't
overlap. The event order for this one case becomes `TrackEnded` then
`Error` (the reverse of `fail()`'s own Error-then-TrackEnded order for a
track that really was playing), which is harmless: nothing in this
codebase branches on the relative order of those two events.

Two tests pin this, both against `FakeSinkFactory`'s existing
`fail_next_open` (no new test hook was needed -- it already fails
whatever `SinkFactory::open` call comes next, exactly what a device
refusal looks like from the engine's point of view): a previous track
playing in full before the next one is refused (confirming the first
track's own audio and the release count are untouched), and the sharper
case the bug report singled out, the very first track ever played being
refused, with no previous track at all. A third scenario the plan asked
for -- the same refusal, but with the next track already prefetched ahead
of the first one finishing -- turned out to be unreliable to force
deterministically through `FakeSinkFactory::autoplay()` (an instantly-played
fake sink never gives `maybe_prefetch`'s periodic check a chance to run
before the track ends, so prefetching only ever starts reactively, inside
`end_of_source` itself, once the current track is already done -- there is
no way to race it ahead of that from a test): since the fix lives in
`start_track`, which both the ordinary and the opened-ahead callers already
share and call identically on failure, the two tests written cover the
same code without needing to pin that particular timing.

## 2026-10-02 — #25/#26 part 4 (last): `phonia devices` shows what a USB DAC claims, passively

The issue's own literal suggestion -- read `/proc/asound/cardN/stream0`
-- was rejected as the thing that *decides* anything back in part 1 (live
`HwParams` probing is authoritative and already open for the real
attempt; `stream0` is USB-only, pre-quirks, and blind to live state), but
it is still worth showing *somewhere*, for exactly one reason none of the
probing can match: it is the only way to say anything about a USB DAC's
capabilities **without opening the device**, so without taking it from
PipeWire first. `phonia devices` is read-only and already does not touch
PipeWire for anything else it prints, so it is the natural home.

`output/device.rs` gained a small parser (`parse_advertised`): it reads
the `Format:`/`Channels:`/`Rates:` lines of each altset under `stream0`'s
`Playback:` section, stopping at a `Capture:` section if there is one,
and skipping a `SPECIAL` (DSD) altset -- not a PCM format phonia or TIDAL
ever asks for. The `Rates:` value is kept as the kernel printed it, not
parsed into numbers: a continuous range (`8000 - 192000 (continuous)`,
seen on some devices) needs no special case this way, and `phonia
devices` never computes anything from this text, only displays it.
Altsets that report the same channel count and rates (the common USB
Audio Class shape: one altset per format, otherwise identical) are
grouped into one line, since that is normally every PCM altset a simple
DAC exposes -- the real Fosi Audio DS2 fixture this is tested against
(its actual `stream0`, captured by hand and checked into the test module
verbatim) collapses to exactly one line, `S16_LE, S24_3LE, S32_LE at
44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000 (2ch)`,
confirmed against the real CLI output too.

This closes #25 and #26: all 4 parts of the approved plan are merged.
Both issues are code-complete; closing them on GitHub is, as always, left
for the project owner to do by hand once they've seen this.

## 2026-10-02 — #28 part 1: the verdict persists in `Status`, stamped by output, protocol 1.7

Opus planned #28 (signal path indicator in the TUI) after a read-only
investigation found the wire path already existed in full: `AlsaSink`'s
`SinkReport` already reached `phoniad`, which already turned it into
`Event::SinkReport` for every subscriber, and `phonia ctl`'s event log
already showed it with its own `BIT-PERFECT`/`CONVERTED (reason)`/`SHARED
...` wording. The TUI was the only thing dropping it, through its
catch-all `_ => Effects::default()`. So #28 is almost entirely a TUI-side
job (parts 2-3) — except for one real gap this part closes.

**The gap:** a `SinkReport` fires once per sink *open*, not once per
track — gapless tracks of the same format, an ordinary skip, and a resume
all reuse the same open sink and get no fresh report. A client that
connects, reconnects (the TUI does this on its own, see #18) or resyncs
after falling behind therefore saw nothing until the *next format
change*, which on a same-format album could be never. `Status` needed a
persistent slot.

**The design:**

- **`Status.sink_report: Option<SinkReport>`** (additive, protocol 1.7 —
  the first wire change since the `v0.2.0` tag, so the "nothing released
  yet, keep adding under 1.6" reasoning from 2026-09-27 no longer applies;
  bumping the minor was the natural next step rather than a special case).
  Filled in by `Daemon::state()` from a new `last_report: Mutex<Option<ipc::SinkReport>>`
  field, which `fan_in` updates whenever a report arrives and clears on
  the two events that mean the sink is gone for certain
  (`OutputReleased`, `StateChanged(Stopped)`) — nothing else announces a
  sink closing or changing on its own.
- **`SinkReport::applies_to(status)`**, the gate `state()` runs the stored
  report through before showing it: true when the format matches
  (`status.spec == Some(report.source)`) and, when both sides know it,
  the output does too. This is how a stale report is recognized instead
  of being told about directly — a reopen for a new format, or an output
  switch, has no dedicated "the old sink is gone" event the way a release
  or a stop does.
- **`SinkReport.output: Option<String>`**, the route id (`exclusive:hw:DS2,0`,
  `shared:default`) of the output whose sink produced the report, is what
  `applies_to` compares. It is stamped in the **per-output factory
  closure in `phoniad/main.rs`** (`spec.id()`, captured once, before the
  closure is handed to `output::factory_for`) — the one place that knows
  it for certain. Stamping it later, say in `fan_in` from
  `self.outputs.route()`, was rejected: `Daemon::set_output` reopens and
  resumes the new sink *before* it awaits `outputs.list()` and publishes
  `Event::OutputChanged`, so a fresh `SinkReport` can arrive — and would
  need to be shown — before the daemon's own route has caught up. Reading
  `route()` at report time would either show the *old* route next to the
  *new* verdict, or (worse) silently miss the id mismatch and show a
  switch's verdict on top of whichever device used to be live a moment
  earlier.
- **No pre-check, no cache, no `Capabilities`.** Consistent with #25's own
  reasoning: a `SinkReport` is cheap to produce (the kernel already ran
  the real negotiation) and the report itself is the single source of
  truth, so nothing here duplicates or pre-computes it.

**Deliberately not done, both flagged in #25's own entries as open seams
for #28 to decide on, and both declined:**

- **#25's probed `Capabilities` are not attached to `SinkReport`.** The
  issue asks for source → format → device plus a verdict, which
  `SinkReport` already carries in full; the capability matrix answers a
  different question (what else the device *could* play), already has a
  home (`phonia probe-device`, and `phonia devices`'s passive `stream0`
  line from #25 part 4), and under "bit-perfect or refuse" adds nothing to
  a verdict for a sink that is already open and playing. The seam stays
  open for a possible future "what can my DAC do" view, separate from
  this indicator.
- **No structured "unsupported format" error code.** `caps::Unsupported`'s
  human text is already precise; a code would only matter to a client
  that acts on a refusal by itself (falling back to shared mode, say),
  which belongs with the "automatic output selection" wish already on
  record (2026-10-02, the #25/#26 wrap-up entry), not with an indicator
  whose job is to show the person what happened.

**Shared code, not a third reimplementation:** the verdict's own text
(`BIT-PERFECT`, `CONVERTED (reason)`, `SHARED (not bit-perfect[, resampled
to N Hz])`, `SHARED, LOSSY[ CODEC (codec)]`) moved from `phonia ctl`'s
`format_event` into `phonia_ipc::fmt::verdict`, the same boundary
`fmt::stream_quality`/`fmt::sample_rate` already establish for things both
`phonia-tui` and `phonia` need to show identically. `ctl`'s own test pins
that the move changed nothing observable. `phonia ctl status` gains a
`Verdict:` line from the new field as a side effect — a visible result of
part 1 on its own, with no TUI change needed to see it.

One cosmetic fix folded in, also from #25's own notes: shared mode's
hardcoded negotiated-format string was `"S32LE"`, while ALSA's own
`Display` (and exclusive mode) spell it `"S32_LE"` — `output/shared/pulse.rs`
now matches.

**Parts 2 and 3 remain**: the indicator itself in the TUI's bottom bar
(a new, always-reserved line, so the bar's height never depends on
whether a report has arrived — the same "layout never jumps" discipline
#24's covers work established), and showing a refused or failed track's
reason on that same line instead of the bare "Stopped" the TUI shows
today.

## 2026-10-03 — #28 part 2: the signal-path line, a fixed fourth row in the bar

`state.status.sink_report` needed no new field on `app::State`: since
`Status` already carries it after part 1, `Connected` and `Resync`
(which replace `status` whole) bring it in for free, the same as every
other status field. `on_daemon_event` gained the arms part 1's own design
implied it would need: `Event::SinkReport` sets it, `Event::OutputReleased`
clears it (the output is gone, so whatever it reported no longer applies),
`Event::OutputAcquired` and `Event::TrackStarted` mark the output open
again (`TrackStarted` only ever fires once `start_track` has actually
opened a sink), and `Event::StateChanged { Stopped }` closes the output
and clears the verdict -- the same two "the sink is definitely gone"
events the daemon itself keys off in part 1, kept in step on purpose.
`Event::TrackEnded` stays exactly as it was: it still does *not* clear
`sink_report`, since a report is per sink-open, not per track, and a
gapless album of one format must not go blank between tracks.

**Layout: a fourth bar line, present and blank in every connection state,
not only `Connected`.** `BAR_HEIGHT` went from 4 to 5 (the border plus
four lines, up from three) everywhere, including `Connecting`,
`Disconnected` and `Refused`, each of which gained one `Line::raw("")` to
match -- the same "never let the main panel's height depend on what
happened to load" discipline #24's cover work established, now applied to
the bar itself. A dedicated regression test
(`the_quit_key_is_on_the_same_row_in_every_connection_state_and_with_or_without_a_report`)
pins that the bar's last line lands on the identical screen row whether
connecting, disconnected, playing with no verdict yet, or playing with
one. The first line's own `(24-bit / 96 kHz, hires)` parenthetical was
removed: showing format and rate twice, once generically and once next
to the device that actually negotiated them, would read as noise once
the new line exists.

**The line's own shape**, by what `status` says:
- `Output::Released { by }`: "Output released[ to X]: resume to take it
  back" -- the same verb `ctl`'s own released wording uses.
- No track, but a known route: "Output: {description} ({exclusive |
  shared, not bit-perfect})".
- A track, but no `SinkReport` that `applies_to` the current status yet
  (the window between `TrackStarted` and the first write, or a track
  loaded paused): the source and format, ending in a bare arrow, styled
  dim -- nothing is claimed about the device until the kernel has
  actually confirmed something.
- A track with an applicable report: `{TIDAL <tier> | TIDAL | file}
  {bits}-bit / {rate}[ {n}ch if not stereo] → {format}[ {resampled rate}
  if shared] → {device} → ✔/✖ {verdict}`. Bit-perfect is `theme.accent`;
  a shared-mode "not bit-perfect" (not wrong, just not exclusive) is the
  new `theme.warn`; an actual `CONVERTED` or non-shared failure stays
  `theme.error`.

**`fit(path, verdict, width)`**, a new pure helper in the style of
`progress_line`: the verdict is kept whole as long as it fits by itself;
`path` is truncated with `…` to make room for it; only once the verdict
alone would not fit at all is *it* the one truncated. The verdict is the
whole point of the line, so it is the last thing to give way, not the
first.

**One new theme colour, `warn` (`Color::Yellow`)**, added to the existing
16-ANSI-colours palette rather than introducing anything outside it (see
the 2026-09-26 colours decision) -- with `NO_COLOR` it carries no
modifier at all, since the `SHARED (not bit-perfect...)` wording already
says plainly that this is not an error, unlike `error`'s bold+underline.

**Verified live** against the real Fosi Audio DS2 (shared mode, via a
PipeWire null sink, a real TIDAL track) by driving the real TUI over a
forked pty: the captured terminal bytes show the new line rendering
correctly end to end --
`92 kHz → S32_LE 48 kHz → phonia_test Audio/Sin…  ✖ SHARED (not
bit-perfect, r[esampled to 48000 Hz])` -- confirming the whole path from a
real `SinkReport` through `Status` to the rendered line. The harness could
not confirm a clean exit on `q` this time: a `q` keypress sent through
this particular forked-pty setup is never observed to end the process
within the harness's own wait window. Checked against `develop` itself
with the exact same harness and exact same non-result, to rule out a
regression before accepting it as a pre-existing limitation of the pty
setup, not of the change -- `q`'s own handling is separately pinned by a
plain unit test (`q_and_control_c_quit`) that needs no terminal at all.

## 2026-10-03 — #28 part 3 (last): a refusal reaches the signal-path line, not just "Stopped"

Today, a track the DAC refuses (#25) already fails with a precise reason
(`caps::Unsupported`'s own text) -- but the TUI never showed it. `Event
::Error` fell into `on_daemon_event`'s own catch-all, so the person just
saw the state go to `Stopped` with no explanation, the exact gap #25/#26's
own planning investigation first flagged and this part closes.

**A new, TUI-only `State.playback_error: Option<String>`**, not part of
the wire protocol at all (nothing here is a daemon concept, it is purely
"the last thing the signal-path line should say"). Set by `Event::Error`;
cleared once something strictly newer replaces it: a track actually
starting (`TrackStarted`), or a fresh `SinkReport` (a sink that just
opened is itself proof the refusal is over). It deliberately does **not**
reuse `last_error`: that field answers "did the request *I* just sent
work", is cleared by the next key the person presses, and its own
"Could not do that: ..." wording is written for a rejected command, not
an asynchronous failure from the engine that had nothing to do with a key
at all. Conflating the two would mean an unrelated keypress silently
wiping a signal-path error the person has not even read yet, or the
"Could not do that" phrasing appearing next to a problem nobody asked for.

`signal_line` checks `playback_error` **first**, before `status` at all:
by the time `Event::Error` arrives, `TrackEnded` (which fires first, per
#25/#26's own part 3 fix) has typically already cleared `status.track`,
so without this ordering the line would simply go blank, or worse, fall
through to whatever output-without-a-track text was already there,
losing the one thing worth saying. The text is run through the existing
`truncate` helper (not `fit`, which is for the path-plus-verdict pair;
here there is only one string) so a long `caps::Unsupported` message
(these can run well past a typical terminal's width, since they name
every rate the device actually offers) is cut with `…` instead of being
silently clipped by ratatui's own right-edge behavior or wrapping onto a
line that does not exist.

No live verification against real hardware for this part: the
development machine's own Fosi Audio DS2 accepts every rate and format
TIDAL can send it (confirmed repeatedly since #25), so a genuine refusal
cannot be produced against it -- exactly the same limitation #25/#26
recorded for testing the refusal path itself, now inherited here. Covered
instead by unit tests (`app.rs`: the error persists across unrelated
events and is cleared by the next track or report) and `TestBackend`
screen tests (`view/mod.rs`: the message renders, a track starting
replaces it, and a long message is truncated with `…` rather than wrapped
or silently cut).

This closes #28: all 3 parts of the approved plan are merged.

## 2026-10-03 — #30: ReplayGain is scaled in the shared sink only, never server-side

Picked after #28 shipped. A read-only investigation found TIDAL already
sends everything needed: `playbackinfopostpaywall` (fetched on every
track open already) carries `trackReplayGain`/`trackPeakAmplitude`/
`albumReplayGain`/`albumPeakAmplitude`; phonia's own `RawPlaybackInfo` in
`tidal.rs` simply didn't declare those four fields, so they were silently
dropped from JSON already being parsed.

**Central question: where does the gain actually get applied?** The
investigation's own leaning — fold it into shared mode's existing
PipeWire cvolume call, the same path volume/mute already use — was
rejected by the plan in favor of **scaling samples in process, confined
entirely to `SharedSink`**, for a concrete reason the investigation had
not considered: shared mode's buffer is roughly 0.65s deep (the ring
holds a quarter-second, the server itself targets another 2/5s), so a
volume command issued at a track boundary would land on audio from
*around* that boundary, not at it — misapplying the outgoing track's gain
to a few hundred milliseconds of the incoming track during a gapless
join, and vice versa. Server volume would also move the desktop mixer's
displayed percentage (polluting a control the person didn't touch),
fight with phonia's own echo-suppression for its own volume readback, and
needs new math regardless, since PipeWire's "raw" volume is cubic in
amplitude, not linear.

Exclusive mode gets **no new code at all**: a new `AudioSink::set_gain(&mut
self, linear: f64)` trait method defaults to a no-op body, and `AlsaSink`
never overrides it. Bit-perfect-or-refuse stays true by construction —
there is no runtime flag that could be left in the wrong state — rather
than by every call site remembering not to apply gain in exclusive mode.

**Track gain vs. album gain** follows play order, not queue order: album
gain applies when shuffle is off and the adjacent entry in play order
shares the same `album_id`; track gain otherwise, since shuffle breaks
album sequencing and an album-wide gain would be meaningless applied to
songs from different albums back to back. This needs a new
`album_id: Option<String>` threaded from `SourceInfo` to `QueueTrack`,
mirroring the `cover` field's own precedent exactly (#21/#24) — nothing
existing carries album identity this far; gapless (#27) is purely
format-based and has no concept of an album at all.

**Clip protection is always on, with no setting to disable it**: the
final applied gain is capped at `-20·log10(peak)` so a positive (boost)
gain can never clip a track whose true peak TIDAL reported; negative
gains are never touched by the cap, and a missing peak caps a boost at
0 dB (never silently trusted to be safe).

**Config defaults to `off`.** This is the first feature where phonia
alters a sample's value at all — on by default would contradict every
prior "never degrade without being told" choice in this project (DAC
reservation, bit-perfect-or-refuse, shared mode's own explicit
not-bit-perfect labelling). `[playback] replaygain = off|track|album|auto`
is file-only, like `gapless`, with no CLI flag.

**Displayed only when actually applied** (shared mode, gain != 0): the
TUI's flags area and `phonia ctl status`'s new `Gain:` line show it;
nothing is added to exclusive mode or to #28's own signal-path line — the
`BIT-PERFECT` verdict there already says everything worth saying, and a
redundant "not applied" note would just be noise.

**Wire protocol 1.8** (additive): a new `phonia_ipc::ReplayGain { kind:
GainKind, millibels: i32 }`, carried on both `dto::Track` and
`Event::TrackStarted` (so a gapless album's per-track gain updates
through the existing `TrackStarted` handler, no new plumbing). Millibels,
not a float, because every IPC type derives `Eq` and floats don't. No new
`Status` field: the value is fully derivable from `track.replay_gain` +
`status.route.mode`.

**PR split (4 parts, approved up front):** part 1 (this entry) — TIDAL's
loudness data reaches `TrackMeta`, no behavior change at all. Part 2 —
album identity, the `choose()` decision logic (track vs. album, peak
capping), and the config key; decided but not yet audibly applied. Part
3 — the first audible change: `AudioSink::set_gain`, `SharedSink`
applying it per write with the lead-in/outgoing-tail crossing rule for
gapless joins. Part 4 — protocol 1.8, `ctl`'s `Gain:` line, the TUI's
flags display.

**This part:** `RawPlaybackInfo`/`PlaybackInfo` in `tidal.rs` gained the
four optional fields (default-missing, for older responses); a new
`replaygain.rs` module holds just `Loudness` (a pure data type) and
`Loudness::from_playback_info`, which returns `None` when TIDAL sends no
track gain at all (the one field that's never absent when there's
anything to report). `TrackMeta` gained `loudness: Option<Loudness>`,
filled by `TidalOpener::open` from the playback info already being
fetched; `FileOpener` leaves it `None` (the seam is ready for reading
local-file ReplayGain tags later, out of scope for #30). Verified against
a real TIDAL track at both LOSSLESS and HI_RES_LOSSLESS (`-10.78 dB`
track / `-11.24 dB` album, `0.988553` peak both ways, same track measured
once per tier) via a new `#[ignore]`d test, run by hand.

Out of scope for #30: ReplayGain tags in local files, a runtime IPC
setter for the replaygain mode, and applying gain through a DAC's
hardware mixer in exclusive mode (left for a future issue if ever
wanted).

## 2026-10-03 — #30 part 2: the decision logic, still wired to nothing audible

Adds `Mode` (the `[playback] replaygain` config key: `off` default,
`track`, `album`, `auto`), `Kind` (which of the two gains ended up used)
and `choose(loudness, mode, same_album_neighbor) -> Option<AppliedGain>`
to `replaygain.rs` — a pure function, so the whole decision matrix is
unit-tested without a queue or an engine in sight.

**Peak capping follows the approved rule literally, not just
approximately**: a cut (`db <= 0.0`) is returned completely untouched,
whatever the peak says; only a boost (`db > 0.0`) is ever capped, at
`-20·log10(peak)` when a peak is known, or at `0.0` when it is not. An
earlier draft used a plain `db.min(cap)` for every gain, which happens to
agree with the literal rule for a normal peak (≤ 1.0, where the cap is
non-negative) but quietly over-cuts a negative gain whenever `peak > 1.0`
(a track TIDAL itself measured as already clipping, where `-20·log10(peak)`
goes negative) — caught by a dedicated test
(`a_cut_is_never_touched_by_the_cap_however_small_the_peak`) before it
could become a real discrepancy between the written rule and the code.

**`Mode::Album` falls back to the track's own gain when TIDAL has no
album measurement, and says so honestly**: `AppliedGain.kind` becomes
`Kind::Track` in that case, not `Kind::Album` — the point of `kind` is to
tell a later display (#30 part 4) what was *actually* used, and reporting
"(album)" next to a number that is really the track's gain would be a
small but real lie.

**Album context is a queue concept, not a `TrackMeta` one.** `album_id`
was added to `SourceInfo` and `QueueTrack`, mirroring `cover`'s own
precedent from #21/#24 exactly (same two structs, same "`None` for a
local file or an albumless track" rule) — nothing already threads album
identity this far, and #27's gapless join is purely format-based with no
concept of an album at all. `Inner::album_context(id)` (private to the
queue's own sequencing state machine, alongside `advance`/`peek`) looks
at `self.order` — the same play order `Advance`/`Previous`/shuffle
already maintain — for a neighbor (either side) sharing `album_id`, and
returns `false` unconditionally while shuffled, regardless of what the
shuffled order happens to put next to what: shuffle breaks album
sequencing by definition, so there is no "lucky" shuffled adjacency worth
treating as an album.

**`Queue::set_replay_gain(mode)` is called once, at daemon construction,
from `config.toml` alone** (`DaemonParts.replaygain`, mirroring how
`engine::Options.gapless` already arrives from `settings.gapless.value`)
— there is deliberately no IPC request to change it at runtime, per the
plan's own stated scope. `Queue::applied_gain(id, loudness)` combines the
stored mode with `album_context(id)` and `replaygain::choose`, so the
engine (part 3) only has to call one method with the `Loudness` it
already has from `TrackMeta`, never touching `Inner` directly.

Still nothing audible: no sink reads `AppliedGain`, no sample is scaled.
That is part 3. Verified with `phonia config show` and a real
`config.toml` carrying `replaygain = "auto"` (`(config file)` shown, not
`(default)`) in addition to the unit test suite.

## 2026-10-03 — #30 part 3 (first audible change): gain is scaled inside `SharedSink` only

Adds `AudioSink::set_gain(&mut self, linear: f32)` with a **default body that does nothing**, not
a flag `AlsaSink` has to remember to check. `alsa::AlsaSink` does not override it at all — zero new
lines in that file — so exclusive mode stays bit-perfect-or-refuse by construction: there is no
runtime state anywhere that could be left wrong and silently degrade a bit-perfect stream. Only
`shared::SharedSink` overrides it, scaling samples in `write` with a new `scale_samples(samples,
gain)` helper (`output/mod.rs`, `pub(crate)`, shared with `fake.rs`): rounds to the nearest integer
and clamps to `i32::MIN..=i32::MAX`, so a boost near full scale saturates instead of wrapping to a
huge negative value.

**Where `TrackMeta.gain` gets its value**: not computed by the engine at all. `Queue::open_entry`
(the same place `cover` is already filled in from the queue item, and `record_meta` is already
called) now also does `loaded.meta.gain = loaded.meta.loudness.as_ref().and_then(|loudness|
queue.applied_gain(id, loudness))` — reusing #30 part 2's `Queue::applied_gain` directly. This
means the engine's own code never has to know about `Mode`, album context, or even that a `Queue`
exists: by the time a `TrackMeta` reaches `audio_thread.rs`, its `gain` is already the final,
settled answer, exactly the same layering already used for `cover`/`loudness`.

**The subtle part: which track's gain applies to a given `write` call.** Normally the answer is
trivial — `playing.pending` is always `self.current`'s own decoded audio, so `playing.meta`'s gain
applies to every write, full stop. The one real exception is `set_aside_unheard` (called when the
device is reopened mid-track: an output switch, a release): during a gapless crossing, it rebuilds
`pending` from `recent` (the rolling buffer of what was already written to the old sink but not
yet heard) plus whatever hadn't been sent yet. If the reopen happens while the listener is still
inside the *previous* track's tail — `playing.lead_in` tracks exactly where, in the frame-counter
space, the previous track's audio ends and the current one's begins — that rebuilt buffer can
genuinely mix both tracks' samples in one `Vec`. `play_step` handles this by comparing
`playing.frames_written` against `playing.lead_in` before every write: while still short of it, the
write is capped at the boundary (so a single `write()` call is never split across two gains) and
uses `outgoing.meta`'s gain; once past it, `playing.meta`'s gain applies as normal. Caught early by
writing the regression test first (`switching_output_mid_crossing_still_gives_each_track_its_own_gain`,
modeled directly on the pre-existing `switching_the_output_while_the_next_track_is_joined_...`
test) rather than discovering the mixed-buffer case by inspection alone — the test reuses that
existing scenario's exact timing (switching while "b is written behind a") specifically because
that is what forces `set_aside_unheard` to run with `self.outgoing` still `Some`.

**`FakeSink` gained the same scaling `SharedSink` has** (recording which gain was in effect for
each write, the way a real `SharedSink` would convert it to louder or quieter samples), purely so
the engine's own gain-selection and lead-in-splitting logic has a test double to run against — it
does **not** mean `AlsaSink`-backed (exclusive) playback scales anything for real; `FakeSink` here
is standing in for "whatever the write ends up doing," the same way it already stands in for ALSA's
blocking/period behavior in every other engine test. No change needed to the non-gapless case or to
`recent`'s own bookkeeping: `recent` already stored pre-gain (raw, as-decoded) samples before this
part, which turns out to be exactly right — re-scaling them on replay, per the rule above, is
correct precisely because they were never scaled going in.

**Verified for real against PipeWire** (a null sink loaded and torn down by the test itself, `parec`
recording its monitor): `set_gain_scales_every_sample_written_after_it` writes a known ramp with
`set_gain(0.5)` and confirms the exact halved sequence appears in what was recorded;
`a_gain_change_lands_on_the_exact_frame_boundary` writes unscaled audio, then calls `set_gain(0.5)`
mid-stream and writes more, confirming the unscaled-then-exactly-halved sequence appears joined
with no sample caught at the wrong gain. Both ignored by default (`needs a sound server, pactl and
parec`), run by hand like every other test in this file's "against the real sound server" section.
No real ALSA/DAC hardware test is needed: exclusive mode has no new code to verify.

## 2026-10-03 — #30 part 4 (last): a gain is shown only where it was actually applied

Wire protocol 1.8: `dto::Track` and `Event::TrackStarted` both gain `replay_gain:
Option<ReplayGain>`, where `ReplayGain { kind: GainKind, millibels: i32 }` — millibels, not a
float, for the same reason `SinkReport`'s own numeric fields are integers: every IPC type derives
`Eq`, and `f32`/`f64` don't. `GainKind` mirrors `replaygain::Kind` (`Track`/`Album`). No new
`Status` field: the value is fully derivable from `track.replay_gain` and `status.route`, so there
is nothing to keep in sync between two places. `convert::replay_gain` is the one conversion point
(`AppliedGain { kind, db }` → `ReplayGain { kind, millibels }`, `db * 100.0` rounded), called from
both `status_dto` and `event` wherever `meta.gain` already is.

**The one real design decision here**: a new `phonia_ipc::fmt::replay_gain(track, route) ->
Option<String>` decides whether to show anything at all, and it checks *two* things, not one —
`track.replay_gain.is_some()` **and** `route.mode == Shared`. A gain is decided by
`Queue::applied_gain` the same way regardless of which sink ends up playing the track (the
decision doesn't know about exclusive vs. shared at all), but it is only ever actually scaled into
the audio inside `SharedSink` (part 3). Showing `RG -2.9 dB` next to an exclusive-mode track would
therefore claim an effect that never happened — the number would be real, but misleading about
what the listener is actually hearing. This is exactly the same reasoning #25/#26 already applied
to `catalog.rs`'s `bit_perfect` flag (describe what the route actually does, not what was merely
computed), reused here rather than re-derived.

Display text: `RG {millibels/100:+.1} dB ({kind})`, e.g. `RG -2.9 dB (album)` or `RG +1.2 dB
(track)` — always signed, since a boost is the rarer and more surprising case and deserves to be
unambiguous at a glance. `phonia ctl status` gained a `Gain:` line, placed right after `Quality:`
(both describe the track, not the output). The TUI's bar shows the same text next to the volume in
`flags()`, so it appears exactly when there is something to say and nothing otherwise — no empty
line, no placeholder. Nothing was added to #28's own signal-path line: that line already answers
"what format, through what device, bit-perfect or not," and a loudness adjustment is a different
question from format fidelity: conflating them would make that line's one job less clear, not more
complete.

Tested the way `ctl.rs` and the TUI already test everything else: `format_status` gets a
shared-with-gain case (shows the line) and an exclusive-with-the-same-gain case (does not),
confirming the route check actually gates the display and not just the data's presence. The TUI's
`flags()` gets the same three-state progression (no route yet → exclusive → shared) as a single
test, matching the existing `TestBackend`-based verification style used for every other display
decision in this codebase.

This closes #30: all 4 parts of the approved plan are merged.

## 2026-10-03 — #31: a hardware volume belongs to the card, not to phonia

Picked after #30 shipped. The issue is one line: "ALSA control if it exists; lock at 100% if not."
A read-only investigation found #30 had already named this exact feature and explicitly deferred
it ("applying gain through a DAC's hardware mixer in exclusive mode, left for a future issue if
ever wanted") — #31 is that issue. Exclusive mode has had no volume at all until now:
`SinkFactory::volume()` defaults to `None`, and the CLI's own refusal message tells the person to
go use the DAC's own knob by hand. #31 does that programmatically, through ALSA's Selem (simple
mixer) API — already safely wrapped by the `alsa` crate phonia depends on, no new dependency.

**This is a different mechanism from #30's `AudioSink::set_gain` entirely.** ReplayGain scales
samples in process, inside `SharedSink` only; a hardware volume attenuates inside the DAC itself,
after the stream leaves phonia untouched. `AlsaSink` never overrides `set_gain`, and nothing here
changes that: exclusive mode stays bit-perfect by construction, the same way it always has.

**Central principle, settled during planning rather than left implicit: a hardware volume belongs
to the card, not to phonia.** phonia reads it and sets it only when the person explicitly asks —
it never seeds a value, never restores one, and never carries a level *into* a hardware control on
startup or an output switch. This one rule is what the rest of the plan falls out of, and it fixes
two real bugs the plan's own research found in `phoniad/src/outputs.rs` before any code was
written: `Outputs.level` seeded a fresh output at `Volume::default()` (100%) instead of reading the
hardware's actual level, so the first relative change (`+5`) would be computed from the wrong base
and could jump the DAC hard; and `switched_to` wrote the carried level into *any* newly attached
output, which on a DAC already driven by PipeWire (shared mode uses the same hardware control)
would double-attenuate, and on a fresh exclusive attach could push it straight to 0 dB. Fixed by
simply never writing a hardware control except on an explicit `set`, not by a special case for
either bug — both follow from the one rule.

**Real checks run during planning, not left to implementation time**: the development machine's own
Fosi Audio DS2 does have a usable control — a `PCM` Selem (a UAC Feature Unit, so the attenuation
happens inside the DAC after the USB stream), raw range 0..63 mapping to an exact -63..0 dB in 1 dB
steps — currently driven by WirePlumber for its own shared-mode volume (found already set to a
non-default value, confirming something else actively uses it). The internal `sof-hda-dsp` card
exposes a `Master` Selem, a second real "has a control" subject; an NVidia HDMI output has only
switch-only `IEC958` controls, a real "locked" subject. Also found: the `alsa` crate's `Mixer` is
`Send` but explicitly not `Sync` — ruling out holding one open inside an `Arc<dyn VolumeControl>`,
which independently supports re-opening the mixer fresh per call rather than caching anything.

**Decisions (user confirmed all four as recommended):**

- **Re-probe the control on every `get`/`set`, with no cached state.** Resilient to a replug (a
  different physical card, possibly with a different control entirely, under the same configured
  `hw:DS2,0`), the same by-id precedent `AlsaSink` itself follows for the PCM device (2026-09-23).
  Also sidesteps the `Mixer: !Sync` constraint above. The cost (opening the control device, a few
  syscalls) is well under a millisecond and not a hot path.
- **Map percent to hardware through the control's own dB range, on the same cubic-in-amplitude
  curve already implied by shared mode's own percent semantics** (`Volume`'s doc: "50% is about
  -18 dB"): `dB = 60·log10(p/100)`, rounded always toward quieter. This is also the curve alsamixer
  and pavucontrol themselves use, so phonia's percentage agrees with the rest of the desktop — the
  alternative (ALSA's raw linear range) does not: 50% raw on the DS2 is actually -31 dB. 100% is
  capped at the lower of the control's maximum or unity (0 dB): phonia never selects a gain above
  unity even on a control that offers headroom. 0% always means the control's *exact* reported
  minimum, never a computed figure (the curve's own value there, -∞, is meaningless, and a control
  shallower than 60 dB would otherwise be asked for something unreachable). A control with no
  usable dB data falls back to its raw linear range, the same fallback alsamixer itself uses.
- **Mute uses the control's own playback switch when it has one** (the DS2 does). A control with
  volume but no switch **refuses** a mute request outright, rather than faking it by dropping to
  the minimum: the minimum is often not silence (the DS2's is -63 dB, not -∞), and faking it would
  need phonia to remember a level to restore on unmute — exactly the cached, card-could-have-moved
  state the "belongs to the card" principle rules out.
- **The live outside-change watcher (`alsamixer` run concurrently, or WirePlumber rewriting the
  control after reclaiming the card) ships as part 3 of #31 itself, not deferred to a separate
  issue** — justified as a real, not hypothetical, case: the DS2's control was found already at a
  value something else had set. Until part 3 lands, `on_change` only stores the handler and never
  calls it, which is an accepted, temporary gap (the symptom is a stale TUI display until the next
  full status, nothing worse) rather than a reason to block part 1 on it.
- **"No control, locked at 100%" stays communicated by rewording the existing refusal message, with
  no protocol change.** `Status.volume` stays `None` exactly as today; the message explains why
  rather than the wire format gaining a `fixed: bool` for what would be a purely cosmetic gain.

**Other decisions, adopted as recommended without a separate question:** control preference order
when a card exposes several volume-capable Selems — `"Master"`, then `"PCM"`, then the single
remaining one *only if there is exactly one*, else none (no config override in v1; the DS2 resolves
to `"PCM"`, `sof-hda-dsp` to `"Master"`, confirmed against both real cards); carry the digital
volume across an output switch only shared→shared, never into or out of a hardware control, which
is really the same "belongs to the card" rule stated once more, not a separate case; include a
passive `phonia devices` hint (the control's name and range) as part of part 3, alongside the
watcher, the same spirit as #25/#26 part 4's passive `stream0` display.

**3-part split:** Part 1 (this entry) — `output/mixer.rs`: pure, unit-tested percent↔dB and
control-selection functions, plus `HardwareVolume` (the `VolumeControl` impl), not wired into
`AlsaSinkFactory` yet. Part 2 (first audible change) — wire `AlsaSinkFactory::volume()` to return
`Some` conditionally; fix both `Outputs` hazards above; publish `VolumeChanged` after `OutputChanged`
when the new output has one; reword every stale "exclusive has no volume" message across
`mod.rs`/`proto.rs`/`daemon.rs`/`ctl.rs`/the TUI. Part 3 — the live watcher thread and the `phonia
devices` hint.

**Verified for real against three physical cards, no fakes needed for either branch**: the DS2's
`PCM` control (read at its actual live value, -10 dB/68% — not a throwaway default — round-tripped
down to silence and back in several steps never louder than that starting point, muted and
unmuted, restored to the exact original raw value and switch state afterward, confirmed with
`amixer` before and after); the `sof-hda-dsp` card's `Master` control (confirming `choose_control`
picks the right name on a second, differently-shaped real control); and an NVidia HDMI output
(confirmed to correctly report no usable control, read-only, nothing audible — the reservation
system is irrelevant here, since the mixer control device has nothing to do with the PCM's
reservation and can be read/set whether or not phonia currently holds the card).

Out of scope for #31: a config override for control-name selection, a `fixed: bool` protocol field,
showing the hardware dB value on #28's own signal-path line, and hardware volume for shared outputs
(PipeWire already owns that). Software volume in exclusive mode of any kind remains out of the
question — it would break bit-perfectness, which is the one thing this project never trades away.

## 2026-10-03 — #31 part 2: `Outputs` reads the control, it never caches it

Wires `AlsaSinkFactory::volume()` to `self.hw_volume.probe().is_some().then(|| self.hw_volume.clone()
as Arc<dyn VolumeControl>)` — re-probed on every call, per part 1's own decision, so a replugged card
is never trusted to still have whatever it had a moment ago.

**Both hazards part 1 found are fixed by the same change, not two separate patches.** The actual bug
in both cases was that `phoniad::Outputs` kept its own `Mutex<Volume>` (`level`) as the thing it
reported and computed relative changes from, seeded once at `Volume::default()` and never
synchronized against reality. The fix removes that role from `level` entirely: `Outputs::volume()`
now calls `control.get()` directly every time, and `set_volume`'s relative base is `control.get()`,
not `level`. `level` still exists, but only for one narrow job — remembering the last *shared*
level, so a future shared output can start where the previous one left off — and nothing else reads
it. This is a strictly more correct design than literally patching `attach()` to "read the value when
not carrying," which was the plan's own original framing: it closes the staleness window entirely
(there is no gap between an attach and the first read where a cached value could be wrong), it costs
nothing extra (a hardware read is already sub-millisecond, confirmed in part 1), and it means a
hardware control changed by anything else — even before part 3's watcher exists — is reflected the
very next time anyone asks, not just after a reattach.

`switched_to` now computes `both_shared = old spec is Shared && new spec is Shared` *before*
overwriting `self.current`, and passes that as `carry_volume` to `attach` instead of always `true`.
Carrying was never the only thing standing between "shared" and "hazard": even with the fix above,
writing an old level into a *hardware* control on attach would have been a real, audible jump by
itself, so both the write-side (`attach`'s `carry_volume`) and the read-side (`volume`/`set_volume`
above) needed fixing, not just one.

**Every "exclusive has no volume" message became "no hardware mixer control"**, reworded, not
restructured: `SinkFactory::volume`'s and `Volume`'s own doc comments (`output/mod.rs`),
`VolumeError::Unsupported`'s doc, `SetVolume`'s doc and `ErrorCode::Unsupported`'s doc (`proto.rs`),
the daemon's own wire error text and the TUI's local copy of the same refusal, and `ctl.rs`'s
`volume`/`mute` help text. No wire or behavior change from this alone — `Status.volume` is still
`None` exactly when there is nothing to set, `ErrorCode::Unsupported` is still what a refusal reports
as. The TUI's `OutputChanged` handler, which clears `status.volume` pre-emptively for any exclusive
route, was *not* restructured: the daemon already publishes `VolumeChanged` right after
`OutputChanged` whenever the new output has one (this already existed, in `set_output`, for whatever
reason the investigation's own research had missed it), so the clear is corrected by the very next
event in the same sequence; only the comment explaining *why* it clears needed to stop asserting
"exclusive never has volume" as if it were still universally true.

**Verified for real, end to end, against the DS2** (a scratch `phoniad` on a throwaway socket,
`--output exclusive:hw:DS2,0`, no track ever played): `phonia ctl status` showed the control's actual
live value (68%, matching the -10 dB `amixer` independently confirmed) the moment the daemon started,
never a seeded 100%; `volume -5` landed at exactly 63%, confirmed both by phonia's own report and a
separate `amixer` read; mute, then unmute, each left the level exactly where it was; switching to a
wholly unrelated shared output (the laptop's own speaker, not the DS2 under PipeWire) started that
stream at a fresh 100%, not the DS2's 63%; switching back to the DS2 showed 63% again, proving the
hardware was never written during the detour through shared mode; the card was returned to its exact
starting state (84%/-10 dB/on) before the scratch daemon was stopped. This exercises the full stack —
engine-independent, since the mixer control has nothing to do with the PCM or its reservation — with
nothing synthetic standing in for any part of it.

## 2026-10-03 — #31 part 3 (last): the watcher fires on a change, not on attaching

Adds the live watcher `on_change` was always meant to start, and a passive `phonia devices` hint.

**The watcher is a background thread, started once per `HardwareVolume` the first time anything
calls `on_change`** (guarded by a `watching: Mutex<bool>`, since every `Outputs::attach` calls it
again on the same, still-live control). It keeps its own `Mixer` open for as long as it runs — the
one place in this module that does, everywhere else in `HardwareVolume` opens a fresh one per call
on purpose (part 1) — because that is the only way to use `Mixer::wait`, which blocks on the
control's own poll descriptors for a real kernel event instead of guessing a polling interval. The
thread holds only a `Weak<HardwareVolume>` (via `Arc::new_cyclic` at construction), so it exits on
its own, with no explicit stop needed, once whatever `AlsaSinkFactory` owned it switches away and
drops the last strong reference. While the card is gone it re-resolves and reopens every five
seconds, the same re-probe-on-every-operation principle as everywhere else here.

**A real bug, caught by writing the hardware test for this before trusting the first version at
all**: the first implementation compared every reading against a `last: Option<Volume>` that
started as `None`, so the very first reading after opening the control — the level the card
already happened to be at, not a change at all — always looked different from `None` and fired
immediately. The test (`hardware_watcher_reports_a_change_made_outside_itself`) caught this
directly: it failed by reporting the *original* value instead of the *target* one, because the
spurious "attach" announcement raced ahead of the real change and satisfied the test's "wait for
any report" loop first. Fixed by reading the starting value silently right after opening, with no
comparison and no callback, and only beginning to compare — and only then ever calling the
handler — from the next tick on.

**The "outside change" in the test is a second, independent write to the same control**, not
literally `alsamixer` or WirePlumber: from the watcher's own persistently-open `Mixer`, there is no
way to tell a write issued by another real program from one issued by a second call to the same
`HardwareVolume`'s own `set` (which always opens its *own*, separate `Mixer` per part 1's design) —
both arrive at the watcher as nothing more than "the control's value changed," which is exactly the
mechanism being tested. Running two of these hardware tests at once (plain parallel `cargo test`,
no `--test-threads=1`) makes this same ambiguity work against the tests themselves — confirmed
directly: `hardware_mixer_round_trip` and the watcher test running concurrently each looked to the
other like an unrelated outside change, and both failed until serialized. Documented at the top of
the real-hardware test section rather than treated as flaky: it is the expected behavior of
hardware two tests happen to share, the same reason none of this module's other real-hardware tests
are safe to run concurrently with each other either.

**The `phonia devices` hint is deliberately not on `catalog::Entry`, or anywhere the daemon's own
output list or the wire protocol reaches.** It is a standalone-CLI, read-only probe — the same
boundary the USB `advertises` line (#25/#26 part 4) already drew between "what a device claims,
read without opening it, shown by the CLI" and "what the daemon's own clients see over IPC." Giving
`device::list` a `probe: impl Fn(u32) -> Option<ControlInfo>` parameter (real callers get
`mixer::probe_card`) rather than calling real ALSA mixer access unconditionally was necessary, not
just tidy: `list`'s existing tests build a *fake* `/proc/asound`-shaped directory tree and reuse
card numbers freely, and this machine's real card 1 (NVidia) and the fake tree's card 1 (also named
differently) are different cards entirely — a hardcoded real probe would have silently reached past
the fake tree into this machine's actual hardware, passing or failing depending on what happened to
be plugged in on whoever ran the test, not on the code. `mixer::probe_card(index)` itself is new
too: unlike `probe(device)`, it never calls `device::resolve` at all, so a caller that already has
the right numeric index from its own tree (fake or real) is never at risk of that same mismatch.

**Verified for real.** The watcher: a dedicated test opens the real DS2's control, installs a
handler, waits for the watcher thread to establish its baseline, writes a new level through a
second call to the same control, and confirms the handler fires with that exact value — the bug
above was caught this way, by this same test, before any version of the fix shipped. `phonia
devices`, run live: `hardware volume: PCM (-63.0..0.0 dB)` under the DS2, `hardware volume: Master
(-65.2..0.0 dB)` under the internal `sof-hda-dsp` card, and correctly no such line at all under the
NVidia HDMI card.

**Also verified through the full real daemon, end to end, not just the isolated module**: a scratch
`phoniad` on the real DS2, with `phonia ctl watch` running against it. A plain `amixer -c 0 sset PCM
20%` — a completely independent process, the closest real stand-in for `alsamixer` short of
scripting the TUI itself — landed the control at -50 dB, and `phonia ctl watch` printed `volume 15%`
(matching `100·10^(-50/60) ≈ 15`) within the same second, with `phonia ctl status` agreeing
afterward. This exercises every link the isolated hardware test above does not: the watcher's
handler reaching `Outputs::volume_changed_outside`, the daemon publishing `VolumeChanged`, and a
client actually receiving it. Restored to the exact original raw value afterward.

This closes #31: all 3 parts of the approved plan are merged.

## 2026-10-06 — #120 part 1: plays go to TIDAL as its own `playback_session` event, Android-shaped

The first issue of Phase 4. Its one-line body: "Reports finished plays to TIDAL so Recently Played
reflects what you listen to in SONE" — SONE is a different, open-source Linux TIDAL client
(`lullabyX/sone`), not phonia's own name; the issue's wording was lifted from its README, which is
also what led to finding it as a reference during planning (see below).

There is no documented "mark this as played" endpoint. TIDAL's official apps send a generic
analytics event — a `playback_session`, under the `play_log` event group — through the "TIDAL
Event Platform," an ingest pipeline behind AWS SQS's `SendMessageBatch`, reached at
`https://ec.tidal.com/api/event-batch`. None of this is in TIDAL's public developer docs; it comes
from TIDAL's own open-source SDKs (`tidal-music/tidal-sdk-web`'s `event-producer` and `player`
packages, and `tidal-music/tidal-sdk-android`), which define the event's name, transport and the
"web" shape of its body. That web shape — a bare `{group, name, payload, version, ts, uuid}` — is
what a literal reading of those SDKs gives, but it is **not** what makes a play show up in Recently
Played for phonia: SONE's own implementation (GPL-3; read for the protocol facts it had already
live-verified against a real account, no code copied, same category of information as the SDK
source itself) found and documented that only the **mobile/Android shape** — the same body plus
top-level `user: {id, clientId, sessionId}` and `client: {token, deviceType: "mobile", version,
platform: "android"}` objects, both filled from the access token's own JWT claims — actually
produces a row. That difference matters specifically for phonia, not just because SONE happened to
need it: phonia's own PKCE client id (what `tidlers` authenticates with by default,
`6BDSRdpK9hqEBTgU`, decoded from its `auth::credentials` module) is a native, Android-type client,
not a browser one — an event rides on the identity of the client whose token it carries, so it has
to describe that kind of client, not phonia, whatever client happens to be sending it.

**`sourceType`/`sourceId` are not optional**, even though TIDAL's backend accepts an event without
them: SONE's own live-verified finding is that a sourceless play is accepted but never produces a
Recently Played row at all. The real enum is `ALBUM | PLAYLIST | ARTIST | MIX | ITEM | MY_ITEMS`,
not the `"TRACK"` a literal reading of "productType" would suggest. phonia has no plumbing today to
know whether a play came from an album, a playlist or a mix it was queued from, so every play is
reported as `ITEM` + the track's own id — the same fallback the official apps themselves use for a
track started outside any container (search, a deep link, a context menu). Richer attribution is a
seam for a later issue, not something #120 needs to get the feature working at all.

**The threshold is a flat 30 seconds actually heard** (wall-clock time in `Playing`, excluding any
paused span), regardless of `EndReason` or the track's own length — TIDAL's own rule, confirmed
live by SONE's tests, not a guess or a Last.fm-style "half the track" heuristic. A track completed,
skipped or failed after 30 real seconds of listening counts the same way; a 25-second track played
in full never can.

**Decided with the user** (recommended options in parens, chosen unless noted): (1) implement
against TIDAL's real Recently Played via this event, accepting that it is an undocumented contract
TIDAL could change without notice — a failure here is always silent and never touches playback
(recommended, chosen); (2) `[tidal] report_plays`, file-only like `replaygain` (sending your own
listening activity to your account is a deliberate, written-down choice), but **on by default**
like the official apps — the one place the user went against the recommendation, which had argued
for off-by-default on privacy-adjacent-decision grounds; (3) fire-and-forget with one in-memory
retry, no disk-persisted outbox — anything still pending when the daemon exits is lost (recommended,
chosen; the official SDKs' own persistent queue is solving a problem — surviving a mobile app being
killed by the OS — that doesn't really apply to a desktop daemon); (4) ship with `ITEM` + track id
now, leave real container attribution for a later issue (recommended, chosen).

Everything about the exact identity phonia's events claim to be — the pinned app
version/OS/device-model/vendor strings, whether `sid` is reliably present in the JWTs `tidlers`'
PKCE flow produces, and whether a bare `playback_session` is sufficient on its own (versus needing
a correlated `x-tidal-streamingsessionid` header on the playbackinfo request, or the separate
`streaming_metrics` events TIDAL's SDKs also send) — is marked `PROVISIONAL (#120)` in
`play_log.rs` and stays open until a live test against a real account confirms or corrects it in
part 2.

Part 1 (this part) adds `phonia-core`'s new `play_log.rs`: the event's body/headers/SQS-batch
encoding as pure, unit-tested functions; `PlayLog::send`, which sends through the one TIDAL session
the process already owns (`TidalOpener::play_log()`, the same pattern as `catalog()`); and
`SessionTracker`, a pure state machine (every method takes an injected timestamp, not the real
clock, so it needs no real time to test) that turns `Position`/pause/resume/seek/`TrackEnded`-shaped
events into a finished `PlaybackSession` once 30s have actually been heard. Nothing is wired to the
engine or the daemon yet — `[tidal] report_plays` exists and defaults to `true`, but nothing reads
it outside a test. No change to the wire protocol: this is entirely a `phoniad`-side, best-effort
side effect, not something any client needs to see or control.

## 2026-10-06 — #120 part 2 (last): wired into `phoniad`, verified live

`Daemon::note_play_log` feeds every relevant engine event
(`TrackStarted`/`Position`/`Seeked`/`StateChanged(Paused|Playing)`/`TrackEnded`) to part 1's
`SessionTracker`, called from inside `fan_in` on the exact queue snapshot `fan_in` already fetches
for `convert::event` — not a separate, later lookup. That has to happen there specifically: a
gapless join can advance the queue to the next entry before the outgoing track's own `TrackEnded`
is even converted, so resolving the TIDAL id and delivered quality any later would risk resolving
the *wrong* track's source. The resolution itself (`resolve_tidal_track`, new, pure) parses
`QueueTrack.source`'s wire form through `openers::Source::parse`; a local file (`Source::File`)
always resolves to no id, since there is nothing to report TIDAL about.

Sending is fire-and-forget from `fan_in`'s own perspective: a finished session is handed to a
spawned task (`spawn_report`) with one in-memory retry after 30 seconds, then given up — never a
disk-persisted outbox, as decided when the plan was approved. Any failure is logged only through
`phonia_core::warn!`, so it is silent unless `[daemon] verbose` is on, and never anything the
caller of `note_play_log` waits on.

`main.rs` only builds a `PlayLog` at all — via `TidalOpener::play_log()` — when `settings.
report_plays.value` is true; `Daemon::start` only starts a `SessionTracker` alongside it in that
case. No wire protocol change, exactly as planned: no client needs to see this or control it.

**Not tested against a crossing of the real 30-second threshold**, deliberately: `SessionTracker`
accounts "heard" time against real wall-clock timestamps (`now_ms`, actual `SystemTime`), which is
correct for the real daemon but would make an automated test either take 30 real seconds or need a
fake clock threaded all the way into `Daemon` for no real benefit — that math is already
thoroughly covered, with an injected clock, by part 1's own `SessionTracker` tests. What part 2
adds that part 1's tests could not cover is the wiring itself: a new `daemon::play_log_tests`
module tests `resolve_tidal_track` directly against constructed `QueueSnapshot` fixtures (a TIDAL
track resolves its id and delivered quality; a local file resolves to no id; a reference the queue
no longer has resolves to nothing) — the one genuinely new, fragile piece of logic in this part,
isolated from the engine and the network entirely.

**Verified live, exactly as the plan required before merging**: a scratch daemon on a PipeWire
null sink, real account, `report_plays` at its default (on). Two different real TIDAL tracks each
played past 30 seconds and then skipped (`Interrupted`) were accepted by
`https://ec.tidal.com/api/event-batch` (no `BatchResultErrorEntry`) and confirmed, by hand, to
show up in that account's actual Recently Played shortly after; a third track skipped after only
about 10 seconds correctly produced no event at all, matching `SessionTracker`'s unit tests now
confirmed against real wall-clock time, not an injected one. The heard position reported in each
event (e.g. 59.85s on a track skipped roughly a minute after it started) matched real elapsed time.

This settles the one question part 1 had left open: **a bare `playback_session` event is enough
on its own** — no correlated `x-tidal-streamingsessionid` header on the playbackinfo request, and
no separate `streaming_metrics` events, were needed for Recently Played to update. The
PR3-conditional fallback the plan had set aside for that case was never needed and is dropped. The
pinned client identity in `play_log.rs` (app version, OS/device strings) stays marked
`PROVISIONAL (#120)` on principle — it worked today, against this account, but it is still an
undocumented contract TIDAL could tighten or change without notice, which is exactly why a failure
here stays silent and never touches playback.

This closes #120: both parts of the approved plan are merged and verified against a real account.

## 2026-10-06 — #124: a bigger title, bold not big-text; artist split out as its own prerequisite

A brand new milestone, "Fase 6 — Layout and appearance modifying," created for one issue (#124),
whose one-line body is the title itself. The owner's concrete ask came from a screenshot of
TIDAL's own web player's now-playing view: a visually bigger, bolder track title, with the artist
on its own quieter line underneath next to a small circular marker — a clear two-tier hierarchy,
not today's single `Artist - Title` string.

**Four open design questions, decided with the user (recommended options throughout, via
`AskUserQuestion`):**

1. **How to make the title visually bigger.** Investigated concretely, not assumed: the only
   literal (multi-row-glyph) option is the `tui-big-text` crate (`font8x8` bitmap glyphs,
   `ratatui`-compatible). Its charset covers Basic Latin, Latin-1 and — usefully — Hiragana, so
   "Beyoncé"/"Mötley Crüe"/"Björk" render fine, but **not** Latin Extended-A ("Dvořák," Turkish
   ı/ğ), Cyrillic, Katakana/Kanji, Hangul, or the typographic punctuation TIDAL's own metadata
   actually uses (curly quotes, em/en dashes, ellipses) unless normalized first. A missing glyph
   silently renders as a blank cell — worse than a visible failure, since nothing looks wrong at a
   glance. The bigger problem is width, not characters: every size tier costs at least 4 terminal
   columns per character (`Quadrant`, the only tier using glyphs common to ordinary terminal
   fonts, costs exactly 4; `Full`/`*Height` cost 8), against a guaranteed ~20 columns next to the
   cover (`MIN_TEXT_WIDTH`, `covers.rs:243`) — most real TIDAL titles simply would not fit even at
   the cheapest tier. **Decided: bold-weight hierarchy only for #124** (`Modifier::BOLD` on its
   own line, no new dependency) — `tui-big-text` stays a possible, explicitly optional follow-up
   PR, not blocking this issue, with a normalize-then-fits-or-fall-back-to-bold rule already
   designed if it's ever picked up (never truncate a big-text title, never leave blank cells).
2. **Whether to lift the 16-ANSI-color deferral.** That deferral (see the 2026-09-xx entries
   above) was explicitly left open-ended — "once covers make it worth having" — and covers have
   shipped. **Decided: no, not now.** #124 is solved entirely with weight (bold vs. dim) inside
   the existing 16-color system; true color (or a cover-derived accent) is real scope for a
   separate issue, not something to fold in here just because the deferral's own condition
   technically became true.
3. **Where the bigger title lives.** The bottom bar's height (`BAR_HEIGHT = 5`) is a deliberate,
   tested invariant from #28 — "the bar's height never depends on what is playing," with a
   regression test pinning the quit-key row to the same screen row in every connection state.
   Growing the bar to fit a taller title would reopen that invariant. **Decided: the title lives
   in the header beside the cover instead** (`now_playing_header`, untouched by #28's own
   invariant), and that header is now reserved by screen size even with no cover/no image-capable
   terminal, so the bigger title is not conditional on `ratatui-image`'s own capability detection.
   The bar's own `connected_line()` keeps showing one compact line (state + `Artist - Title`, via
   the new `fmt::track_name` below) exactly as before.
4. **Whether splitting the artist out of `title` belongs in #124 at all.** It is really a
   prerequisite data-model change (a protocol version bump), not itself a layout change.
   **Decided: yes, but as its own PR, landed before any visual work**, so the two concerns (wire
   shape vs. presentation) stay cleanly separated and neither PR is stuck waiting on the other's
   review.

**Part 1 (this part, merged): protocol 1.8 → 1.9, no visible change.** The artist name was always
available from TIDAL at track-open time — `tidlers`' `Track.artist`/`Track.artists` — but
`TidalOpener::describe()` (`openers.rs`) joined it into `title` immediately
(`format!("{} - {}", artist.name, title)`), before the daemon, the protocol or the TUI ever saw
them apart. Catalog views (search/album/artist/library) already carried a real, structured
`artists: Vec<ArtistRef>` the whole time — this was never a TIDAL limitation, only something
thrown away specifically on the now-playing path.

Kept them separate instead: `SourceInfo.artist`/`QueueTrack.artist`/`engine::TrackMeta.artist`,
all `Option<String>`, threaded through `Queue::open_entry`'s existing title/cover/duration
fill-in (`.or(item.track.artist)`, same pattern as the rest) and `phoniad`'s `queue_add_from`
(joining multiple credited artists with `", "`, matching the catalog's own `fmt::artists`
convention, rather than only ever using the first). On the wire, `artist: Option<String>` is new
on `dto::Track`, `dto::QueueItem` and `proto::Event::TrackStarted`, each `#[serde(default)]` so an
older 1.8 client deserializing a 1.9 server's message just gets `None` and keeps working — `title`
itself changes meaning (no longer includes the artist), which is a real, if harmless, behavior
change for any 1.8 client: documented here rather than silently shipped.

**"No visible change" was a deliberate constraint on this part, not just a description**: a new
`phonia_ipc::fmt::track_name(title, artist, source)` (the shared formatting module every client
already uses for `ms`/`verdict`/etc.) recombines `title`+`artist` back into the same `Artist -
Title` shown before, with the same source/`"?"` fallbacks `title.as_deref().or(source)` used to
have — applied everywhere that string used to be built ad hoc: `phonia ctl status`/`queue
list`/`watch`, the no-daemon `phonia play`'s own status line and queue listing, and the TUI's
`now_playing_header`/`connected_line`/queue list. One regression test
(`an_album_is_added_with_the_titles_and_lengths_that_came_with_its_listing`) caught the one
genuine behavior change needed fixing before merge — it had asserted the old joined string, which
is now the correct place to assert `title`/`artist` as two separate fields instead.

Part 2 (merged, last) is the actual visual work, per decisions 1-3 above. `now_playing_header`
(`view/mod.rs`) splits what used to be one `fmt::track_name` line into the title alone, styled
`theme.accent` -- already bold (plus green, with color) since that style already existed for this
exact line; no new theme style was needed, only reusing it for less text. The artist, when known,
gets its own line beneath it in `theme.dim`, prefixed `◉ ` (a plain Unicode glyph, not a Nerd Font
icon or a downloaded image -- portable, and the only one of the three icon options that didn't
need a second image fetch or risk being illegible at 1-2 cells, per the investigation).

**The header is now reserved whenever a track is playing, not only when a cover could show.** A
new `HEADER_TEXT_ROWS = 4` fixed-height path in `draw_queue` sits alongside the existing
image-sized one: when `covers.picker()` is `None`, there's no cover id, or the image genuinely
doesn't fit, but a track is still playing, the header reserves exactly 4 rows (title, artist,
quality, the same trailing blank line it always had) at full width instead of reserving nothing.
This is deliberately *not* scaled to screen height the way the cover's own reservation is (`(area.
height / 3).clamp(6, 12)`): that scaling exists so a *picture* looks reasonably sized on a tall
terminal, which has no equivalent for plain text -- four lines of text never benefit from more
room, so the height is simply fixed at exactly what the content can ever be, content is never
taller than its own reservation. This follows the same "reserved on content existing, never on
content having arrived" discipline #24's own cover reservation established (`draw_queue`'s
existing comment on the image path), now applied to the no-cover path it never covered before.
`BAR_HEIGHT` and #28's own fixed-height-bar invariant are completely untouched: `connected_line()`
still shows one compact `Artist - Title` line via part 1's `fmt::track_name`, exactly as before.

A new test, `the_title_and_artist_show_above_the_queue_even_with_no_cover_at_all`, pins this down
directly (a track playing, no cover, `Covers::disabled()`, asserting both the title and `◉ Artist`
render, with the queue list still visible beneath). The existing
`with_covers_off_or_nothing_playing_the_queue_layout_is_exactly_as_before` test still passes
unmodified -- not because nothing changed (the header's presence genuinely did), but because that
test only ever compares two covers-disabled renders against *each other*, both of which gained the
new header identically, so the comparison it actually makes (whether a cover id with no picker
differs from no cover id at all) still holds.

**Verified live, not only with `TestBackend` unit tests**: a scratch daemon on a PipeWire null
sink, a real TIDAL track ("Sultans Of Swing," Dire Straits) played through a real pty, confirming
`Sultans Of Swing` rendered as its own bold line immediately followed by `◉ Dire Straits` on the
next line, with the queue list and the bar's own compact line both still showing correctly beneath
and below it.

This closes #124: both parts of the approved plan are merged.

## 2026-10-07 — #32 part 1: lyrics read raw, like search already does, not through `tidlers`

The next issue of Phase 4, body: "Mostrar letras sincronizadas con la reproducción en la TUI,
obtenidas a partir de la pista actual" (show lyrics synced to playback in the TUI, for the current
track).

TIDAL's endpoint is `GET /tracks/{id}/lyrics` on the same host `playbackinfopostpaywall` already
uses (`tidlers::urls::API_V1_LOCATION`), confirmed externally (the `minim` Python library's
documented `PrivateTracksAPI.get_track_lyrics`, and community LRC-export tools) before being
confirmed again directly against a real account in this part. The answer has `lyrics` (plain,
unsynced text) and `subtitles` (the time-synced version, LRC format: lines like
`[01:02.34]Some words`).

**`tidlers` 0.5.0 (already a dependency) wraps this exact endpoint, `TidalClient::get_track_lyrics`
(`~/.cargo/registry/.../tidlers-0.5.0/src/client/api/track.rs:276-294`) — but its `LyricsResponse`
type (`.../tidlers-0.5.0/src/client/models/track/mod.rs:64-72`) has no field for `subtitles` at
all**, confirmed by reading the struct directly, not inferred: only `track_id`, `lyrics_provider`,
`provider_commontrack_id`, `provider_lyrics_id`, `lyrics: String`, `right_to_left: bool`. No
`#[serde(deny_unknown_fields)]` either, so calling `tidlers`' own wrapper parses successfully and
silently drops exactly the field #32 is about. This is the same situation `catalog/remote.rs`'s own
module doc already describes for why it makes its own HTTP calls instead of using `tidlers`'
search/listing calls (required fields TIDAL sometimes omits, breaking the whole answer) — lyrics is
the second, independent case of it, not a new problem to solve differently. `TidalCatalog::
artist_bio`/`bio_from` (a 404 means "no bio," not an error) is the closest existing precedent and
is what `track_lyrics`/`lyrics_from` mirror directly, down to folding a 200-with-everything-blank
response into the same `None` a 404 gives (confirmed as a real, not hypothetical, case in part 2 of
#25/#26's own investigation style: never assumed, always checked against a real response before
shipping).

**Decided with the user** (recommended options throughout, via `AskUserQuestion`): (1) **pull, not
push** — a client asks for lyrics (a new `Request::Lyrics` protocol addition, part 2) only when it
actually opens the Lyrics panel or the track changes while that panel is open, rather than the
daemon fetching lyrics for every track regardless of whether anyone looks — avoids an unwanted
network call per track and keeps the protocol simpler (no new `Event`, no staleness-gating logic
like `SinkReport::applies_to` had to grow); the cost is a short loading state the first time a
track's panel opens, gone on reopening thanks to the daemon's own cache; (2) when there's plain
text but no synced version, show it unsynced with a "not synced" notice, rather than hiding lyrics
that do exist just because they aren't timed; a track with nothing at all says so plainly; a local
file says lyrics are TIDAL-only, since it has no TIDAL id to look up and guessing by title/artist
risks the wrong song's lyrics entirely; (3) caching is in-memory only, bounded, on the daemon
(matching `TidalOpener`'s own `delivered_tiers` precedent) — nothing persisted to disk, no
precedent for that anywhere in this project; (4) a fourth TUI section, `Section::Lyrics`, not an
overlay or a single-line hint under the title — synced lyrics follow playback on their own (current
line centered and bold, sung lines dimmed, no manual scroll needed), plain lyrics scroll manually
like the help panel already does; (5) right-to-left text (Arabic, Hebrew) is right-aligned and left
to the terminal to order correctly — `ratatui` does no bidi shaping of its own, and attempting it
by hand would be a lot of work for a corner case. A `CAP_LYRICS` capability bit is new (so an older
daemon or a client talking to one can tell lyrics support apart from catalog support generally),
settled without needing to ask, the same reasoning `CAP_QUALITY` already established when
`SetMaxQuality` shipped.

**Part 1 (this part, merged): `phonia-core` only, no protocol change.** `Catalog` (the trait both
`TidalCatalog` and `fake::FakeCatalog` implement) gains `track_lyrics(id) -> Result<Option<Lyrics>,
CatalogError>`; `Lyrics { lines: Vec<LyricLine>, plain: Option<String>, right_to_left: bool,
provider: Option<String> }`, `LyricLine { at: Duration, text: String }`. `catalog/remote.rs` adds
`RawLyrics`/`lyrics_from`/`parse_lyrics`, calling the endpoint through the catalog's own existing
`get()` helper (the same one `search`/`album`/`artist_bio`/etc. already use) — no new session or
credentials plumbing needed, it was already there. A new, pure `catalog/lrc.rs` module parses the
LRC text: one or more leading `[mm:ss]`/`[mm:ss.xx]`/`[mm:ss.xxx]` tags per line (a repeated chorus
line gets one `LyricLine` per tag), metadata tags (`[ar:]`, `[ti:]`, `[length:]`...) ignored,
`[offset:±ms]` shifting every timestamp from that point on, a timestamp with nothing after it kept
as an empty line (TIDAL's way of marking an instrumental gap — dropping it would make the display
look stuck on the previous line during that gap instead), a malformed line skipped without losing
the rest of the parse, output always sorted by time regardless of source order. `fake::FakeCatalog`
gains `with_lyrics(id, Lyrics)` and records the call like every other method does, for tests
upstream of this that need a track with (or without) lyrics.

**Verified live against a real account, not assumed from documentation alone**: Dire Straits'
"Sultans of Swing" (id `233059491`, the same track used throughout #30/#120's own real
verifications) parsed into 39 correctly time-ordered lines, provider `MUSIXMATCH`, first line
`12.44s "You get a shiver in the dark"` — matching the real song, confirming the LRC parser against
a genuine response rather than a hand-built fixture. A genuinely instrumental track (id `518338`)
correctly answered `None`, settling (for this account, this track) that a lyrics-less track folds
cleanly into the same `bio_from`-style handling rather than needing a separate code path — though
whether that specific track 404s or 200s-with-blank-fields on TIDAL's side was deliberately left
unlogged in the test output in favor of the already-correct `None` outcome either way, since
`lyrics_from`/`parse_lyrics` handle both identically by design.

Part 2 (not started) is the wire protocol: `Request::Lyrics`, `CAP_LYRICS`, protocol 1.9 → 1.10,
the daemon-side in-memory cache. Part 3 (not started) is the TUI panel itself.

## 2026-10-07 — #32 part 2: `Request::Lyrics`, the daemon's own cache, `ctl lyrics`

Protocol 1.9 → 1.10. `Request::Lyrics { id }`, answered with `Payload::Lyrics { id, lyrics:
Option<Lyrics> }` — `id` is repeated in the answer, the same reason `Payload::Tracks`/`Albums`
repeat the `from` they were asked about: a client that moved on to another track by the time a slow
answer arrives needs to tell "stale" from "current" without guessing from arrival order alone. New
wire DTOs `Lyrics { lines: Vec<LyricLine>, plain: Option<String>, right_to_left: bool, provider:
Option<String> }` and `LyricLine { at_ms: u64, text: String }`, mirroring `phonia-core`'s own types
from part 1 field for field except `at_ms` in place of a `Duration` (the wire is plain JSON
everywhere else too: `TrackSummary::duration_ms`, `Status::position_ms`, etc. — nothing here is a
new convention). A new `CAP_LYRICS` capability, announced only alongside `CAP_CATALOG` (no catalog,
no lyrics either): `phoniad::Daemon::hello()` pushes it right after `CAP_CATALOG`, matching how
`CAP_QUALITY` and the others were each added when their request shipped.

`Daemon::lyrics()` is read-only like `search`/`album`/`artist` (no `control_lock`), refused with
`ErrorCode::Unsupported` through the same `catalog_or_refuse` helper, and `server.rs` runs it
through `run_beside` alongside them: TIDAL may take a moment, and a slow lyrics fetch must not hold
up the rest of a connection's requests (covered by
`asking_for_lyrics_does_not_hold_up_the_requests_behind_it`, copying the existing
`asking_for_an_artist_does_not_hold_up_the_requests_behind_it`/`listing_an_album_does_not_hold_up_
the_requests_behind_it` pattern with a `.delayed()` fake catalog).

**The in-memory cache decided in part 1** is a new `LyricsCache` private to `daemon.rs`: a
`HashMap<String, Option<ipc::Lyrics>>` plus a `VecDeque<String>` for FIFO insertion order, capped
at 64 tracks (`LYRICS_CACHE_CAPACITY`) — hand-rolled rather than pulling in an `lru` crate, since
nothing else in the workspace depends on one and a 64-line structure needs no library for
eviction this simple. Critically, **only `Some`/`None` answers are cached, never a `CatalogError`**
— a transient TIDAL failure (rate limited, briefly unavailable) must stay retryable the next time
the panel is reopened, not stick as a cached failure until the daemon restarts; covered by
`a_lyrics_failure_is_not_cached_and_can_be_retried` (two failing asks, two calls reaching the fake
catalog) against `lyrics_answer_the_id_asked_for_and_are_cached_after_the_first_ask` (two successful
asks, one call). Lost on restart, same as every other daemon-side cache in this project — nothing
here is persisted to disk.

A new `phonia ctl lyrics [<id>]` prints synced lines as `[m:ss] text` (via the existing
`phonia_ipc::fmt::ms`) or the plain text with a "not synced" note, falls back to the track playing
now (its `tidal:` source, stripped of the prefix) when no id is given, and refuses plainly for a
local file playing (no TIDAL id to look up) or nothing playing at all — the same shape
`require_catalog` already gives `album`/`artist`/`library`, extended with the extra `CAP_LYRICS`
check since an older daemon could have `CAP_CATALOG` without it.

**Verified live against the real daemon and the real account** (this part's end-to-end check,
standing in for a client exercising `Request::Lyrics` before the TUI exists to do it): `phonia ctl
lyrics 233059491` printed all 39 lines of "Sultans of Swing" timestamped `[m:ss]`, ending with
"Lyrics via MUSIXMATCH"; `phonia ctl lyrics 518338` (the instrumental) answered "TIDAL has no
lyrics for this track"; with Sultans of Swing queued and playing, `phonia ctl lyrics` with no id
resolved to the same 39 lines automatically.

Part 3 (not started, last) is the TUI panel itself: `Section::Lyrics`, current-line tracking for
synced lyrics, manual scrolling for plain ones.

## 2026-10-07 — #32 part 3 (last): one scroll offset, auto or manual, for both kinds of lyrics

A fourth sidebar section, `Section::Lyrics` (key `4`, `Group::Panels`), closing #32. Planned with
Opus from a read-only Explore investigation of `phonia-tui`'s existing precedents (`view/mod.rs`'s
`draw_main` dispatch, `queue_lines`' current-row styling, `view/library.rs`'s loading/failed/done
pattern, `cursor.rs`'s lack of a stored viewport offset, `Found<T>`'s TIDAL-pagination shape, and
the project's several existing "is this answer stale" mechanisms); the user then chose, against the
plan's own recommendation, to allow overriding the auto-follow of synced lyrics by scrolling
manually (not just of unsynced plain text) — the rest of this entry reflects that choice, not the
original recommendation.

**State** (`crates/phonia-tui/src/lyrics.rs`, new): `LyricsState { id, generation, phase:
browse::Phase, lyrics: Option<phonia_ipc::Lyrics>, scroll: Option<usize> }`, one held in
`app::State.lyrics`, replaced (never mutated in place) whenever a fetch starts for a different id —
a track change while the panel is open drops the old track's lyrics and shows "Loading..." for the
new one at once, not the old lyrics under the new title, since lyrics are pulled per track, not
merged. `browse::Phase` (already shared by the library and every browse view) is reused rather than
inventing a fourth loading-state enum.

**One scroll model for both kinds of answer.** `LyricsState.scroll: Option<usize>` is `None`
(auto) until the user scrolls, then an explicit row offset kept until the track changes. With no
override, synced lyrics center on the line being sung (`lyrics::current_line` -- the last line
whose timestamp has passed, by `partition_point`; `lyrics::centered_first` -- `current -
rows/2`, clamped) and plain text starts at the top (`lyrics::auto_offset`, 0 for plain, computed
for synced). `app::scroll_lyrics` (mirroring `scroll_help`'s shape) computes the same baseline,
applies the pressed key's delta, clamps to `lyrics::max_scroll` (the line count, or the plain
text's line count, less however many rows the panel has), and stores the result as the new
override. This was deliberately **not** two separate mechanisms (an auto-follow cursor for synced,
a stored offset like `help_scroll` for plain) since one model that can be either computed or
overridden covers both without a "normally computed, but sometimes not" special case in the view
itself, and it is what let scrolling-with-override fall out of the help-scroll precedent almost for
free once the user asked for it.

**A bug the live check against a real daemon caught, not a review**: the first version set
`scroll` to `Some(new)` on every keypress, including one that happened to change nothing (nowhere
yet to scroll, every line already fit on screen). That quietly locked the panel out of auto-follow
from then on, invisibly, until the terminal was later resized smaller and it stopped centering on
the current line for no apparent reason. Fixed by only committing to manual mode when the computed
offset actually differs from what auto-follow is showing right now
(`current.scroll.is_none() && new == auto` stays in auto mode) -- caught by testing the TUI live
against the user's own already-running daemon (a second, read-only client, careful not to send any
request that would disturb its paused track), not by the unit tests, none of which happened to
resize the terminal mid-session the way a real one can.

**Fetch trigger**: one hook, `app::maybe_load_lyrics`, called from `update()` right after
`maybe_load_library`'s own (same "ask the moment the section is shown, not on a dedicated key"
shape). It fetches when the Lyrics section is open, the connection has `CAP_CATALOG` and the new
`CAP_LYRICS`, the current track has a TIDAL id (`lyrics::tidal_id`, stripping `tidal:` the same way
`phonia ctl lyrics` already does), and either nothing is held for this id yet, it is a different
id, or `retry` is set and the last fetch failed. `retry` is true on entering the section and on
reconnecting while already in it (the daemon caches `Some`/`None` but never a failure, so a failed
fetch is worth trying again without a dedicated retry key) -- leaving the panel and returning is
the other retry path. Pull, never pushed: a `TrackStarted` while the panel is closed changes
nothing, since the hook only acts when the section is actually shown.

**Staleness**: `Tag::Lyrics { generation }` (not the id itself -- `Tag` derives `Copy`, which a
`String` field would break). `on_response` drops an answer whose generation no longer matches
`state.lyrics`'s, the same pattern `Tag::Search`/`Tag::Library` already use, rather than a new one.

**Rendering** (`crates/phonia-tui/src/view/lyrics.rs`, new): the current line is marked `>` (not
`▶`, so it reads identically on a terminal with poor Unicode support, and matches the exact glyph
`queue_lines` already uses for "the entry playing") in `theme.accent`; lines already sung are
`theme.dim`; upcoming ones are plain `theme.text` -- the same three-tier styling `queue_lines`
established for "selected / current / plain", just without a `theme.selected` tier, since there is
no cursor to select a row with here. Right-to-left text is right-aligned per line
(`Line::alignment(Alignment::Right)`), no bidi shaping attempted. The provider credit
("Lyrics via {provider}") is a dim footer row outside the scrollable body when there is one; an
unsynced plain answer gets a dim notice row above it. No lyrics at all, a local file, a missing
capability, and a failed fetch each say so in place of the body, the same message styles
`view/library.rs` already established for its own no-data cases.

**Verified live against the real daemon, the real account, and a track actually playing** (not a
synthetic fixture): with the user's own `phoniad` already running a paused track (Watain, "De
Profundis"), `phonia tui` connected as a second, read-only client (no playback-affecting key
pressed, so as to not disturb the session already in progress) showed its real synced lyrics,
correctly auto-centered on the line being sung, "Lyrics via MUSIXMATCH" credited at the bottom --
and, as above, is what caught the auto-follow-lockout bug before it shipped.

## 2026-10-08 — #39 part 1: TIDAL's real playlist-folder API, read into `phonia-core`

Planned with Opus (Phase 4, next after #32). Nothing in the codebase or in `tidlers` wraps this
endpoint usefully, so the plan itself had to investigate TIDAL's real v2 "My Collection" API
first. `tidlers-0.5.0` (already vendored) has unused calls against
`https://api.tidal.com/v2/my-collection/playlists/folders/*` (create-folder, remove,
flattened-folders-only) but no call to list a folder's actual contents, and its own models
require every field — the same fragility that already justified bypassing `tidlers` for every
other catalog call (see this file's 2026-09-27 entry). The real listing call, confirmed live
against this account below, is `GET /my-collection/playlists/folders` with
`folderId=root|<uuid>`, `order`, `orderDirection`, `offset`, `limit`.

**User's 4 decisions, all = recommended:** (1) read-only for now — show the folder tree already
built in TIDAL's own app; creating/renaming/moving a folder from the TUI is a separate future
issue, same reasoning as #21 leaving favorite-editing out of scope; (2) a folder's contents show
a playlist the user only *follows* exactly like TIDAL's own app does, not filtered down to only
the ones they created — a deliberate, scoped exception to `my_playlists`'/#21's "owned only"
rule, which stays as-is for the flat list; (3) the "Your playlists" library tab becomes the root
of the folder tree (sub-folders first, then loose playlists) rather than a separate tab; (4)
sorted by name, folders before playlists, for a tree that reads top-to-bottom rather than a feed.

**Real investigation against this account** (a test folder named "test" with one followed
playlist inside, made for this check): the v2 answer's envelope is
`{"items":[...],"totalNumberOfItems":N,"lastModifiedAt":"..."}` — unlike every v1 listing already
used here, it never echoes back an `offset` (and has no cursor either, despite `tidlers`' own
`FolderListResponse.cursor` field suggesting one), so `parse_folder_page` keeps the caller's own
requested offset rather than trusting a field that isn't there. Each item is
`{"trn","itemType":"FOLDER"|"PLAYLIST","name","parent","data":{...}}`; a `FOLDER`'s `data` has
its own `id`/`name`/`totalNumberOfItems`; a `PLAYLIST`'s `data` is shaped exactly like the
existing `RawPlaylist` (`uuid`, `title`, `creator`, `numberOfTracks`, `duration`, `squareImage`),
reused as-is rather than duplicated. Confirmed live: the one playlist inside "test" has
`creator: {"id":0,"name":null,"type":"TIDAL"}` — a followed, not owned, playlist — which is what
decision (2) above is about, and is why `playlist_folder` does not apply `my_playlists`'
creator-id filter.

**`catalog::FolderEntry`** (`Folder { id, name, item_count }` or `Playlist(Playlist)`) and
`Catalog::playlist_folder(folder: Option<String>, offset, limit)` (root when `folder` is `None`)
added to the trait, `TidalCatalog` (new `get_v2`, parallel to the existing v1 `get`, since this is
the first v2 call the project makes) and `FakeCatalog` (`with_folder(folder, entries)`, keyed by
`Option<String>`). Unknown item types (TIDAL may add kinds beyond folder/playlist later) are
skipped, not a hard failure, matching every other listing's own "one bad item doesn't fail the
page" rule.

**3-part plan, this is part 1**: part 2 is the wire protocol (1.11, additive:
`Request`/`Payload::PlaylistFolder`) and the daemon handler; part 3 is the TUI — the "Your
playlists" tab becomes the folder root, opened and nested through the existing `browse::Stack`
exactly like an album or artist already is (a sub-folder is just another pushable view), closing
#39. A follow-up issue, not scoped here, would cover creating/renaming/moving folders through the
v2 API's own `create-folder`/`remove` calls `tidlers` already wraps.

Verified live end-to-end in `a_real_playlist_folder` against the real account (root, then the
real "test" folder's one followed playlist) — see that test's own doc comment for the exact
confirmed shape. Full workspace green, fmt+clippy clean.

## 2026-10-08 — #39 part 2: wire protocol 1.11 and the daemon handler

`Request`/`Payload::PlaylistFolder { folder, offset, limit }` (additive, protocol 1.10 → 1.11),
under the existing `catalog` capability — no new capability, same as `Playlists`/`Library`,
since `playlist_folder` costs exactly as much as those already do and nothing about it needs its
own feature flag. `dto::FolderEntry` is tagged (`{"type":"folder"|"playlist",...}`); its
`Playlist` variant wraps the existing `PlaylistSummary` as an internally-tagged newtype rather
than duplicating its fields — confirmed by the golden test that serde merges the tag with the
wrapped struct's own fields exactly as hoped, with no special-casing needed. An `Unknown` variant
covers an item type a future daemon/TIDAL adds and this client doesn't know, same as every other
tagged wire enum here.

The daemon's `playlist_folder` handler is a direct mirror of `playlists`/`library`: refused with
`Unsupported` when there is no catalog, paged the same way, `convert::folder_entry` mapping the
core `FolderEntry` to the wire one. It is dispatched the same way `Playlists`/`Library` already
are (the default `Daemon::handle` path), not through `run_beside` — matching its two closest
siblings by request shape and cost, rather than `Lyrics`/`Search`, which do use `run_beside` for
reasons specific to them.

`phonia ctl folder [<id>]` was added alongside (not originally scoped as its own PR step, but
every previous protocol addition here has shipped with matching `ctl` support in the same
breath, so this followed that precedent): prints a folder's contents, sub-folders first, each
row ending with the id to open it (`folder <id>`) or queue it (`playlist <id>`). No id means the
root of "My Collection".

Verified live end-to-end against the real account: a scratch daemon on a throwaway PipeWire null
sink, `phonia ctl folder` showed the real "test" folder, `phonia ctl folder <its id>` showed the
real followed "Dark Jazz" playlist inside it, `--json` printed the matching wire shape — then shut
down cleanly, sink unloaded, nothing left running. Full workspace green (fmt+clippy clean); new
tests in `phonia-ipc`'s golden suite (the request/response pair, the version bump), `phoniad`'s
protocol suite (folder contents told apart from a sub-folder, opening a sub-folder by id, the
no-catalog refusal, added to the existing "none of these work without a catalog" list), and
`phonia`'s own `ctl` formatting tests.

Part 3 (next, last): the TUI — the existing "Your playlists" library tab becomes the root of the
folder tree, nested through the same `browse::Stack` an album or artist already nests through.
Closes #39 once merged.

## 2026-10-08 — #39 part 3 (last): the Playlists tab becomes the folder tree, closing #39

A new `browse::FolderView` (`folder: Option<String>`, `name`, `phase`, `entries: Found<FolderEntry>`)
and `View::Folder`, pushed onto the exact same `library_views: Stack` an opened album or artist
already nests through — a sub-folder opens, and nests further, for free from `Stack` alone, no
new nesting mechanism. A playlist inside a folder opens exactly like one anywhere else
(`open_track_list`); a sub-folder opens into another `FolderView` (`open_folder_view`); both are
reached through one shared `act_on_folder_entry`, called from the library's own root level and
from an already-opened `FolderView` alike, so the two levels (root vs. nested) share identical
Enter/`a`/`A` behavior rather than two parallel implementations. Read-only, as decided in part 1:
`a`/`A` on a sub-folder do nothing (there is no sensible "add a whole folder" without recursing).

**The library's root level needed its own request, not a reuse of `Payload::Library`.** The
"Your playlists" list used to come free with `Request::Library`'s own `my_playlists` field; that
field is flat and owned-only, the wrong shape for a folder root, and changing its *type* on the
wire would silently break an older client's parsing instead of being additive — so it stays on
the wire untouched (an older client, or `phonia ctl library`, still sees the old flat list) and
is simply no longer read here. The root folder is fetched by its own, already-existing
`Request::PlaylistFolder { folder: None, .. }` (from part 2), fired at the exact same moment as
`Request::Library` (`Tag::LibraryPlaylists`, a new tag paired with `Tag::Library`, both sharing
`LibraryState.generation` so a reconnect invalidates both together). This is also why
`LibraryState` grew a second, independent `playlists_phase: Phase`: the Playlists tab's own
"loading / failed / done" no longer has to agree with the other two tabs' `phase`, since it is
genuinely a separate request that can succeed or fail on its own schedule. Pagination needed no
new mechanism at all: `next_page`/`add_page`/`page_failed` were already generic over which tab,
so the Playlists tab's "more" page just asks `Request::PlaylistFolder` instead of
`Request::Playlists` through the exact same `Tag::LibraryMore` path everything else already used.

**`library::Selected` collapsed `Playlist(&PlaylistSummary)` into `Entry(&FolderEntry)`.** The old
variant only ever came from the flat list, which no longer exists as something to select from;
every folder-tree row (a sub-folder or a playlist, owned or followed) is now one `FolderEntry`,
read the same way regardless of position (library root or a nested `FolderView`).

**Real investigation done, one real limitation hit and accepted, not solved.** The code itself
was verified thoroughly: 308 `phonia-tui` unit/`TestBackend` tests (up from 280), including the
exact nested flow (open a sub-folder, see its real contents, page past 50, open a playlist found
inside it, close back down to the root) and a screen-rendering test asserting a folder row
("`Moods/ (1 item)`") reads differently from a playlist row on the same list. A live, real-account
end-to-end attempt was also made for this part (beyond part 1/2's own real-API/real-protocol
checks): a scratch `phoniad` on a throwaway PipeWire null sink, `phonia tui` driven through a
forked pty sending the real key sequence (library → Playlists tab → the real "test" folder →
the real "Dark Jazz" playlist inside it → back out → quit). The daemon side answered correctly
throughout (confirmed separately via `ctl folder` in part 2) and the TUI process never panicked
or died across the whole sequence — but the pty harness could not be made to show the *rendered*
screen text at all this time: the TUI's own startup handshake (terminal capability queries for
truecolor/graphics support, e.g. a Kitty graphics protocol probe and a cursor-position report)
blocks waiting for responses a bare `pty.fork()` harness never sends, so nothing past that
handshake was ever captured. This is the same class of pre-existing, already-documented pty
limitation noted in earlier sessions (a `q` keypress not reliably observed exiting the process
through this same kind of harness) — not a regression from this change, and not chased further
here either, for the same reason: the process-survives-every-keypress check it CAN do still
passed, and the actual logic is already the most heavily unit-tested part of this whole issue.

Full workspace green, fmt+clippy clean. README/ROADMAP/DECISIONS.md all updated — **this closes
#39**, all 3 parts (PRs to follow this entry).

## 2026-10-08 — #33 part 1: `Catalog::track_radio`, using `/tracks/{id}/radio` over the two-step mix

Planned with Opus (Phase 4, next after #39). The planning investigation couldn't run live (read
only), so the plan's own first step — confirming the real shape — became part 1's own first
action, the same escalation #39's own PR1 used.

**User's 4 decisions, all = recommended:** (1) scope is on-demand track radio (browse/queue) plus
autoplay; personal "My Mixes" (a different, uninvestigated set of endpoints) is a future issue;
(2) autoplay defaults to `off`, with a runtime toggle that is not persisted — same shape as
`SetMaxQuality`; (3) the autoplay seed is the last track that played, refilling with ~10 tracks
once the last queued entry starts (not a fixed original seed, not a whole 100-track page at once);
(4) autoplayed tracks are plain queue entries, no new `QueueItem` field.

**Real investigation, live against this account** (seed `33723914`, Korn - "Falling Away from
Me", a track id already known real/streamable from this file's own earlier album-listing
checks): `GET /tracks/{id}/mix` returns just `{"id":"<mixId>"}`; that mix's own
`/mixes/{mixId}/items` and `GET /tracks/{id}/radio` directly were, as far as checked, the same
100-track list — `radio` gets there in one request instead of two, which matters because autoplay
will call this repeatedly, so `track_radio` uses `/tracks/{id}/radio` and the mix endpoints are
not wired up at all. **Confirmed, and the reason for `parse_radio`'s one real behavior**: the seed
track itself always comes back as the FIRST item of its own radio — `Catalog::track_radio` filters
it out (`item.id != seed`) before returning, since nothing that asks for a track's radio wants
that same track handed back; `total` stays TIDAL's own count regardless (same "a page shorter than
the total says is fine, that's TIDAL's count" precedent `parse_track_items` already set for
videos left out of an album/playlist listing). Also confirmed: `radio`'s `items` are bare tracks
(no `{"item":...}` envelope, unlike a playlist's or an album's own listing) — already handled by
the existing `parse_track_items`, which tolerates either shape, so no new parsing had to be built,
only a thin `parse_radio` wrapper that filters the seed.

**6-part plan**, this is part 1: `Catalog::track_radio(id, offset, limit) -> Page<Track>` added to
the trait, `TidalCatalog` (`/tracks/{id}/radio`, `parse_radio`) and `FakeCatalog`
(`with_radio(seed, tracks)`, filtering the seed the same way so a test against the fake behaves
like one against the real catalog). Part 2 is the wire/daemon addition — a new
`CatalogRef::TrackRadio { id }` reusing the EXISTING `Request::Tracks`/`Payload::Tracks` and
`Request::QueueAddFrom` machinery (which already pages/queues any `CatalogRef`), so this is
expected to need no new protocol request type, only a new tagged variant plus one daemon dispatch
arm, cheaper than #39's own protocol bump. Part 3 is `ctl`/the TUI's on-demand "open this track's
radio" action. Parts 4-5 are the autoplay setting and its real behavior: the architectural
decision (found by reading the actual code, not assumed) is that autoplay belongs entirely in
`phoniad`, hooked into the EXISTING prefetch mechanism (`maybe_prefetch`, which already looks ~30s
ahead and opens the next track for gapless joins) rather than reacting to `Event::QueueExhausted`
-- by the time the queue is reported exhausted the engine has already released the DAC, so
reacting there would cost a gap and a re-acquire; appending tracks while the last queued entry is
still playing keeps gapless intact. `Repeat::One`/`Repeat::All` never trigger it, by construction
(the queue's own `peek` cannot reach "nothing more" in those modes unless the queue is genuinely
empty). Part 6 is the TUI's autoplay toggle, closing #33.

Verified live end to end against the real account (`a_real_track_radio`, `--ignored`). Full
workspace green, fmt+clippy clean.

## 2026-10-08 — #33 part 2: a track's radio needed no new wire request, only a new `CatalogRef`

A new `CatalogRef::TrackRadio { id }` (protocol 1.11 → 1.12), dispatched in the one place
(`track_page` in `daemon.rs`) both `Request::Tracks` (paging) and `Request::QueueAddFrom`
(queuing a whole list) already go through for every other kind of list — unlike #39's own
`PlaylistFolder`, which needed a genuinely new request/payload shape because `FolderEntry` isn't a
`Track`, this one slots into the existing `Track`-shaped machinery with a single match arm, no new
`Request`/`Payload` variant at all. `phonia ctl radio <id>` (new, lists it) and `queue add
radio:<id>` (new prefix in `catalog_source`, alongside the existing `album:`/`playlist:`) are the
only other additions.

Verified live end to end against the real account: a scratch `phoniad` on a throwaway PipeWire
null sink, `phonia ctl radio 33723914` printed a real nu-metal radio list (Slipknot, Limp Bizkit,
Pantera, System of a Down...) with the seed itself correctly absent; `--json` confirmed the wire
shape; `queue add radio:33723914` added all 100 real tracks to a real queue. Full workspace green
(2 new daemon protocol tests, 1 new golden test, 2 new `ctl` unit tests), fmt+clippy clean.

Next: the TUI's on-demand "open this track's radio" action (part 3), then the autoplay setting
and its real behavior (parts 4-5), then the TUI's autoplay toggle (part 6, closing #33).

## 2026-10-08 — #33 part 3: `o` opens a track's radio in the TUI; queue/now-playing deferred

A new `Action::OpenRadio` (key `o`, `Group::Search` like `a`/`A`, which also already work beyond
the Search section) and `browse::Header::Radio { title }` (title precomputed as `"Radio: <seed
track>"`, so `Header::title()` keeps returning a plain `&str` like its other two variants; `cover()`
is always `None`, a radio is not itself a TIDAL object with its own artwork). `open_track_radio`
is a two-line wrapper around the EXISTING `open_track_list` — a track's radio reuses the exact
same `browse::TrackListView`/`Stack` machinery an album or a playlist already opens into, so
nesting (a radio opened from inside an already-open album, or even from inside another radio)
falls out for free, same precedent as #39's folders nesting through the same stack.

`OpenRadio` is handled at the same three places that already resolve "the row under the cursor"
for `Activate`/`AddToQueue`/`AddNext` -- `act_on_result` (search), `act_on_library_result`
(library), `act_on_track_row` (a track already inside an open view) -- each gaining one early
branch: a `Track` row opens its radio, anything else (an album, a playlist, an artist, a
sub-folder) does nothing. Found and fixed one real latent bug while wiring this:
`act_on_folder_entry`'s own non-`Activate` branch for a `FolderEntry::Playlist` row unconditionally
built an "add to queue" request for ANY action that wasn't `Activate` -- harmless while the only
two non-`Activate` actions that could reach it were `AddToQueue`/`AddNext` (both correctly meaning
"add"), but `OpenRadio` reaching the same branch would have silently queued the whole playlist
instead of doing nothing. Fixed by checking for `AddToQueue`/`AddNext` explicitly rather than
"anything that isn't Activate".

**Deliberately out of scope for this part, and said so rather than silently skipped**: opening a
queue entry's or the now-playing track's own radio. Both need somewhere to SHOW the opened radio
that neither the Queue section nor a global "now playing" view currently has (`active_stack_mut`
only knows a stack for `Section::Search`/`Section::Library` today) -- building that is its own
small design decision (a new Queue-section stack? jump over to Search/Library and open it there?),
not something to improvise silently inside an unrelated implementation PR. Confirmed with a
regression test (`o_does_nothing_on_a_queue_entry_a_deliberate_follow_up_not_this_part`) that this
is the current, intended behavior, not an oversight to be caught by a future bug report.

4 new tests (12 total for this part across the 3 entry points plus the folder-bug regression and
the queue no-op), 312 `phonia-tui` tests passing (up from 308). Full workspace green, fmt+clippy
clean. No live pty-driven TUI check attempted this time -- #39's own part 3 already hit and
documented the same startup-handshake limitation in this exact harness; nothing new to learn from
repeating it, so the (thorough) unit/TestBackend coverage carries this part's verification weight,
same reasoning already recorded there.

Next: the autoplay setting (part 4, no behavior yet), then its real behavior in `phoniad` (part
5), then the TUI's autoplay toggle (part 6, closing #33).

## 2026-10-08 — #33 part 4: the autoplay setting, no behavior yet

Modeled like `shuffle`/`repeat`, not like `replaygain`: `Inner`/`Queue` grew an `autoplay: bool`
field, changeable at runtime (`Queue::set_autoplay`, a new `Request::SetAutoplay`), not a
config-file-only, set-once-at-startup value — the approved decision was a runtime toggle (like
`SetMaxQuality`), unlike `replaygain`, which genuinely has no runtime setter. `[playback]
autoplay` (default `false`, same "deliberate opt-in" reasoning as `replaygain`'s own default)
supplies the startup value, applied once via `queue.set_autoplay(...)` right after construction,
exactly where `replaygain`'s own startup value already is.

The flag travels on the existing queue snapshot (`ipc::Queue.autoplay`, `#[serde(default)]`,
protocol 1.12 → 1.13) rather than needing a new event: it is broadcast on `QueueChanged` for free,
the same way `shuffle`/`repeat` already are. A new `autoplay` capability is advertised only
alongside `catalog` (confirmed by a dedicated test: present when there's a catalog, absent
without one), since the behavior this setting will eventually enable needs TIDAL to fetch more
tracks from — there would be nothing for the flag to actually do on a daemon with no login.

`phonia ctl autoplay [on|off]` (no argument flips it, reading the current value from
`client.queue()` first) reuses the existing `MuteMode` (`On`/`Off`/`Toggle`) clap enum rather than
a near-duplicate — it is already exactly the shape this needs.

Verified live end to end against the real account: a scratch `phoniad` on a throwaway PipeWire
null sink, `ctl autoplay on` then a plain `ctl autoplay` (toggle) correctly flipped
`true` → `false`, visible in `ctl queue list --json`'s `autoplay` field each time, with the
queue's `version` incrementing only on a real change (confirmed by a dedicated unit test:
setting it to what it already was does not bump the version, same invariant `shuffle`/`repeat`
already hold).

This part is deliberately inert otherwise: nothing reads the flag to actually do anything yet.
Full workspace green (new tests in `phonia-core`'s queue, `phonia-ipc`'s golden suite including a
1.12-compat check that an older `Queue` answer still parses with `autoplay: false`, `phoniad`'s
protocol suite, `phoniad`'s own `convert`/`daemon` unit tests), fmt+clippy clean.

Next: the real behavior in `phoniad` (part 5) — hooked into the engine's existing prefetch
mechanism, not `Event::QueueExhausted`, so the DAC is never released and re-acquired for it, per
the architecture already settled in part 1's plan. Then the TUI's autoplay toggle (part 6,
closing #33).

## 2026-10-08 — #33 part 5: autoplay's real behavior, no engine change

Planned with Opus a second time for this part specifically, with full code access this time
(part 1's own plan was written without it). **Correction to part 1's own note**: that entry said
autoplay would be "hooked into `maybe_prefetch`". Reading the real code found this unnecessary —
`maybe_prefetch` already re-peeks the queue on every position tick, and does nothing (no cached
"nothing more" state) when it finds `Peeked::End`. So appending tracks while the queue's last
entry is already playing is picked up by the engine entirely on its own, gapless, the next time
it ticks — confirmed directly by a new `phonia-core` engine test
(`a_track_added_while_the_last_one_plays_is_picked_up_without_stopping`) that adds a second track
only after the first is already mid-write, blocked, and asserts one drain, two `TrackStarted` with
`gapless: [false, true]`, and the exact continuous sample stream. **The engine needed zero
changes for #33.** All the real work is in `phoniad`.

**The actual trap, confirmed by reading `audio_thread.rs`**: `Event::QueueExhausted` only fires at
the very end of `advance()` returning `None`, by which point `stop()` has already run and released
the DAC. Reacting there would be too late — autoplay has to notice *before* that, while the
engine is still playing the last entry, so the fetch has time to land before `QueueExhausted` ever
happens.

**Design**: `Inner::autoplay_due(&self) -> Option<ItemId>` (new, pure): the current entry, if
autoplay is on, repeat is off, and `peek(Advance::Auto)` already says `End` (which only Repeat::Off
can ever do — One always has something, All wraps — so checking repeat explicitly here is belt
and suspenders, not load-bearing). Exposed as `Queue::autoplay_due()`, read by `phoniad` from two
places: every `Event::TrackStarted` (the normal case) and every `Event::QueueChanged` (catches the
condition becoming true *without* a fresh start — autoplay turned on mid-track, repeat switched to
off, or the tracks that followed removed; confirmed with the user as wanted, not just a
side-effect).

A new, pure `phoniad::autoplay::Autoplay` state machine (unit-tested on its own, no network, no
locks) owns: which entry a fetch is running for, a generation number that makes a stale fetch
(superseded by a newer entry's fetch, or an explicit Stop/`QueueClear`) tell itself apart from the
one that matters, whether the queue ran dry *while* that fetch was in flight (so it should resume
playback once it lands), and the last TIDAL track that actually started (the seed when the newly
last entry is a local file, which has no radio of its own). `Daemon::consider_autoplay` (sync,
cheap) decides whether to start a fetch at all; `Daemon::run_autoplay` (spawned, async) does the
real `catalog.track_radio` call *without* holding `control_lock` (so it never holds up another
client's request), then takes `control_lock` only to re-check and act, the same ordering
`queue_add_from` already uses for every other async-resolved addition. The re-check after the
fetch resolves is real, not a formality: it re-reads `autoplay_due()` and the engine's state, so a
user who skipped, queued their own tracks, flipped repeat or autoplay off, or cleared the queue
while the fetch was in flight is respected — the fetch's answer is simply dropped. `autoplay::pick`
skips anything already in the queue, and anything TIDAL's own radio repeats within the same page,
capped at 10 (`BATCH`) per refill.

**The one deliberately accepted gap, per the user's own confirmed choice**: if the last queued
entry is short enough (or the network slow enough) that `QueueExhausted` fires — DAC released —
before the fetch lands, the daemon resumes playback on the first added track once it does
(`Controller::play_item`), costing one real gap and one hardware re-acquire, rather than silently
failing to autoplay in that case. A narrower, accepted-but-not-built-around window remains: if the
fetch lands in the handful of milliseconds between the engine's `advance()` returning `None` and
`fan_in` actually handling `QueueExhausted`, the tracks are still added, just without the resume —
playback waits for the user to press Play. Not chased further; it is a race measured in
milliseconds against a real network call.

**Refactored, no behavior change**: `accepted_from_catalog`/`queue_track` extracted from
`queue_add_from`/`add_to_queue`'s own inline logic (same exact rules — unstreamable tracks
refused with the same message — just now shared with `run_autoplay`, which needs the identical
"a TIDAL `catalog::Track` becomes a queue entry" conversion).

**Test coverage, and one gap acknowledged rather than hidden**: the state machine
(`phoniad::autoplay`) and the engine premise are both unit-tested directly. The actual `fan_in`
wiring (`consider_autoplay`/`run_autoplay` reacting to a real `TrackStarted` for a TIDAL-sourced
entry) is **not** covered by an automated daemon-level test: `tests/protocol.rs`'s own fixture
builds every daemon with `DispatchOpener::new(None)`, which makes a `tidal:` source fail to open
by construction (confirmed by reading `DispatchOpener::open`), so no external test in that file
can ever make a real `TrackStarted` fire for one — and `Daemon::autoplay`'s private field means an
in-crate harness would be needed instead, which was judged, after the fact, not worth building
for this PR (it would re-implement a large slice of `tests/protocol.rs`'s own fixture machinery to
exercise wiring that is otherwise thin glue over already-tested pure functions). **Verified live
instead, against the real account**, which exercises the real wiring end to end more convincingly
than a mock would: a scratch `phoniad` on a throwaway PipeWire null sink —
- `autoplay on`, `repeat off`, one real track queued and played: within ~2 seconds, 10 real tracks
  from its actual TIDAL radio appeared in the queue, none of them the seed.
- `repeat all`: no fetch.
- `autoplay off`: no fetch.
- `Play` immediately followed by `Stop`: the in-flight fetch was cancelled, nothing was added, and
  the engine stayed stopped (no spurious resume).

Full workspace green (phonia-core gained 6 new tests: 5 for `autoplay_due`, 1 pinning the engine
premise; `phoniad` gained 9 for the `autoplay` module), fmt+clippy clean. No protocol bump — the
wire was already finished in part 4.

Next, and last: the TUI's own autoplay toggle (part 6), closing #33.

## 2026-10-08 — #33 part 6 (last): the TUI's autoplay toggle, closing #33

A new `Action::ToggleAutoplay` (key `O`, `Group::Playback`, alongside `s`/shuffle and
`r`/repeat), reusing the exact same `request_for`/`send_playback` plumbing shuffle and repeat
already go through -- a thin, mechanical addition, same shape as `ToggleShuffle`/`CycleRepeat`.
Refused (same wording as the TUI's other `CAP_*`-gated actions) without the `autoplay`
capability, which only a daemon with a catalog ever advertises. Shown in the bar next to
`shuffle`/`repeat N` whenever it is on, reading `Queue.autoplay` the same way those two already
do -- no new state to track in the TUI, it already had everything it needed once part 4 put the
flag on the wire.

Deliberately paired with part 3's own `o` (lowercase, opens a track's radio): `o` for one radio,
`O` for continuous autoplay, the same letter at two cases for two related but distinct actions.

New tests: `app.rs` (the capability gate, and the flag flipping true then false), `view/mod.rs`
(the bar shows `autoplay` alongside the other flags). Full workspace green, fmt+clippy clean. No
live pty-driven TUI check -- the same startup-handshake limitation already hit and documented in
#39's and #33's own earlier TUI parts; nothing new to learn from repeating it against a toggle
this thin, verified instead by the unit tests above plus part 5's own live, real-account
verification of the behavior this toggle controls.

**This closes #33**, all 6 parts merged: on-demand track radio (browse, queue, and the TUI's `o`)
plus autoplay (the setting, its real behavior, and the TUI's `O`). Personal "My Mixes" remain a
deliberately separate future issue, as scoped back in part 1.

## 2026-10-08 — #139 part 1: a real pre-existing bug found while planning the Home screen

New issue, new milestone (Fase 7 — Home screen and discovery, #8), scoped explicitly to option (a)
of a choice the user made outside any existing issue: a lightweight home screen using only data
phonia already fetches locally (resuming, favorites, playlist folders), with a real
TIDAL-style editorial/personalized home (mixes, new releases) left as a deliberately separate,
not-yet-investigated future issue — the same "My Mixes" territory #33 already left alone.

Planned with Opus (read-only investigation of the TUI's own section architecture, not code it had
touched before this issue). **Headline finding, not about Home at all**: `act_on_track_row`'s
`Activate` branch (the function that computes, for a track inside an *already open* album,
playlist, folder or radio, how many tracks before it could stream, to land `play_at` on the right
one) read `state.search_views.top()` unconditionally — a leftover from before the library grew its
own stack (`library_views`, #21). The effect: pressing Enter on a track inside a view opened from
the *library* did nothing at all whenever no search view happened to be open (the common case), or
silently used whichever unrelated view *was* open in search instead. No existing test caught this
because the library's own tests only ever open a view, never press Enter on a track inside one.

Fixed to read `active_stack_mut(state)` — the stack belonging to whichever section actually
opened the view — the same helper every other view-aware action already uses. Two regression
tests: Enter on a track inside an album opened from Library's Favorite Albums tab, and inside a
radio opened from Library's Favorite Tracks tab (two different entry points —
`act_on_library_result`'s album branch and `open_track_radio` — both pushing onto the same
`library_views` stack, both now correctly read back by the one shared fix).

This matters for Home specifically because Home's own planned navigation (opening an album,
playlist or folder nested *inside Home's own stack*, decision 3 of the plan) would otherwise have
inherited the exact same silent failure on day one — fixing it first, as its own PR, means Home's
later parts build on a stack-reading path already proven correct, rather than re-discovering the
same bug a third time.

Full workspace green (phonia-tui +2 tests), fmt+clippy clean. No README/user-facing-feature note
(a bug fix restoring intended behavior, not new surface) beyond this entry and the ROADMAP note.

**Approved design for the rest of #139** (recorded now, implemented across the next 3 parts): a
new `Section::Home`, first in the sidebar at key `1` (everything else shifts to `2`-`5`, matching
tidal.com's own "the key is the position" convention already in use). `HomeState` holds no data of
its own — a pure `home::rows(&State)` builds rows from `state.status`/`state.queue`
(already-connected data, no new request) and `state.library` (the same `Request::Library`/
`Request::PlaylistFolder` the Library section already makes — `maybe_load_library`'s guard widens
to fire on Home too, so visiting Home first just means those two requests go out at startup
instead of on first visiting Library). Rows: one "Continue" row (resumes a paused track at its
exact position via `Request::Resume`; replays a stopped one from 0:00 via `Request::Play{Some}`,
since the position is gone after a real stop; starts the queue via `Request::Play{None}` if
nothing has played yet this session), then three 6-row blocks (favorite albums, playlist folders,
favorite tracks) each ending in a "See all (N) →" row that jumps into Library on the matching tab.
Opening an album, playlist or folder from Home pushes onto Home's own new stack (`home_views`),
the same way Library's own works — "Home › Album", Back returns to Home. Resuming/continuing is
scoped to "while this `phoniad` stays up" only: nothing in the daemon persists the queue or
position across a restart today, and adding that is explicitly a separate future issue, not part
of #139.

Parts 2-4 (next): the `Section::Home` shell with only the Continue row (part 2); the three
favorites/folders blocks plus "See all" (part 3); opening albums/playlists/folders nested inside
Home, closing #139 (part 4).

## 2026-10-08 — #139 part 2: the Home section shell, Continue row only

Added `Section::Home` as the plan called for: first in `Section::ALL`, so it is key `1` and (via
`Cursor::default()`) the section the TUI now opens on, with Queue/Search/Library/Lyrics shifting to
`2`-`5`. No `HomeState`/`home_views` yet — with a single row, part 2 has nothing to hold a cursor
or a stack over, so those are deferred to part 3 (rows) and part 4 (nested navigation) rather than
added early and left unused. `home::continuation(&State)` is a pure function straight off
`state.status`/`state.queue`, exactly as planned: `status.track` present means resume at its exact
position; absent but `queue.current` still set means a real Stop happened (which clears
`status.track` but never the queue's own `current`) — replay that entry from 0:00 since the
position is gone; a queue with items but no `current` yet starts it from the top; otherwise there
is nothing to continue. `view::home::draw` renders just that row's text, highlighted when Home has
the focus.

Five exhaustive `Section` match sites needed a `Home` arm (the compiler found them all): the
sidebar's cover lookup (reuses Queue's — Home's Continue row is about the same now-playing track),
the `Activate` dispatch (new `act_on_home`, sending whatever `continuation().request()` returns),
`AddToQueue`/`AddNext` and `OpenRadio` (both no-ops on Home, like Queue and Lyrics already are),
`load_more_results` (nothing paginated), and `move_cursor` (no-op, like Lyrics — one row, nothing
to move between yet).

Renumbering the keys broke no production logic, only ~80 tests that either pressed a now-shifted
number or relied on `State::default()` starting on Queue — each fixed by using the new key or
adding the one navigation step now needed to reach the section the test actually exercises; no
assertion about a section's own behavior changed. Full workspace green, fmt+clippy clean.

## 2026-10-08 (later): #139 part 3: the three content blocks, favorites and folders

Added the rest of decision 4's content: `home::rows(&State)` now builds, after the Continue row,
one block per non-empty list the library already holds — favorite albums, playlist folders,
favorite tracks, in that order — each capped at 6 items (`BLOCK_SIZE`) and closed with a
`Row::SeeAll { tab, total }` row. `maybe_load_library`'s guard widened from `Section::Library` to
`Section::Library | Section::Home`: Home reads the exact same `LibraryState` Library does (no
second request type, no duplicated data), so visiting Home first — which is now unavoidable, since
it is also the startup section — asks for the library immediately instead of waiting for Library
to be opened once. This is a real, deliberate behavior change (not just the part 2 renumbering):
the two library requests now go out the moment a connection with the catalog exists, whichever
section happens to be showing.

**A block that has not loaded yet and a block that loaded empty are treated identically: both
contribute nothing** (no header, no "Loading…" row, no "No favorites" row). Chosen over a more
expressive per-block status for two reasons: the library typically answers within the same tick
on a decent connection, so a visible "Loading" blip would mostly flash once; and reusing one
"empty signal" (skip the block) for both would-be-empty states avoids a second variant of `Row`
that exists only to say the same thing ("there is nothing to show you right now") a different way.
If this turns out to feel too bare on a slow connection, it is a one-line change to `rows()`, not a
redesign.

**`Row<'a>` borrows from `state.library`/`state.queue` rather than cloning** (`Row::Album(&'a
AlbumSummary)`, `Row::Entry(&'a FolderEntry)`, `Row::Track(&'a TrackSummary)`), matching
`library::Selected<'a>`'s own precedent — `home::rows(state)` is cheap and rebuilt fresh on every
draw/move/Enter (at most ~25 rows), so there is nothing here worth caching. Acting on a row
(`act_on_home`), though, needs to mutate `state` (set `last_error`, switch section) while a `Row`
borrowed from it is still alive — resolved with a small `Intent` enum (`Nothing` / `Request` /
`PlayTrack(TrackSummary)` / `JumpTo(LibraryTab)`) computed once, owned, with no live borrow of
`state` by the time anything mutates it. `home::intent(state, selected)` is the one place that
turns "which selectable row is this" into "what should happen", kept separate from `rows()` itself
so the pure "what is on screen" question and the "what does Enter do" question do not tangle.

**Scope line for part 3 vs. part 4, decided while implementing (not re-asked, since it follows
directly from the plan's own wording)**: part 4's own name is "opening albums, playlists and
folders nested inside Home" — a *track* is never "opened" anywhere in this codebase, only played
or queued, so giving a favorite-track row's Enter the exact one-track behavior Library's own
Favorite Tracks tab already has (`send_add`, `AddAt::Next`, played at once) is squarely part 3's
own "show the content and let the obvious interaction work" scope, not a part 4 concession. An
album or a folder row's Enter is a deliberate no-op until part 4 wires nested opening — showing
the row now, inert, is judged better than hiding it until it is fully interactive, since the "See
all" row already gives a working path to open any of them today via Library.

**`Row::selectable()`** (false only for `Header`/`Spacer`) and **`display_index_of(rows,
selected)`** (maps the cursor's own index, which only counts selectable rows, back to its real
position in the full display list) are what let `move_cursor`'s `Section::Home` arm and
`view::home::draw`'s scrolling (`first_visible`, the same helper every other list already uses)
work without a second, denser representation: one `Vec<Row>` serves both the cursor's count and
the screen's rendering.

12 new unit tests in `home.rs` (block construction, capping, ordering, display-index mapping,
`intent` for each row kind) plus 3 in `view/mod.rs` (blocks render with their "See all" rows and
real totals, nothing shows before the library loads, moving down highlights the first album row
specifically — reusing the existing `REVERSED`-modifier-count pattern already used for search/
library row highlighting). Full workspace green (335 phonia-tui tests, up from 320), fmt+clippy
clean.

## 2026-10-08 (latest): #139 part 4 (last): opening albums, playlists and folders nested in Home — CLOSES #139

New `home_views: Stack` field on `State`, added to `active_stack_mut`'s match alongside Search's
and Library's own (`Section::Home => Some(&mut state.home_views)`). **The headline finding of this
part: almost none of the "browsing an opened view" machinery needed Home-specific code at all.**
`act_in_view`, `act_on_track_row`, `browsed_row`, `pop_view_if_browsing`, `move_cursor`'s browsing
branch, and the artist-tab-switch check under `TabNext`/`TabPrevious` are all already written
generically against whatever `active_stack_mut(state)` returns for the *current* section — they
were never Search-or-Library-specific to begin with, just never exercised by a third section
before. Adding Home to that one function's match was enough to make opening, paging, adding from,
playing from, and closing a nested view all work for Home exactly as they already do for Library,
with no duplicated logic.

**The `Selected`-from-part-3 refactor paid for itself here.** Because `home::Selected` already
separated "which row, as owned data" from "what action does with it", extending
`act_on_home_result` (renamed from part 2/3's `act_on_home`, to match `act_on_library_result`'s
own naming) to handle `AddToQueue`/`AddNext`/`OpenRadio` — not just `Activate` — was a matter of
mirroring `act_on_library_result`'s own shape arm by arm, not a redesign: `Selected::Track` gets
the exact streamable-check-then-`send_add` logic Library's Favorite Tracks tab already has (now
for all four actions, not just Activate); `Selected::Album`'s `Activate` opens via
`open_track_list` nested onto `home_views`, otherwise `send_add`s the whole album; `Selected::
Entry` is handed whole to the existing `act_on_folder_entry`, unchanged, since it already handles
a folder/playlist/`Unknown` entry for all four actions on its own.

**Scope decided while implementing, not re-asked**: `a`/`A`/`o` were only specified for Enter in
the original plan, but extending them to Home's own unopened rows (not just once something is
opened) was judged the right call for parity with Library, which already supports both on its
equivalent rows — otherwise Home's rows would have been inconsistently *less* capable than
Library's for the exact same data, which the plan's own "nests the same way Library's own already
works" framing argues against, not for.

**A real bug found via the new integration tests, the same shape as #140's**: `find_view` (which
routes a `Tag::View` answer — `Request::Tracks` or `Request::PlaylistFolder` — back to the view
that asked for it, by serial) checked only `search_views` and `library_views`, a hardcoded
two-stack list from before Home's own stack existed. A view opened from Home therefore never
received its answer and sat on "Loading" forever — caught immediately by
`enter_on_a_track_inside_an_album_opened_from_home_plays_from_it` (modeled directly on #140's own
regression test). Fixed the same way #140 was: read whichever stack actually holds the serial,
trying all three instead of two. **Lesson repeated for the second time this issue**: a function
enumerating "the stacks that can have an open view" by name, rather than deriving the list from
`Section::ALL`, silently drops a case the moment a new stack is added — `active_stack_mut` already
matches on `state.section()` and is therefore exhaustive-checked by the compiler on every new
`Section` variant, while `find_view` has no such safety net since it searches by serial across
*all* stacks regardless of section and the compiler cannot tell it is missing one. Worth watching
for a third time if a future section ever grows its own stack again.

Snapshot's own change-detection grew `home_cursor`/`home_view` fields (mirroring `library_tab`/
`library_cursors`/`library_view`), `Msg::Disconnected` now also calls `home_views.connection_lost()`,
and `open_cover`/`draw_main` each grew the same "nothing open: show the Continue-row-equivalent
state; something open: show the stack's own cover/breadcrumb" branch Library already had. 9 new
integration tests in `app.rs` (opening an album/folder/playlist from Home, the #140-shaped
regression, adding without opening, jumping via "See all", opening a track's radio, playing a
favorite track, closing an opened view, losing the connection while one is still loading) plus the
`Intent` → `Selected` rename carried through `home.rs`'s own unit tests. Full workspace green (344
phonia-tui tests, up from 335), fmt+clippy clean.

**#139 (Home screen in the TUI) is now FULLY DONE, all 4 PRs merged.** A lightweight home reading
only data phonia already fetches locally (resume/replay/start, favorite albums, playlist folders,
favorite tracks, each opening nested in Home's own stack exactly like Library's own). A real
TIDAL-style editorial/personalized home (mixes, new releases) remains the deliberately separate,
not-yet-investigated future issue #33 already left "My Mixes" in.
