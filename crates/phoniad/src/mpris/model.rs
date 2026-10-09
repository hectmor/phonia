//! The pure MPRIS policy: no zbus, no D-Bus, no async -- just turning phonia's own wire types
//! (`phonia_ipc::Status`/`Queue`/`Event`) into MPRIS's own vocabulary (`PlaybackStatus`,
//! `Metadata`, the `Can*` flags...) and back (an MPRIS call becomes a `Request`). Pure state the
//! same shape as `autoplay::Autoplay`: no locks, no async, so it is unit-tested directly; the
//! zbus adapter (a later part of #34) holds one of these and does the actual D-Bus work around
//! it.

use phonia_ipc::{Event, ItemId, Queue, Repeat, Request, SeekTarget, State, Status, Volume};

/// The object path for `mpris:trackid` when nothing is current, per the MPRIS spec.
const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";

/// The pixel size asked for an `mpris:artUrl` -- big enough for a lock-screen widget.
const ART_PX: u32 = 640;

/// MPRIS's own three-value playback status (`org.mpris.MediaPlayer2.Player.PlaybackStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

impl PlaybackStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PlaybackStatus::Playing => "Playing",
            PlaybackStatus::Paused => "Paused",
            PlaybackStatus::Stopped => "Stopped",
        }
    }

    /// The engine's own five-value `State` collapsed onto MPRIS's three, "sticky" during
    /// `Loading`/`Seeking` (#34 decision 1): neither is a real play/pause/stop transition on its
    /// own, so the *previous* MPRIS status is kept -- except coming from `Stopped`, where
    /// `Loading` is itself the evidence that something was just asked to play, so it shows
    /// `Playing` at once rather than waiting for the engine's own settling `StateChanged`.
    /// Mapping `Loading`/`Seeking` straight to `Playing` would be wrong whenever a seek happens
    /// while paused, or a pause requested mid-load lands in `Paused` once the load finishes.
    fn collapse(previous: PlaybackStatus, state: State) -> PlaybackStatus {
        match state {
            State::Playing => PlaybackStatus::Playing,
            State::Paused => PlaybackStatus::Paused,
            State::Stopped => PlaybackStatus::Stopped,
            State::Loading | State::Seeking => {
                if previous == PlaybackStatus::Stopped {
                    PlaybackStatus::Playing
                } else {
                    previous
                }
            }
        }
    }
}

/// MPRIS's own `LoopStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopStatus {
    None,
    Track,
    Playlist,
}

impl LoopStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            LoopStatus::None => "None",
            LoopStatus::Track => "Track",
            LoopStatus::Playlist => "Playlist",
        }
    }

    fn from_repeat(repeat: Repeat) -> Self {
        match repeat {
            Repeat::Off => LoopStatus::None,
            Repeat::One => LoopStatus::Track,
            Repeat::All => LoopStatus::Playlist,
        }
    }

    pub fn to_repeat(self) -> Repeat {
        match self {
            LoopStatus::None => Repeat::Off,
            LoopStatus::Track => Repeat::One,
            LoopStatus::Playlist => Repeat::All,
        }
    }
}

/// `org.mpris.MediaPlayer2.Player.Metadata`, the fields phonia can actually fill. `xesam:album`
/// is left out: the wire carries no album title today, only `album_id` internally -- a follow-up
/// if ever wanted, not a protocol change this issue needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Metadata {
    /// `mpris:trackid`: an opaque object path, never a real exported object.
    pub track_id: String,
    /// `mpris:length`, microseconds.
    pub length_us: Option<i64>,
    /// `mpris:artUrl`.
    pub art_url: Option<String>,
    /// `xesam:title`.
    pub title: Option<String>,
    /// `xesam:artist`: always 0 or 1 elements. Never split from the wire's own joined string
    /// (`"Earth, Wind & Fire"` would break), so there is nothing to split to begin with.
    pub artist: Vec<String>,
    /// `xesam:url`, a real `file://` URI, only for a local file.
    pub url: Option<String>,
}

impl Metadata {
    fn none() -> Self {
        Metadata {
            track_id: NO_TRACK.to_string(),
            ..Default::default()
        }
    }

