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