    fn from_track(track: &TrackInfo, current: Option<ItemId>, duration_ms: Option<u64>) -> Self {
        let track_id = match track.item_id.or(current) {
            Some(id) => format!("/org/mpris/MediaPlayer2/Track/{}", id.0),
            None => NO_TRACK.to_string(),
        };
        let art_url = track.cover.as_deref().and_then(|cover| {
            phonia_ipc::image::url(phonia_ipc::image::Kind::AlbumCover, cover, ART_PX)
        });
        // phonia's own source is `file:/abs/path` (one slash); a real file URI needs three.
        let url = track
            .source
            .as_deref()
            .and_then(|source| source.strip_prefix("file:"))
            .map(|path| format!("file://{path}"));
        Metadata {
            track_id,
            length_us: duration_ms.map(|ms| ms as i64 * 1000),
            art_url,
            title: track.title.clone(),
            artist: track.artist.clone().into_iter().collect(),
            url,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackInfo {
    item_id: Option<ItemId>,
    source: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    cover: Option<String>,
}

/// Everything the MPRIS side needs, kept just once and derived from on every change -- not
/// recomputed field by field, so there is one place ([`Raw::derive`]) that can disagree with
/// itself.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Raw {
    /// The "sticky" collapsed status; see [`PlaybackStatus::collapse`]. Updated only by
    /// `StateChanged` and `TrackStarted` (a track starting is itself always evidence of
    /// `Playing`, including a gapless join that sends no `StateChanged` at all).
    status: PlaybackStatus,
    track: Option<TrackInfo>,
    duration_ms: Option<u64>,
    position_ms: u64,
    /// Play order (already shuffled if shuffle is on), same as `Queue::order`.
    order: Vec<ItemId>,
    current: Option<ItemId>,
    repeat: Repeat,
    shuffle: bool,
    volume: Option<Volume>,
}

impl Raw {
    fn from_snapshot(status: &Status, queue: &Queue) -> Self {
        Raw {
            // No real "previous" at startup: treating it as if coming from Stopped is the only
            // sensible default, and collapses Loading/Seeking to Playing, same as any other time
            // playback is caught already in motion.
            status: PlaybackStatus::collapse(PlaybackStatus::Stopped, status.state),
            track: status.track.as_ref().map(|track| TrackInfo {
                item_id: track.item_id,
                source: track.source.clone(),
                title: track.title.clone(),
                artist: track.artist.clone(),
                cover: track.cover.clone(),
            }),
            duration_ms: status.duration_ms,
            position_ms: status.position_ms,
            order: queue.order.clone(),
            current: queue.current,
            repeat: queue.repeat,
            shuffle: queue.shuffle,
            volume: status.volume,
        }
    }

    fn apply(&mut self, event: &Event) {
        match event {
            Event::StateChanged { state } => {
                self.status = PlaybackStatus::collapse(self.status, *state);
            }
            Event::TrackStarted {
                item_id,
                source,
                title,
                artist,
                duration_ms,
                cover,
                ..
            } => {
                self.track = Some(TrackInfo {
                    item_id: *item_id,
                    source: source.clone(),
                    title: title.clone(),
                    artist: artist.clone(),
                    cover: cover.clone(),
                });
                self.duration_ms = *duration_ms;
                self.position_ms = 0;
                self.status = PlaybackStatus::Playing;
            }
            Event::Position {
                position_ms,
                duration_ms,
            } => {
                self.position_ms = *position_ms;
                self.duration_ms = *duration_ms;
            }
            Event::Seeked { position_ms } => {
                self.position_ms = *position_ms;
            }
            Event::QueueChanged { queue } => {
                self.order = queue.order.clone();
                self.current = queue.current;
                self.repeat = queue.repeat;
                self.shuffle = queue.shuffle;
            }
            Event::VolumeChanged { percent, muted } => {
                self.volume = Some(Volume {
                    percent: *percent,
                    muted: *muted,
                });
            }
            // Nothing MPRIS shows changes on these: a track ending is followed by either another
            // TrackStarted, a QueueChanged, or a StateChanged(Stopped), each already handled
            // above; the rest are output/catalog/quality bookkeeping MPRIS has no property for.
            _ => {}
        }
    }

    /// `CanGoNext`: the order isn't empty, and either repeat wraps (`One`/`All` both wrap Next),
    /// or the current entry isn't the last of the order, or there is no current entry yet to be
    /// "last" (#34 decision, adopted as recommended: deliberately ignores autoplay, which would
    /// make this true a moment later anyway via the `QueueChanged` that follows a refill, rather
    /// than ever claiming a Next that would actually do nothing).
    fn can_go_next(&self) -> bool {
        !self.order.is_empty()
            && (self.repeat != Repeat::Off
                || self.current.is_none()
                || self.order.last() != self.current.as_ref())
    }

    /// `CanGoPrevious`: Previous always does *something* once a track is current (restarts after
    /// 3s, otherwise walks real play history) -- the queue can't predict which ahead of time.
    fn can_go_previous(&self) -> bool {
        self.current.is_some()
    }

    fn derive(&self) -> View {
        let metadata = match &self.track {
            Some(track) => Metadata::from_track(track, self.current, self.duration_ms),
            None => Metadata::none(),
        };
        let (volume, volume_controllable) = match self.volume {
            None => (1.0, false),
            Some(Volume { muted: true, .. }) => (0.0, true),
            Some(Volume {
                percent,
                muted: false,
            }) => (f64::from(percent) / 100.0, true),
        };
        View {
            status: self.status,
            metadata,
            volume,
            volume_controllable,
            loop_status: LoopStatus::from_repeat(self.repeat),
            shuffle: self.shuffle,
            can_go_next: self.can_go_next(),
            can_go_previous: self.can_go_previous(),
            can_play: !self.order.is_empty(),
            can_pause: self.track.is_some() && self.status != PlaybackStatus::Stopped,
            can_seek: self.track.is_some() && self.duration_ms.is_some(),
        }
    }
}

/// Everything derived from [`Raw`], compared before and after an event to decide what changed.
#[derive(Debug, Clone, PartialEq)]
struct View {
    status: PlaybackStatus,
    metadata: Metadata,
    volume: f64,
    volume_controllable: bool,
    loop_status: LoopStatus,
    shuffle: bool,
    can_go_next: bool,
    can_go_previous: bool,
    can_play: bool,
    can_pause: bool,
    can_seek: bool,
}

/// Which properties changed after one [`Model::apply`] call, so the zbus adapter can emit exactly
/// one `PropertiesChanged` (and a `Seeked` signal, if `seeked_us` is set) rather than guessing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Changed {
    pub status: bool,
    pub metadata: bool,
    pub volume: bool,
    pub loop_status: bool,
    pub shuffle: bool,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_seek: bool,
    /// `Some(position_us)` exactly when a `Seeked` signal should fire.
    pub seeked_us: Option<i64>,
}

impl Changed {
    pub fn any_property(&self) -> bool {
        self.status
            || self.metadata
            || self.volume
            || self.loop_status
            || self.shuffle
            || self.can_go_next
            || self.can_go_previous
            || self.can_play
            || self.can_pause
            || self.can_seek
    }
}

/// The whole MPRIS-facing state, built from a snapshot and kept current by [`Model::apply`].
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    raw: Raw,
    view: View,
}

impl Model {
    pub fn from_snapshot(status: &Status, queue: &Queue) -> Self {
        let raw = Raw::from_snapshot(status, queue);
        let view = raw.derive();
        Model { raw, view }
    }

    /// Applies one event, returning what changed. Call sites that only care about *whether*
    /// anything did can check [`Changed::any_property`] or `seeked_us`.
    pub fn apply(&mut self, event: &Event) -> Changed {
        let before = self.view.clone();
        self.raw.apply(event);
        self.view = self.raw.derive();
        let seeked_us = match event {
            Event::Seeked { position_ms } => Some(*position_ms as i64 * 1_000),
            _ => None,
        };
        Changed {
            status: before.status != self.view.status,
            metadata: before.metadata != self.view.metadata,
            volume: before.volume != self.view.volume
                || before.volume_controllable != self.view.volume_controllable,
            loop_status: before.loop_status != self.view.loop_status,
            shuffle: before.shuffle != self.view.shuffle,
            can_go_next: before.can_go_next != self.view.can_go_next,
            can_go_previous: before.can_go_previous != self.view.can_go_previous,
            can_play: before.can_play != self.view.can_play,
            can_pause: before.can_pause != self.view.can_pause,
            can_seek: before.can_seek != self.view.can_seek,
            seeked_us,
        }
    }

    pub fn status(&self) -> PlaybackStatus {
        self.view.status
    }

    pub fn metadata(&self) -> &Metadata {
        &self.view.metadata
    }

    pub fn volume(&self) -> f64 {
        self.view.volume
    }

    pub fn volume_controllable(&self) -> bool {
        self.view.volume_controllable
    }

    pub fn loop_status(&self) -> LoopStatus {
        self.view.loop_status
    }

    pub fn shuffle(&self) -> bool {
        self.view.shuffle
    }

    pub fn can_go_next(&self) -> bool {
        self.view.can_go_next
    }

    pub fn can_go_previous(&self) -> bool {
        self.view.can_go_previous
    }

    pub fn can_play(&self) -> bool {
        self.view.can_play
    }

    pub fn can_pause(&self) -> bool {
        self.view.can_pause
    }

    pub fn can_seek(&self) -> bool {
        self.view.can_seek
    }

    pub fn position_us(&self) -> i64 {
        self.raw.position_ms as i64 * 1_000
    }

    // ---- MPRIS calls -> requests -----------------------------------------------------------

    /// `Play()`: starts the queue from Stopped, resumes from Paused, does nothing while Playing
    /// (idempotent, per spec).
    pub fn play_request(&self) -> Option<Request> {
        match self.view.status {
            PlaybackStatus::Stopped => Some(Request::Play { item: None }),
            PlaybackStatus::Paused => Some(Request::Resume),
            PlaybackStatus::Playing => None,
        }
    }

    /// `Pause()`: only meaningful while actually playing.
    pub fn pause_request(&self) -> Option<Request> {
        (self.view.status == PlaybackStatus::Playing).then_some(Request::Pause)
    }

    /// `PlayPause()`: starts the queue from Stopped (there is nothing to "toggle" yet), otherwise
    /// toggles, same as the TUI's own Space key.
    pub fn play_pause_request(&self) -> Request {
        match self.view.status {
            PlaybackStatus::Stopped => Request::Play { item: None },
            PlaybackStatus::Playing | PlaybackStatus::Paused => Request::TogglePause,
        }
    }

    pub fn stop_request(&self) -> Request {
        Request::Stop
    }

    pub fn next_request(&self) -> Request {
        Request::Next
    }

    pub fn previous_request(&self) -> Request {
        Request::Previous
    }

    /// `Seek(offset)`: a relative jump, forward or backward depending on the sign.
    pub fn seek_request(offset_us: i64) -> Request {
        let ms = offset_us.unsigned_abs() / 1_000;
        let target = if offset_us >= 0 {
            SeekTarget::Forward { ms }
        } else {
            SeekTarget::Backward { ms }
        };
        Request::Seek { target }
    }

    /// `SetPosition(track_id, position)`: per spec, ignored outright if `track_id` isn't the
    /// current track, or `position` is negative or past the end -- `None` means exactly that,
    /// not an error.
    pub fn set_position_request(&self, track_id: &str, position_us: i64) -> Option<Request> {
        if track_id != self.view.metadata.track_id || position_us < 0 {
            return None;
        }
        if let Some(length) = self.view.metadata.length_us
            && position_us > length
        {
            return None;
        }
        Some(Request::Seek {
            target: SeekTarget::Absolute {
                ms: (position_us / 1_000) as u64,
            },
        })
    }

    /// Setting `Volume`: a value above 0 sets the percent *and* unmutes, so a slider never
    /// silently snaps back to 0 while muted stays set underneath (#34 decision, adopted as
    /// recommended); exactly 0.0 sets percent 0 without touching mute.
    pub fn set_volume_requests(value: f64) -> Vec<Request> {
        let percent = (value.clamp(0.0, 1.0) * 100.0).round() as u8;
        if value <= 0.0 {
            vec![Request::SetVolume { percent: 0 }]
        } else {
            vec![
                Request::SetVolume { percent },
                Request::SetMute { mute: false },
            ]
        }
    }

    pub fn set_loop_status_request(value: LoopStatus) -> Request {
        Request::SetRepeat {
            repeat: value.to_repeat(),
        }
    }

    pub fn set_shuffle_request(value: bool) -> Request {
        Request::SetShuffle { shuffle: value }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phonia_ipc::{EndReason, Spec};

    fn status(state: State) -> Status {
        Status {
            state,
            track: None,
            spec: None,
            position_ms: 0,
            duration_ms: None,
            output: phonia_ipc::Output::Closed,
            route: None,
            volume: None,
            quality_range: None,
            sink_report: None,
        }
    }

    fn queue() -> Queue {
        Queue {
            version: 1,
            items: Vec::new(),
            order: Vec::new(),
            current: None,
            shuffle: false,
            repeat: Repeat::Off,
            autoplay: false,
        }
    }

    fn with_track(mut status: Status, id: u64, cover: Option<&str>) -> Status {
        status.track = Some(phonia_ipc::Track {
            item_id: Some(ItemId(id)),
            source: Some(format!("tidal:{id}")),
            title: Some("Song".into()),
            artist: Some("Artist".into()),
            duration_ms: Some(200_000),
            quality: None,
            cover: cover.map(str::to_string),
            replay_gain: None,
        });
        status.duration_ms = Some(200_000);
        status
    }

    fn track_started(id: u64) -> Event {
        Event::TrackStarted {
            item_id: Some(ItemId(id)),
            source: Some(format!("tidal:{id}")),
            title: Some("Song".into()),
            artist: Some("Artist".into()),
            duration_ms: Some(200_000),
            spec: Spec {
                sample_rate: 44_100,
                channels: 2,
                bits_per_sample: 16,
            },
            gapless: false,
            quality: None,
            cover: None,
            replay_gain: None,
        }
    }

    #[test]
    fn playback_status_is_sticky_through_loading_and_seeking() {
        let mut model = Model::from_snapshot(&status(State::Playing), &queue());
        assert_eq!(model.status(), PlaybackStatus::Playing);
        model.apply(&Event::StateChanged {
            state: State::Seeking,
        });
        assert_eq!(model.status(), PlaybackStatus::Playing, "still playing");

        let mut model = Model::from_snapshot(&status(State::Paused), &queue());
        model.apply(&Event::StateChanged {
            state: State::Seeking,
        });
        assert_eq!(
            model.status(),
            PlaybackStatus::Paused,
            "a seek while paused does not start showing Playing"
        );
        model.apply(&Event::StateChanged {
            state: State::Loading,
        });
        assert_eq!(
            model.status(),
            PlaybackStatus::Paused,
            "a pause requested mid-load lands back in Paused, not Playing"
        );
    }

    #[test]
    fn stopped_to_loading_shows_playing_at_once() {
        let mut model = Model::from_snapshot(&status(State::Stopped), &queue());
        let changed = model.apply(&Event::StateChanged {
            state: State::Loading,
        });
        assert_eq!(model.status(), PlaybackStatus::Playing);
        assert!(changed.status);
    }

    #[test]
    fn a_track_starting_always_shows_playing_even_with_no_state_changed_gapless() {
        let mut model = Model::from_snapshot(&status(State::Playing), &queue());
        model.apply(&Event::StateChanged {
            state: State::Paused,
        });
        assert_eq!(model.status(), PlaybackStatus::Paused);
        model.apply(&track_started(2));
        assert_eq!(model.status(), PlaybackStatus::Playing);
    }

    #[test]
    fn metadata_is_none_with_nothing_playing() {
        let model = Model::from_snapshot(&status(State::Stopped), &queue());
        assert_eq!(model.metadata().track_id, NO_TRACK);
        assert_eq!(model.metadata().title, None);
    }

    #[test]
    fn metadata_builds_trackid_length_and_a_single_element_artist() {
        let mut model = Model::from_snapshot(&status(State::Stopped), &queue());
        model.apply(&track_started(42));
        let metadata = model.metadata();
        assert_eq!(metadata.track_id, "/org/mpris/MediaPlayer2/Track/42");
        assert_eq!(metadata.length_us, Some(200_000_000));
        assert_eq!(metadata.title.as_deref(), Some("Song"));
        assert_eq!(metadata.artist, vec!["Artist".to_string()]);
    }

    #[test]
    fn a_cover_id_becomes_a_real_art_url_and_none_stays_none() {
        let with_cover = with_track(
            status(State::Playing),
            1,
            Some("3c6247c7-d0d7-4978-91b1-0bddc13f45b5"),
        );
        let model = Model::from_snapshot(&with_cover, &queue());
        assert!(
            model
                .metadata()
                .art_url
                .as_deref()
                .unwrap()
                .starts_with("https://")
        );

        let no_cover = with_track(status(State::Playing), 1, None);
        let model = Model::from_snapshot(&no_cover, &queue());
        assert_eq!(model.metadata().art_url, None);
    }

    #[test]
    fn a_local_file_source_becomes_a_real_file_uri() {
        let mut model = Model::from_snapshot(&status(State::Stopped), &queue());
        model.apply(&Event::TrackStarted {
            item_id: Some(ItemId(1)),
            source: Some("file:/home/x/song.flac".into()),
            title: Some("Song".into()),
            artist: None,
            duration_ms: Some(1_000),
            spec: Spec {
                sample_rate: 44_100,
                channels: 2,
                bits_per_sample: 16,
            },
            gapless: false,
            quality: None,
            cover: None,
            replay_gain: None,
        });
        assert_eq!(
            model.metadata().url.as_deref(),
            Some("file:///home/x/song.flac")
        );
        assert_eq!(model.metadata().artist, Vec::<String>::new());
    }

    fn queue_with(order: Vec<u64>, current: Option<u64>, repeat: Repeat) -> Queue {
        Queue {
            order: order.into_iter().map(ItemId).collect(),
            current: current.map(ItemId),
            repeat,
            ..queue()
        }
    }

    #[test]
    fn can_go_next_is_false_only_on_the_last_entry_with_repeat_off() {
        let q = queue_with(vec![1, 2, 3], Some(3), Repeat::Off);
        let model = Model::from_snapshot(&status(State::Playing), &q);
        assert!(!model.can_go_next(), "already on the last entry");

        let q = queue_with(vec![1, 2, 3], Some(2), Repeat::Off);
        let model = Model::from_snapshot(&status(State::Playing), &q);
        assert!(model.can_go_next(), "not the last entry yet");
    }

    #[test]
    fn can_go_next_wraps_with_repeat_one_or_all_even_on_the_last_entry() {
        for repeat in [Repeat::One, Repeat::All] {
            let q = queue_with(vec![1, 2, 3], Some(3), repeat);
            let model = Model::from_snapshot(&status(State::Playing), &q);
            assert!(model.can_go_next(), "{repeat:?} wraps");
        }
    }

    #[test]
    fn can_go_next_is_true_with_nothing_current_yet_and_false_on_an_empty_queue() {
        let q = queue_with(vec![1, 2], None, Repeat::Off);
        assert!(Model::from_snapshot(&status(State::Stopped), &q).can_go_next());

        let q = queue();
        assert!(!Model::from_snapshot(&status(State::Stopped), &q).can_go_next());
    }

    #[test]
    fn can_go_previous_only_once_something_is_current() {
        let q = queue_with(vec![1, 2], None, Repeat::Off);
        assert!(!Model::from_snapshot(&status(State::Stopped), &q).can_go_previous());

        let q = queue_with(vec![1, 2], Some(1), Repeat::Off);
        assert!(Model::from_snapshot(&status(State::Playing), &q).can_go_previous());
    }

    #[test]
    fn can_play_pause_seek_follow_the_queue_and_the_current_track() {
        let model = Model::from_snapshot(&status(State::Stopped), &queue());
        assert!(!model.can_play(), "nothing queued");
        assert!(!model.can_pause());
        assert!(!model.can_seek());

        let q = queue_with(vec![1], None, Repeat::Off);
        let model = Model::from_snapshot(&status(State::Stopped), &q);
        assert!(model.can_play(), "something queued, even if stopped");

        let playing_with_track = with_track(status(State::Playing), 1, None);
        let model = Model::from_snapshot(&playing_with_track, &queue());
        assert!(model.can_pause());
        assert!(model.can_seek());

        let stopped_with_track = with_track(status(State::Stopped), 1, None);
        let model = Model::from_snapshot(&stopped_with_track, &queue());
        assert!(!model.can_pause(), "Stopped is never pausable");
    }

    #[test]
    fn volume_reads_zero_while_muted_and_one_with_no_hardware_control() {
        let mut playing = status(State::Playing);
        playing.volume = Some(Volume {
            percent: 60,
            muted: true,
        });
        let model = Model::from_snapshot(&playing, &queue());
        assert_eq!(model.volume(), 0.0);
        assert!(model.volume_controllable());

        let mut playing = status(State::Playing);
        playing.volume = Some(Volume {
            percent: 60,
            muted: false,
        });
        let model = Model::from_snapshot(&playing, &queue());
        assert_eq!(model.volume(), 0.6);

        let model = Model::from_snapshot(&status(State::Playing), &queue());
        assert_eq!(model.volume(), 1.0);
        assert!(
            !model.volume_controllable(),
            "no hardware mixer to write to"
        );
    }

    #[test]
    fn apply_reports_exactly_what_changed_and_nothing_when_nothing_did() {
        let mut model = Model::from_snapshot(&status(State::Playing), &queue());
        let changed = model.apply(&Event::Position {
            position_ms: 1_000,
            duration_ms: None,
        });
        assert!(
            !changed.any_property(),
            "position alone is not a property MPRIS gets a signal for"
        );

        let changed = model.apply(&Event::StateChanged {
            state: State::Paused,
        });
        assert!(changed.status);
        assert!(!changed.metadata);
    }

    #[test]
    fn seeked_reports_the_microsecond_position_only_on_a_real_seek() {
        let mut model = Model::from_snapshot(&status(State::Playing), &queue());
        let changed = model.apply(&Event::Position {
            position_ms: 5_000,
            duration_ms: None,
        });
        assert_eq!(changed.seeked_us, None);

        let changed = model.apply(&Event::Seeked { position_ms: 7_000 });
        assert_eq!(changed.seeked_us, Some(7_000_000));
        assert_eq!(model.position_us(), 7_000_000);
    }

    #[test]
    fn play_pause_and_playpause_map_to_the_right_request_per_status() {
        let stopped = Model::from_snapshot(&status(State::Stopped), &queue());
        assert_eq!(stopped.play_request(), Some(Request::Play { item: None }));
        assert_eq!(stopped.pause_request(), None);
        assert_eq!(stopped.play_pause_request(), Request::Play { item: None });

        let paused = Model::from_snapshot(&status(State::Paused), &queue());
        assert_eq!(paused.play_request(), Some(Request::Resume));
        assert_eq!(paused.pause_request(), None);
        assert_eq!(paused.play_pause_request(), Request::TogglePause);

        let playing = Model::from_snapshot(&status(State::Playing), &queue());
        assert_eq!(
            playing.play_request(),
            None,
            "idempotent while already playing"
        );
        assert_eq!(playing.pause_request(), Some(Request::Pause));
        assert_eq!(playing.play_pause_request(), Request::TogglePause);
    }

    #[test]
    fn seek_request_picks_forward_or_backward_from_the_sign() {
        assert_eq!(
            Model::seek_request(5_000_000),
            Request::Seek {
                target: SeekTarget::Forward { ms: 5_000 }
            }
        );
        assert_eq!(
            Model::seek_request(-2_500_000),
            Request::Seek {
                target: SeekTarget::Backward { ms: 2_500 }
            }
        );
    }

    #[test]
    fn set_position_is_refused_for_the_wrong_track_or_out_of_range() {
        let mut model = Model::from_snapshot(&status(State::Stopped), &queue());
        model.apply(&track_started(42));
        let id = model.metadata().track_id.clone();

        assert_eq!(
            model.set_position_request(&id, 50_000_000),
            Some(Request::Seek {
                target: SeekTarget::Absolute { ms: 50_000 }
            })
        );
        assert_eq!(model.set_position_request("/some/other/track", 1_000), None);
        assert_eq!(model.set_position_request(&id, -1), None);
        assert_eq!(
            model.set_position_request(&id, 10_000_000_000),
            None,
            "past the end of a 200s track"
        );
    }

    #[test]
    fn setting_volume_above_zero_also_unmutes() {
        assert_eq!(
            Model::set_volume_requests(0.6),
            vec![
                Request::SetVolume { percent: 60 },
                Request::SetMute { mute: false }
            ]
        );
        assert_eq!(
            Model::set_volume_requests(0.0),
            vec![Request::SetVolume { percent: 0 }],
            "exactly 0 does not also send an explicit mute"
        );
    }

    #[test]
    fn set_loop_status_and_shuffle_map_directly() {
        assert_eq!(
            Model::set_loop_status_request(LoopStatus::Track),
            Request::SetRepeat {
                repeat: Repeat::One
            }
        );
        assert_eq!(
            Model::set_loop_status_request(LoopStatus::Playlist),
            Request::SetRepeat {
                repeat: Repeat::All
            }
        );
        assert_eq!(
            Model::set_shuffle_request(true),
            Request::SetShuffle { shuffle: true }
        );
    }

    #[test]
    fn track_ended_and_other_untracked_events_leave_everything_as_is() {
        let mut model = Model::from_snapshot(&status(State::Playing), &queue());
        model.apply(&track_started(1));
        let before = model.clone();
        let changed = model.apply(&Event::TrackEnded {
            item_id: Some(ItemId(1)),
            reason: EndReason::Completed,
        });
        assert_eq!(model, before);
        assert!(!changed.any_property());
    }
}
