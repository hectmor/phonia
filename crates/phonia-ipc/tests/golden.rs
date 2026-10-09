//! The wire format, pinned. Every message is checked against an exact JSON string in both
//! directions: changing one of these strings is changing the protocol, and should be a
//! deliberate decision (and a version bump if it isn't purely additive).

use phonia_ipc::*;

fn round_trip<T>(value: &T, json: &str)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(
        serde_json::to_string(value).unwrap(),
        json,
        "serializing {value:?}"
    );
    let parsed: T =
        serde_json::from_str(json).unwrap_or_else(|error| panic!("parsing {json}: {error}"));
    assert_eq!(&parsed, value, "parsing {json}");
}

fn request(id: u64, request: Request, json: &str) {
    round_trip(
        &ClientMessage {
            id: RequestId(id),
            request,
        },
        json,
    );
}

fn spec() -> Spec {
    Spec {
        sample_rate: 96_000,
        channels: 2,
        bits_per_sample: 24,
    }
}

fn status() -> Status {
    Status {
        state: State::Playing,
        track: Some(Track {
            item_id: Some(ItemId(7)),
            source: Some("file:/music/a.flac".into()),
            title: Some("a.flac".into()),
            artist: None,
            duration_ms: Some(215_000),
            quality: None,
            cover: None,
            replay_gain: None,
        }),
        spec: Some(spec()),
        position_ms: 1_234,
        duration_ms: Some(215_000),
        output: Output::Open,
        route: None,
        volume: None,
        quality_range: None,
        sink_report: None,
    }
}

const STATUS_JSON: &str = r#"{"state":"playing","track":{"item_id":7,"source":"file:/music/a.flac","title":"a.flac","artist":null,"duration_ms":215000,"quality":null,"cover":null,"replay_gain":null},"spec":{"sample_rate":96000,"channels":2,"bits_per_sample":24},"position_ms":1234,"duration_ms":215000,"output":{"state":"open"},"route":null,"volume":null,"quality_range":null,"sink_report":null}"#;

fn queue() -> Queue {
    Queue {
        version: 5,
        items: vec![
            QueueItem {
                id: ItemId(7),
                source: "file:/music/a.flac".into(),
                title: Some("a.flac".into()),
                artist: None,
                duration_ms: Some(215_000),
                cover: None,
            },
            QueueItem {
                id: ItemId(8),
                source: "tidal:233059491".into(),
                title: None,
                artist: Some("Dire Straits".into()),
                duration_ms: None,
                cover: Some("abc123-def4".into()),
            },
        ],
        order: vec![ItemId(8), ItemId(7)],
        current: Some(ItemId(7)),
        shuffle: true,
        repeat: Repeat::All,
        autoplay: true,
    }
}

const QUEUE_JSON: &str = r#"{"version":5,"items":[{"id":7,"source":"file:/music/a.flac","title":"a.flac","artist":null,"duration_ms":215000,"cover":null},{"id":8,"source":"tidal:233059491","title":null,"artist":"Dire Straits","duration_ms":null,"cover":"abc123-def4"}],"order":[8,7],"current":7,"shuffle":true,"repeat":"all","autoplay":true}"#;

// ---- requests ------------------------------------------------------------------------------

#[test]
fn hello_request() {
    request(
        1,
        Request::Hello {
            protocol: Version { major: 1, minor: 0 },
            client: ClientInfo {
                name: "phonia-ctl".into(),
                version: "0.1.0".into(),
            },
        },
        r#"{"id":1,"request":{"type":"hello","protocol":{"major":1,"minor":0},"client":{"name":"phonia-ctl","version":"0.1.0"}}}"#,
    );
}

#[test]
fn simple_requests() {
    for (id, req, kind) in [
        (2, Request::Status, "status"),
        (3, Request::Queue, "queue"),
        (4, Request::Subscribe, "subscribe"),
        (5, Request::Unsubscribe, "unsubscribe"),
        (6, Request::Stop, "stop"),
        (7, Request::Pause, "pause"),
        (8, Request::Resume, "resume"),
        (9, Request::TogglePause, "toggle_pause"),
        (10, Request::Next, "next"),
        (11, Request::Previous, "previous"),
        (12, Request::QueueClear, "queue_clear"),
        (13, Request::Shutdown, "shutdown"),
    ] {
        request(
            id,
            req,
            &format!(r#"{{"id":{id},"request":{{"type":"{kind}"}}}}"#),
        );
    }
}

#[test]
fn play_requests() {
    request(
        1,
        Request::Play { item: None },
        r#"{"id":1,"request":{"type":"play","item":null}}"#,
    );
    request(
        2,
        Request::Play {
            item: Some(ItemId(7)),
        },
        r#"{"id":2,"request":{"type":"play","item":7}}"#,
    );
}

#[test]
fn seek_requests() {
    request(
        1,
        Request::Seek {
            target: SeekTarget::Absolute { ms: 90_000 },
        },
        r#"{"id":1,"request":{"type":"seek","target":{"type":"absolute","ms":90000}}}"#,
    );
    request(
        2,
        Request::Seek {
            target: SeekTarget::Forward { ms: 10_000 },
        },
        r#"{"id":2,"request":{"type":"seek","target":{"type":"forward","ms":10000}}}"#,
    );
    request(
        3,
        Request::Seek {
            target: SeekTarget::Backward { ms: 5_000 },
        },
        r#"{"id":3,"request":{"type":"seek","target":{"type":"backward","ms":5000}}}"#,
    );
}

#[test]
fn queue_editing_requests() {
    let tracks = vec![
        NewTrack {
            source: "file:/music/a.flac".into(),
        },
        NewTrack {
            source: "tidal:1".into(),
        },
    ];
    let tracks_json = r#"[{"source":"file:/music/a.flac"},{"source":"tidal:1"}]"#;
    request(
        1,
        Request::QueueAdd {
            tracks: tracks.clone(),
            at: AddAt::End,
        },
        &format!(
            r#"{{"id":1,"request":{{"type":"queue_add","tracks":{tracks_json},"at":{{"type":"end"}}}}}}"#
        ),
    );
    request(
        2,
        Request::QueueAdd {
            tracks: tracks.clone(),
            at: AddAt::Next,
        },
        &format!(
            r#"{{"id":2,"request":{{"type":"queue_add","tracks":{tracks_json},"at":{{"type":"next"}}}}}}"#
        ),
    );
    request(
        3,
        Request::QueueAdd {
            tracks,
            at: AddAt::Index { index: 4 },
        },
        &format!(
            r#"{{"id":3,"request":{{"type":"queue_add","tracks":{tracks_json},"at":{{"type":"index","index":4}}}}}}"#
        ),
    );
    request(
        4,
        Request::QueueRemove {
            ids: vec![ItemId(3), ItemId(9)],
        },
        r#"{"id":4,"request":{"type":"queue_remove","ids":[3,9]}}"#,
    );
    request(
        5,
        Request::QueueMove {
            id: ItemId(3),
            to: 0,
        },
        r#"{"id":5,"request":{"type":"queue_move","id":3,"to":0}}"#,
    );
    request(
        6,
        Request::SetShuffle { shuffle: true },
        r#"{"id":6,"request":{"type":"set_shuffle","shuffle":true}}"#,
    );
    request(
        7,
        Request::SetRepeat {
            repeat: Repeat::One,
        },
        r#"{"id":7,"request":{"type":"set_repeat","repeat":"one"}}"#,
    );
    request(
        8,
        Request::SetAutoplay { autoplay: true },
        r#"{"id":8,"request":{"type":"set_autoplay","autoplay":true}}"#,
    );
}

// ---- responses -----------------------------------------------------------------------------

fn message(m: ServerMessage, json: &str) {
    round_trip(&m, json);
}

#[test]
fn the_release_request() {
    request(
        1,
        Request::Release,
        r#"{"id":1,"request":{"type":"release"}}"#,
    );
}

fn response(id: u64, reply: Reply, json: &str) {
    message(
        ServerMessage::Response {
            id: RequestId(id),
            reply,
        },
        json,
    );
}

#[test]
fn the_server_banner() {
    message(
        ServerMessage::Hello(ServerHello {
            protocol: PROTOCOL,
            server: ServerInfo {
                name: "phoniad".into(),
                version: "0.1.0".into(),
                pid: 1234,
            },
            capabilities: vec![
                CAP_OUTPUT_RELEASE.into(),
                CAP_OUTPUT_SELECT.into(),
                CAP_VOLUME.into(),
                CAP_GAPLESS.into(),
                CAP_QUALITY.into(),
                CAP_RECENTLY_PLAYED.into(),
                CAP_CATALOG.into(),
                CAP_LYRICS.into(),
                CAP_AUTOPLAY.into(),
            ],
        }),
        r#"{"type":"hello","protocol":{"major":1,"minor":15},"server":{"name":"phoniad","version":"0.1.0","pid":1234},"capabilities":["output_release","output_select","volume","gapless","quality","recently_played","catalog","lyrics","autoplay"]}"#,
    );
}

#[test]
fn successful_responses() {
    response(
        1,
        Reply::Ok(Payload::Ack),
        r#"{"type":"response","id":1,"ok":{"type":"ack"}}"#,
    );
    response(
        2,
        Reply::Ok(Payload::Status(status())),
        &format!(
            r#"{{"type":"response","id":2,"ok":{{"type":"status","state":"playing","track":{},"spec":{},"position_ms":1234,"duration_ms":215000,"output":{{"state":"open"}},"route":null,"volume":null,"quality_range":null,"sink_report":null}}}}"#,
            r#"{"item_id":7,"source":"file:/music/a.flac","title":"a.flac","artist":null,"duration_ms":215000,"quality":null,"cover":null,"replay_gain":null}"#,
            r#"{"sample_rate":96000,"channels":2,"bits_per_sample":24}"#
        ),
    );
    response(
        3,
        Reply::Ok(Payload::Removed { count: 2 }),
        r#"{"type":"response","id":3,"ok":{"type":"removed","count":2}}"#,
    );
    response(
        4,
        Reply::Ok(Payload::Added {
            ids: vec![ItemId(9), ItemId(10)],
            rejected: vec![Rejected {
                source: "file:/nope.flac".into(),
                reason: "opening: No such file".into(),
            }],
            unresolved: vec![Unresolved {
                id: ItemId(10),
                reason: "TIDAL unreachable".into(),
            }],
        }),
        r#"{"type":"response","id":4,"ok":{"type":"added","ids":[9,10],"rejected":[{"source":"file:/nope.flac","reason":"opening: No such file"}],"unresolved":[{"id":10,"reason":"TIDAL unreachable"}]}}"#,
    );
    let json = format!(
        r#"{{"type":"response","id":5,"ok":{{"type":"queue",{}}}}}"#,
        &QUEUE_JSON[1..QUEUE_JSON.len() - 1]
    );
    response(5, Reply::Ok(Payload::Queue(queue())), &json);
    let json = format!(
        r#"{{"type":"response","id":6,"ok":{{"type":"snapshot","seq":41,"status":{STATUS_JSON},"queue":{QUEUE_JSON}}}}}"#
    );
    response(
        6,
        Reply::Ok(Payload::Snapshot {
            seq: 41,
            status: status(),
            queue: queue(),
        }),
        &json,
    );
}

#[test]
fn failed_responses() {
    for (code, name) in [
        (ErrorCode::UnsupportedVersion, "unsupported_version"),
        (ErrorCode::HandshakeRequired, "handshake_required"),
        (ErrorCode::UnknownRequest, "unknown_request"),
        (ErrorCode::BadRequest, "bad_request"),
        (ErrorCode::BadSource, "bad_source"),
        (ErrorCode::NotFound, "not_found"),
        (ErrorCode::Unsupported, "unsupported"),
        (ErrorCode::EngineGone, "engine_gone"),
        (ErrorCode::Internal, "internal"),
    ] {
        response(
            9,
            Reply::Err(ProtocolError {
                code,
                message: "why".into(),
            }),
            &format!(r#"{{"type":"response","id":9,"err":{{"code":"{name}","message":"why"}}}}"#),
        );
    }
}

// ---- events --------------------------------------------------------------------------------

fn event(seq: u64, event: Event, json: &str) {
    message(ServerMessage::Event { seq, event }, json);
}

#[test]
fn events() {
    event(
        1,
        Event::StateChanged {
            state: State::Paused,
        },
        r#"{"type":"event","seq":1,"event":{"type":"state_changed","state":"paused"}}"#,
    );
    event(
        2,
        Event::TrackStarted {
            item_id: Some(ItemId(7)),
            source: Some("tidal:1".into()),
            title: Some("t".into()),
            artist: Some("Dire Straits".into()),
            duration_ms: Some(1000),
            spec: spec(),
            gapless: false,
            quality: None,
            cover: Some("cover-uuid".into()),
            replay_gain: Some(ReplayGain {
                kind: GainKind::Album,
                millibels: -290,
            }),
        },
        r#"{"type":"event","seq":2,"event":{"type":"track_started","item_id":7,"source":"tidal:1","title":"t","artist":"Dire Straits","duration_ms":1000,"spec":{"sample_rate":96000,"channels":2,"bits_per_sample":24},"gapless":false,"quality":null,"cover":"cover-uuid","replay_gain":{"kind":"album","millibels":-290}}}"#,
    );
    event(
        21,
        Event::TrackStarted {
            item_id: Some(ItemId(8)),
            source: None,
            title: None,
            artist: None,
            duration_ms: None,
            spec: spec(),
            gapless: true,
            quality: Some(StreamQuality {
                requested: Quality::Hires,
                delivered: Quality::Lossless,
            }),
            cover: None,
            replay_gain: None,
        },
        r#"{"type":"event","seq":21,"event":{"type":"track_started","item_id":8,"source":null,"title":null,"artist":null,"duration_ms":null,"spec":{"sample_rate":96000,"channels":2,"bits_per_sample":24},"gapless":true,"quality":{"requested":"hires","delivered":"lossless"},"cover":null,"replay_gain":null}}"#,
    );
    event(
        3,
        Event::TrackEnded {
            item_id: Some(ItemId(7)),
            reason: EndReason::Completed,
        },
        r#"{"type":"event","seq":3,"event":{"type":"track_ended","item_id":7,"reason":"completed"}}"#,
    );
    event(
        4,
        Event::Position {
            position_ms: 250,
            duration_ms: None,
        },
        r#"{"type":"event","seq":4,"event":{"type":"position","position_ms":250,"duration_ms":null}}"#,
    );
    event(
        5,
        Event::Seeked {
            position_ms: 90_000,
        },
        r#"{"type":"event","seq":5,"event":{"type":"seeked","position_ms":90000}}"#,
    );
    event(
        6,
        Event::SeekRejected {
            reason: "no".into(),
        },
        r#"{"type":"event","seq":6,"event":{"type":"seek_rejected","reason":"no"}}"#,
    );
    event(
        7,
        Event::QueueExhausted,
        r#"{"type":"event","seq":7,"event":{"type":"queue_exhausted"}}"#,
    );
    event(
        8,
        Event::Error {
            message: "boom".into(),
        },
        r#"{"type":"event","seq":8,"event":{"type":"error","message":"boom"}}"#,
    );
    event(
        9,
        Event::ShuttingDown,
        r#"{"type":"event","seq":9,"event":{"type":"shutting_down"}}"#,
    );
    event(
        10,
        Event::QueueChanged { queue: queue() },
        &format!(
            r#"{{"type":"event","seq":10,"event":{{"type":"queue_changed","queue":{QUEUE_JSON}}}}}"#
        ),
    );
    event(
        11,
        Event::Resync {
            skipped: 300,
            seq: 11,
            status: status(),
            queue: queue(),
        },
        &format!(
            r#"{{"type":"event","seq":11,"event":{{"type":"resync","skipped":300,"seq":11,"status":{STATUS_JSON},"queue":{QUEUE_JSON}}}}}"#
        ),
    );
    event(
        12,
        Event::SinkReport(SinkReport {
            device: "hw:1,0".into(),
            source: spec(),
            negotiated_format: "S24_3LE".into(),
            bit_perfect: true,
            problem: None,
            hw_params: Some("rate: 96000 (96000/1)\n".into()),
            mode: Some(OutputMode::Exclusive),
            resampled_to: None,
            codec: None,
            lossy: false,
            output: Some("exclusive:hw:1,0".into()),
        }),
        r#"{"type":"event","seq":12,"event":{"type":"sink_report","device":"hw:1,0","source":{"sample_rate":96000,"channels":2,"bits_per_sample":24},"negotiated_format":"S24_3LE","bit_perfect":true,"problem":null,"hw_params":"rate: 96000 (96000/1)\n","mode":"exclusive","resampled_to":null,"codec":null,"lossy":false,"output":"exclusive:hw:1,0"}}"#,
    );
}

#[test]
fn output_events_and_the_state_of_the_device() {
    event(
        13,
        Event::OutputReleased {
            by: Some("jackd".into()),
            reason: ReleaseReason::Requested,
        },
        r#"{"type":"event","seq":13,"event":{"type":"output_released","by":"jackd","reason":"requested"}}"#,
    );
    event(
        14,
        Event::OutputReleased {
            by: None,
            reason: ReleaseReason::Idle,
        },
        r#"{"type":"event","seq":14,"event":{"type":"output_released","by":null,"reason":"idle"}}"#,
    );
    event(
        15,
        Event::OutputAcquired,
        r#"{"type":"event","seq":15,"event":{"type":"output_acquired"}}"#,
    );

    round_trip(&Output::Closed, r#"{"state":"closed"}"#);
    round_trip(&Output::Open, r#"{"state":"open"}"#);
    round_trip(
        &Output::Released {
            by: Some("jackd".into()),
        },
        r#"{"state":"released","by":"jackd"}"#,
    );
    round_trip(
        &Output::Released { by: None },
        r#"{"state":"released","by":null}"#,
    );
}

fn route() -> Route {
    Route {
        id: "shared:bluez_output.AA".into(),
        mode: OutputMode::Shared,
        description: "Soundcore Life P2".into(),
    }
}

fn output(id: &str, mode: OutputMode, bit_perfect: bool) -> OutputInfo {
    OutputInfo {
        id: id.into(),
        mode,
        name: "Fosi Audio DS2".into(),
        detail: Some("USB".into()),
        bit_perfect,
        lossy: false,
        codec: None,
        is_default: false,
    }
}

#[test]
fn output_selection_requests() {
    request(
        1,
        Request::Outputs,
        r#"{"id":1,"request":{"type":"outputs"}}"#,
    );
    request(
        2,
        Request::SetOutput {
            output: "shared:default".into(),
        },
        r#"{"id":2,"request":{"type":"set_output","output":"shared:default"}}"#,
    );
}

#[test]
fn the_list_of_outputs_and_the_route_in_a_status() {
    response(
        1,
        Reply::Ok(Payload::Outputs {
            outputs: vec![output("exclusive:hw:DS2,0", OutputMode::Exclusive, true)],
            current: Some("exclusive:hw:DS2,0".into()),
        }),
        r#"{"type":"response","id":1,"ok":{"type":"outputs","outputs":[{"id":"exclusive:hw:DS2,0","mode":"exclusive","name":"Fosi Audio DS2","detail":"USB","bit_perfect":true,"lossy":false,"codec":null,"is_default":false}],"current":"exclusive:hw:DS2,0"}}"#,
    );
    let with_route = Status {
        route: Some(route()),
        ..status()
    };
    round_trip(
        &with_route,
        &STATUS_JSON.replace(r#""route":null"#, r#""route":{"id":"shared:bluez_output.AA","mode":"shared","description":"Soundcore Life P2"}"#),
    );
}

#[test]
fn volume_requests_events_and_status() {
    request(
        1,
        Request::SetVolume { percent: 40 },
        r#"{"id":1,"request":{"type":"set_volume","percent":40}}"#,
    );
    request(
        2,
        Request::SetMute { mute: true },
        r#"{"id":2,"request":{"type":"set_mute","mute":true}}"#,
    );
    event(
        20,
        Event::VolumeChanged {
            percent: 40,
            muted: false,
        },
        r#"{"type":"event","seq":20,"event":{"type":"volume_changed","percent":40,"muted":false}}"#,
    );
    let with_volume = Status {
        volume: Some(Volume {
            percent: 72,
            muted: true,
        }),
        ..status()
    };
    round_trip(
        &with_volume,
        &STATUS_JSON.replace(
            r#""volume":null"#,
            r#""volume":{"percent":72,"muted":true}"#,
        ),
    );
}

#[test]
fn a_track_started_from_a_1_3_daemon_is_not_gapless() {
    let old = r#"{"type":"event","seq":2,"event":{"type":"track_started","item_id":7,"source":"tidal:1","title":"t","duration_ms":1000,"spec":{"sample_rate":96000,"channels":2,"bits_per_sample":24}}}"#;
    let ServerMessage::Event {
        event: Event::TrackStarted { gapless, .. },
        ..
    } = serde_json::from_str(old).unwrap()
    else {
        panic!("not a track_started")
    };
    assert!(!gapless);
}

#[test]
fn the_quality_tier_requests_events_and_ranges() {
    request(
        1,
        Request::SetMaxQuality {
            quality: Quality::Lossless,
        },
        r#"{"id":1,"request":{"type":"set_max_quality","quality":"lossless"}}"#,
    );
    event(
        22,
        Event::MaxQualityChanged {
            quality: Quality::Hires,
        },
        r#"{"type":"event","seq":22,"event":{"type":"max_quality_changed","quality":"hires"}}"#,
    );
    let range = QualityRange {
        max: Quality::Hires,
        min: Quality::Lossless,
    };
    assert_eq!(
        serde_json::to_string(&range).unwrap(),
        r#"{"max":"hires","min":"lossless"}"#
    );
    assert!(Quality::Low < Quality::High && Quality::Lossless < Quality::Hires);
    assert!(
        StreamQuality {
            requested: Quality::Hires,
            delivered: Quality::Lossless
        }
        .fell_back()
    );
    assert!(
        !StreamQuality {
            requested: Quality::Lossless,
            delivered: Quality::Lossless
        }
        .fell_back()
    );
}

#[test]
fn a_tier_from_a_newer_daemon_is_unknown_not_an_error() {
    let quality: Quality = serde_json::from_str(r#""dolby""#).unwrap();
    assert_eq!(quality, Quality::Unknown);
    let told = StreamQuality {
        requested: Quality::Unknown,
        delivered: Quality::Lossless,
    };
    assert!(!told.fell_back());
}

fn a_track_summary() -> TrackSummary {
    TrackSummary {
        id: "33723914".into(),
        title: "Here to Stay".into(),
        version: None,
        artists: vec![ArtistRef {
            id: "780".into(),
            name: "Korn".into(),
        }],
        album: Some(AlbumRef {
            id: "33723912".into(),
            title: "Untouchables".into(),
            cover: None,
        }),
        duration_ms: Some(271_000),
        explicit: false,
        track_number: Some(2),
        volume_number: None,
        quality: Some(Quality::Hires),
        streamable: true,
    }
}

#[test]
fn search_requests_and_their_defaults() {
    request(
        1,
        Request::Search {
            query: "korn".into(),
            kinds: vec![CatalogKind::Albums, CatalogKind::Tracks],
            offset: 50,
            limit: Some(25),
        },
        r#"{"id":1,"request":{"type":"search","query":"korn","kinds":["albums","tracks"],"offset":50,"limit":25}}"#,
    );
    // Only the query is required: every kind, from the start, the daemon's page size.
    let minimal: ClientMessage =
        serde_json::from_str(r#"{"id":2,"request":{"type":"search","query":"x"}}"#).unwrap();
    assert_eq!(
        minimal.request,
        Request::Search {
            query: "x".into(),
            kinds: vec![],
            offset: 0,
            limit: None
        }
    );
}

#[test]
fn adding_an_album_or_a_playlist_to_the_queue() {
    request(
        1,
        Request::QueueAddFrom {
            from: CatalogRef::Album {
                id: "33723912".into(),
            },
            at: AddAt::End,
        },
        r#"{"id":1,"request":{"type":"queue_add_from","from":{"type":"album","id":"33723912"},"at":{"type":"end"}}}"#,
    );
    request(
        2,
        Request::QueueAddFrom {
            from: CatalogRef::Playlist {
                id: "5545fb2d-fd50-48a0-be5b-33a1d2677e9c".into(),
            },
            at: AddAt::Next,
        },
        r#"{"id":2,"request":{"type":"queue_add_from","from":{"type":"playlist","id":"5545fb2d-fd50-48a0-be5b-33a1d2677e9c"},"at":{"type":"next"}}}"#,
    );
    // `at` may be left out: the end of the queue.
    let minimal: ClientMessage = serde_json::from_str(
        r#"{"id":3,"request":{"type":"queue_add_from","from":{"type":"album","id":"1"}}}"#,
    )
    .unwrap();
    assert_eq!(
        minimal.request,
        Request::QueueAddFrom {
            from: CatalogRef::Album { id: "1".into() },
            at: AddAt::End
        }
    );
    // Something a newer daemon can add parses as unknown, not as a broken message.
    let future: ClientMessage = serde_json::from_str(
        r#"{"id":4,"request":{"type":"queue_add_from","from":{"type":"mix","id":"x"}}}"#,
    )
    .unwrap();
    assert_eq!(
        future.request,
        Request::QueueAddFrom {
            from: CatalogRef::Unknown,
            at: AddAt::End
        }
    );
}

#[test]
fn search_results_carry_a_page_of_each_kind_asked_for() {
    response(
        3,
        Reply::Ok(Payload::SearchResults {
            query: "korn".into(),
            tracks: Some(Page {
                items: vec![a_track_summary()],
                total: 123,
                offset: 0,
            }),
            albums: None,
            artists: Some(Page {
                items: vec![ArtistSummary {
                    id: "780".into(),
                    name: "Korn".into(),
                    picture: None,
                }],
                total: 1,
                offset: 0,
            }),
            playlists: None,
        }),
        r#"{"type":"response","id":3,"ok":{"type":"search_results","query":"korn","tracks":{"items":[{"id":"33723914","title":"Here to Stay","version":null,"artists":[{"id":"780","name":"Korn"}],"album":{"id":"33723912","title":"Untouchables","cover":null},"duration_ms":271000,"explicit":false,"track_number":2,"volume_number":null,"quality":"hires","streamable":true}],"total":123,"offset":0},"albums":null,"artists":{"items":[{"id":"780","name":"Korn","picture":null}],"total":1,"offset":0},"playlists":null}}"#,
    );
}

#[test]
fn the_catalog_errors_have_their_own_codes() {
    for (code, text) in [
        (ErrorCode::NotLoggedIn, "not_logged_in"),
        (ErrorCode::Unavailable, "unavailable"),
        (ErrorCode::RateLimited, "rate_limited"),
    ] {
        assert_eq!(serde_json::to_string(&code).unwrap(), format!("\"{text}\""));
    }
}

#[test]
fn summaries_from_a_daemon_that_says_less_still_parse() {
    // Only the id and the title are required of a track; the rest has defaults.
    let track: TrackSummary = serde_json::from_str(r#"{"id":"1","title":"t"}"#).unwrap();
    assert!(track.artists.is_empty() && track.album.is_none() && track.streamable);
    assert!(!track.explicit && track.quality.is_none());
    let album: AlbumSummary = serde_json::from_str(r#"{"id":"1","title":"t"}"#).unwrap();
    assert!(album.artists.is_empty() && album.track_count.is_none());
    let playlist: PlaylistSummary = serde_json::from_str(r#"{"id":"u","title":"t"}"#).unwrap();
    assert!(playlist.creator.is_none());
}

#[test]
fn a_kind_from_a_newer_daemon_is_unknown_not_an_error() {
    let kind: CatalogKind = serde_json::from_str(r#""podcasts""#).unwrap();
    assert_eq!(kind, CatalogKind::Unknown);
    // And a search results payload that has a kind this version lacks still parses.
    let payload: Payload = serde_json::from_str(
        r#"{"type":"search_results","query":"q","podcasts":{"items":[],"total":0,"offset":0}}"#,
    )
    .unwrap();
    assert!(matches!(
        payload,
        Payload::SearchResults { tracks: None, .. }
    ));
}

#[test]
fn a_1_5_daemon_has_no_catalog_capability() {
    let hello = r#"{"type":"hello","protocol":{"major":1,"minor":5},"server":{"name":"phoniad","version":"0.1.0","pid":1},"capabilities":["volume","quality"]}"#;
    let ServerMessage::Hello(hello) = serde_json::from_str(hello).unwrap() else {
        panic!("not a hello")
    };
    assert!(!hello.capabilities.iter().any(|c| c == CAP_CATALOG));
}

#[test]
fn a_1_4_status_and_track_say_nothing_of_quality() {
    let old = r#"{"state":"playing","track":{"item_id":7,"source":"tidal:1","title":"t","duration_ms":1000},"spec":null,"position_ms":0,"duration_ms":null,"output":{"state":"open"},"route":null,"volume":null}"#;
    let status = serde_json::from_str::<Status>(old).unwrap();
    assert_eq!(status.quality_range, None);
    assert_eq!(status.track.unwrap().quality, None);
    let old = r#"{"type":"event","seq":2,"event":{"type":"track_started","item_id":7,"source":"tidal:1","title":"t","duration_ms":1000,"spec":{"sample_rate":96000,"channels":2,"bits_per_sample":24},"gapless":false}}"#;
    let ServerMessage::Event {
        event: Event::TrackStarted { quality, .. },
        ..
    } = serde_json::from_str(old).unwrap()
    else {
        panic!("not a track_started")
    };
    assert_eq!(quality, None);
}

#[test]
fn a_1_2_status_has_no_volume() {
    let old = r#"{"state":"paused","track":null,"spec":null,"position_ms":0,"duration_ms":null,"output":{"state":"open"},"route":null}"#;
    assert_eq!(serde_json::from_str::<Status>(old).unwrap().volume, None);
}

#[test]
fn output_selection_events_and_the_lost_reason() {
    event(
        16,
        Event::OutputChanged { route: route() },
        r#"{"type":"event","seq":16,"event":{"type":"output_changed","route":{"id":"shared:bluez_output.AA","mode":"shared","description":"Soundcore Life P2"}}}"#,
    );
    event(
        17,
        Event::OutputsChanged,
        r#"{"type":"event","seq":17,"event":{"type":"outputs_changed"}}"#,
    );
    event(
        18,
        Event::OutputReleased {
            by: None,
            reason: ReleaseReason::Lost,
        },
        r#"{"type":"event","seq":18,"event":{"type":"output_released","by":null,"reason":"lost"}}"#,
    );
}

#[test]
fn a_shared_sink_report_carries_how_the_sound_got_there() {
    let report = SinkReport {
        device: "Soundcore Life P2".into(),
        source: spec(),
        negotiated_format: "S32LE".into(),
        bit_perfect: false,
        problem: Some(
            "shared through the sound server, and the SBC codec loses information".into(),
        ),
        hw_params: None,
        mode: Some(OutputMode::Shared),
        resampled_to: Some(48_000),
        codec: Some("SBC".into()),
        lossy: true,
        output: Some("shared:default".into()),
    };
    event(
        19,
        Event::SinkReport(report),
        r#"{"type":"event","seq":19,"event":{"type":"sink_report","device":"Soundcore Life P2","source":{"sample_rate":96000,"channels":2,"bits_per_sample":24},"negotiated_format":"S32LE","bit_perfect":false,"problem":"shared through the sound server, and the SBC codec loses information","hw_params":null,"mode":"shared","resampled_to":48000,"codec":"SBC","lossy":true,"output":"shared:default"}}"#,
    );
}

// ---- compatibility -------------------------------------------------------------------------

#[test]
fn a_request_this_version_does_not_know_parses_as_unknown() {
    let parsed: ClientMessage =
        serde_json::from_str(r#"{"id":5,"request":{"type":"teleport","to":"mars"}}"#).unwrap();
    assert_eq!(
        parsed,
        ClientMessage {
            id: RequestId(5),
            request: Request::Unknown
        }
    );
}

#[test]
fn unknown_fields_are_ignored_everywhere() {
    let parsed: ClientMessage =
        serde_json::from_str(r#"{"id":1,"future":true,"request":{"type":"status","verbose":1}}"#)
            .unwrap();
    assert_eq!(parsed.request, Request::Status);

    let parsed: ServerMessage = serde_json::from_str(r#"{"type":"event","seq":1,"extra":[1],"event":{"type":"position","position_ms":1,"duration_ms":2,"buffered_ms":9}}"#).unwrap();
    assert_eq!(
        parsed,
        ServerMessage::Event {
            seq: 1,
            event: Event::Position {
                position_ms: 1,
                duration_ms: Some(2)
            }
        }
    );
}

#[test]
fn unknown_events_payloads_and_error_codes_do_not_break_a_client() {
    let parsed: ServerMessage = serde_json::from_str(
        r#"{"type":"event","seq":3,"event":{"type":"lyrics_line","text":"la"}}"#,
    )
    .unwrap();
    assert_eq!(
        parsed,
        ServerMessage::Event {
            seq: 3,
            event: Event::Unknown
        }
    );

    let parsed: ServerMessage =
        serde_json::from_str(r#"{"type":"response","id":1,"ok":{"type":"new_thing","x":1}}"#)
            .unwrap();
    assert_eq!(
        parsed,
        ServerMessage::Response {
            id: RequestId(1),
            reply: Reply::Ok(Payload::Unknown)
        }
    );

    let parsed: ServerMessage = serde_json::from_str(
        r#"{"type":"response","id":1,"err":{"code":"quota_exceeded","message":"m"}}"#,
    )
    .unwrap();
    assert_eq!(
        parsed,
        ServerMessage::Response {
            id: RequestId(1),
            reply: Reply::Err(ProtocolError {
                code: ErrorCode::Unknown,
                message: "m".into()
            })
        }
    );
}

#[test]
fn optional_parts_may_be_left_out() {
    // `at` defaults to the end of the queue, and a banner without capabilities is fine.
    let parsed: ClientMessage = serde_json::from_str(
        r#"{"id":1,"request":{"type":"queue_add","tracks":[{"source":"tidal:1"}]}}"#,
    )
    .unwrap();
    assert_eq!(
        parsed.request,
        Request::QueueAdd {
            tracks: vec![NewTrack {
                source: "tidal:1".into()
            }],
            at: AddAt::End
        }
    );

    let parsed: ServerMessage = serde_json::from_str(r#"{"type":"hello","protocol":{"major":1,"minor":2},"server":{"name":"phoniad","version":"9","pid":1}}"#).unwrap();
    let ServerMessage::Hello(hello) = parsed else {
        panic!("not a banner")
    };
    assert!(hello.capabilities.is_empty());
}

#[test]
fn a_status_from_a_daemon_older_than_1_1_has_no_output() {
    let old = r#"{"state":"paused","track":null,"spec":null,"position_ms":0,"duration_ms":null}"#;
    let parsed: Status = serde_json::from_str(old).unwrap();
    assert_eq!(parsed.output, Output::Closed);
}

#[test]
fn a_1_1_daemon_status_and_sink_report_still_parse() {
    let old = r#"{"state":"paused","track":null,"spec":null,"position_ms":0,"duration_ms":null,"output":{"state":"open"}}"#;
    let parsed: Status = serde_json::from_str(old).unwrap();
    assert_eq!(parsed.route, None);

    let old = r#"{"device":"hw:1,0","source":{"sample_rate":96000,"channels":2,"bits_per_sample":24},"negotiated_format":"S24_3LE","bit_perfect":true,"problem":null,"hw_params":null}"#;
    let parsed: SinkReport = serde_json::from_str(old).unwrap();
    assert_eq!(
        (parsed.mode, parsed.resampled_to, parsed.codec, parsed.lossy),
        (None, None, None, false)
    );
}

#[test]
fn a_1_6_status_and_sink_report_say_nothing_of_the_output_id() {
    let old = r#"{"state":"playing","track":null,"spec":null,"position_ms":0,"duration_ms":null,"output":{"state":"open"},"quality_range":null}"#;
    let parsed: Status = serde_json::from_str(old).unwrap();
    assert_eq!(parsed.sink_report, None);

    let old = r#"{"device":"hw:1,0","source":{"sample_rate":96000,"channels":2,"bits_per_sample":24},"negotiated_format":"S24_3LE","bit_perfect":true,"problem":null,"hw_params":null}"#;
    let parsed: SinkReport = serde_json::from_str(old).unwrap();
    assert_eq!(parsed.output, None);
}

#[test]
fn a_1_7_track_and_track_started_say_nothing_of_replay_gain() {
    let old = r#"{"state":"playing","track":{"item_id":7,"source":"tidal:1","title":"t","duration_ms":1000,"quality":null,"cover":null},"spec":null,"position_ms":0,"duration_ms":null}"#;
    let parsed: Status = serde_json::from_str(old).unwrap();
    assert_eq!(parsed.track.unwrap().replay_gain, None);

    let old = r#"{"type":"event","seq":1,"event":{"type":"track_started","item_id":7,"source":"tidal:1","title":"t","duration_ms":1000,"spec":{"sample_rate":96000,"channels":2,"bits_per_sample":24},"gapless":false,"quality":null,"cover":null}}"#;
    let parsed: ServerMessage = serde_json::from_str(old).unwrap();
    let ServerMessage::Event {
        event: Event::TrackStarted { replay_gain, .. },
        ..
    } = parsed
    else {
        panic!("not a track_started event")
    };
    assert_eq!(replay_gain, None);
}

#[test]
fn a_1_12_queue_says_nothing_of_autoplay() {
    let old =
        r#"{"version":1,"items":[],"order":[],"current":null,"shuffle":false,"repeat":"off"}"#;
    let parsed: Queue = serde_json::from_str(old).unwrap();
    assert!(!parsed.autoplay);
}

#[test]
fn a_mode_from_the_future_does_not_break_a_client() {
    let parsed: Route =
        serde_json::from_str(r#"{"id":"x","mode":"cloud","description":"d"}"#).unwrap();
    assert_eq!(parsed.mode, OutputMode::Unknown);
}

#[test]
fn a_release_reason_from_the_future_does_not_break_a_client() {
    let parsed: ServerMessage =
        serde_json::from_str(r#"{"type":"event","seq":1,"event":{"type":"output_released","by":null,"reason":"thermal"}}"#).unwrap();
    assert_eq!(
        parsed,
        ServerMessage::Event {
            seq: 1,
            event: Event::OutputReleased {
                by: None,
                reason: ReleaseReason::Unknown
            }
        }
    );
}

#[test]
fn versions_are_compatible_across_minors_but_not_majors() {
    let v = |major, minor| Version { major, minor };
    assert!(v(1, 0).compatible_with(v(1, 7)));
    assert!(v(1, 7).compatible_with(v(1, 0)));
    assert!(!v(1, 0).compatible_with(v(2, 0)));
    assert_eq!(PROTOCOL, v(1, 15));
}

#[test]
fn every_message_is_one_line() {
    // The framing relies on it: JSON escapes newlines inside strings.
    let m = ServerMessage::Event {
        seq: 1,
        event: Event::Error {
            message: "line one\nline two".into(),
        },
    };
    let json = serde_json::to_string(&m).unwrap();
    assert!(!json.contains('\n'), "{json}");
}

#[test]
fn the_album_artist_tracks_and_albums_requests_and_their_defaults() {
    request(
        1,
        Request::Album {
            id: "33723912".into(),
            limit: Some(20),
        },
        r#"{"id":1,"request":{"type":"album","id":"33723912","limit":20}}"#,
    );
    request(
        2,
        Request::Artist {
            id: "780".into(),
            limit: None,
        },
        r#"{"id":2,"request":{"type":"artist","id":"780","limit":null}}"#,
    );
    request(
        3,
        Request::Tracks {
            from: CatalogRef::ArtistTopTracks { id: "780".into() },
            offset: 50,
            limit: Some(50),
        },
        r#"{"id":3,"request":{"type":"tracks","from":{"type":"artist_top_tracks","id":"780"},"offset":50,"limit":50}}"#,
    );
    request(
        4,
        Request::Albums {
            from: AlbumListRef::ArtistSingles { id: "780".into() },
            offset: 0,
            limit: None,
        },
        r#"{"id":4,"request":{"type":"albums","from":{"type":"artist_singles","id":"780"},"offset":0,"limit":null}}"#,
    );
    // Paging and the limit may be left out.
    let minimal: ClientMessage = serde_json::from_str(
        r#"{"id":5,"request":{"type":"tracks","from":{"type":"album","id":"1"}}}"#,
    )
    .unwrap();
    assert_eq!(
        minimal.request,
        Request::Tracks {
            from: CatalogRef::Album { id: "1".into() },
            offset: 0,
            limit: None
        }
    );
    let album: ClientMessage =
        serde_json::from_str(r#"{"id":6,"request":{"type":"album","id":"1"}}"#).unwrap();
    assert_eq!(
        album.request,
        Request::Album {
            id: "1".into(),
            limit: None
        }
    );
}

#[test]
fn the_track_radio_request_since_1_12() {
    request(
        1,
        Request::Tracks {
            from: CatalogRef::TrackRadio {
                id: "33723914".into(),
            },
            offset: 0,
            limit: Some(50),
        },
        r#"{"id":1,"request":{"type":"tracks","from":{"type":"track_radio","id":"33723914"},"offset":0,"limit":50}}"#,
    );
}

#[test]
fn the_favorite_artists_list_request_and_response_since_1_14() {
    request(
        1,
        Request::Artists {
            from: ArtistListRef::FavoriteArtists,
            offset: 0,
            limit: Some(50),
        },
        r#"{"id":1,"request":{"type":"artists","from":{"type":"favorite_artists"},"offset":0,"limit":50}}"#,
    );
    response(
        2,
        Reply::Ok(Payload::Artists {
            from: ArtistListRef::FavoriteArtists,
            page: Page {
                items: vec![ArtistSummary {
                    id: "780".into(),
                    name: "Korn".into(),
                    picture: None,
                }],
                total: 1,
                offset: 0,
            },
        }),
        r#"{"type":"response","id":2,"ok":{"type":"artists","from":{"type":"favorite_artists"},"page":{"items":[{"id":"780","name":"Korn","picture":null}],"total":1,"offset":0}}}"#,
    );
    // A list a newer client knows and this one does not falls back to `Unknown`, the same as
    // every other `*ListRef`.
    let minimal: ClientMessage = serde_json::from_str(
        r#"{"id":3,"request":{"type":"artists","from":{"type":"something_new"}}}"#,
    )
    .unwrap();
    assert_eq!(
        minimal.request,
        Request::Artists {
            from: ArtistListRef::Unknown,
            offset: 0,
            limit: None
        }
    );
}

#[test]
fn the_recently_played_request_response_and_event_since_1_15() {
    request(
        1,
        Request::RecentlyPlayed,
        r#"{"id":1,"request":{"type":"recently_played"}}"#,
    );
    let played = PlayedTrack {
        source: "tidal:33723914".into(),
        title: Some("Here to Stay".into()),
        artist: Some("Korn".into()),
        duration_ms: Some(271_000),
        cover: None,
        played_at_ms: 1_700_000_000_000,
    };
    response(
        2,
        Reply::Ok(Payload::RecentlyPlayed {
            items: vec![played.clone()],
        }),
        r#"{"type":"response","id":2,"ok":{"type":"recently_played","items":[{"source":"tidal:33723914","title":"Here to Stay","artist":"Korn","duration_ms":271000,"cover":null,"played_at_ms":1700000000000}]}}"#,
    );
    let json = serde_json::to_string(&ServerMessage::Event {
        seq: 1,
        event: Event::RecentlyPlayedChanged {
            items: vec![played.clone()],
        },
    })
    .unwrap();
    let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(
        parsed,
        ServerMessage::Event {
            seq: 1,
            event: Event::RecentlyPlayedChanged {
                items: vec![played]
            }
        }
    );
}

fn an_album_summary() -> AlbumSummary {
    AlbumSummary {
        id: "33723912".into(),
        title: "Issues".into(),
        version: None,
        artists: vec![ArtistRef {
            id: "780".into(),
            name: "Korn".into(),
        }],
        release_date: Some("1999-11-16".into()),
        track_count: Some(16),
        duration_ms: Some(3_200_000),
        explicit: true,
        quality: Some(Quality::Hires),
        kind: Some(AlbumKind::Album),
        copyright: Some("(P) 1999".into()),
        cover: None,
    }
}

#[test]
fn an_album_an_artist_and_pages_come_back_whole() {
    let track = a_track_summary();
    let page = |n: u64| Page {
        items: vec![track.clone()],
        total: n,
        offset: 0,
    };
    response(
        1,
        Reply::Ok(Payload::Album {
            album: an_album_summary(),
            tracks: page(16),
        }),
        r#"{"type":"response","id":1,"ok":{"type":"album","album":{"id":"33723912","title":"Issues","version":null,"artists":[{"id":"780","name":"Korn"}],"release_date":"1999-11-16","track_count":16,"duration_ms":3200000,"explicit":true,"quality":"hires","kind":"album","copyright":"(P) 1999","cover":null},"tracks":{"items":[{"id":"33723914","title":"Here to Stay","version":null,"artists":[{"id":"780","name":"Korn"}],"album":{"id":"33723912","title":"Untouchables","cover":null},"duration_ms":271000,"explicit":false,"track_number":2,"volume_number":null,"quality":"hires","streamable":true}],"total":16,"offset":0}}}"#,
    );
    // The artist: every list a page, and the bio only when there is one.
    let artist = Payload::Artist {
        artist: ArtistSummary {
            id: "780".into(),
            name: "Korn".into(),
            picture: None,
        },
        bio: None,
        top_tracks: page(300),
        albums: Page {
            items: vec![an_album_summary()],
            total: 35,
            offset: 0,
        },
        singles: Page {
            items: vec![],
            total: 30,
            offset: 0,
        },
    };
    let json = serde_json::to_string(&ServerMessage::Response {
        id: RequestId(2),
        reply: Reply::Ok(artist.clone()),
    })
    .unwrap();
    assert!(
        json.contains(r#""type":"artist""#) && json.contains(r#""bio":null"#),
        "{json}"
    );
    let back: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(
        back,
        ServerMessage::Response {
            id: RequestId(2),
            reply: Reply::Ok(artist)
        }
    );
    // A page of tracks says which list it is of.
    response(
        3,
        Reply::Ok(Payload::Tracks {
            from: CatalogRef::ArtistTopTracks { id: "780".into() },
            page: Page {
                items: vec![],
                total: 300,
                offset: 100,
            },
        }),
        r#"{"type":"response","id":3,"ok":{"type":"tracks","from":{"type":"artist_top_tracks","id":"780"},"page":{"items":[],"total":300,"offset":100}}}"#,
    );
    response(
        4,
        Reply::Ok(Payload::Albums {
            from: AlbumListRef::ArtistAlbums { id: "780".into() },
            page: Page {
                items: vec![],
                total: 35,
                offset: 0,
            },
        }),
        r#"{"type":"response","id":4,"ok":{"type":"albums","from":{"type":"artist_albums","id":"780"},"page":{"items":[],"total":35,"offset":0}}}"#,
    );
}

#[test]
fn an_album_from_before_the_views_and_kinds_from_the_future_still_parse() {
    // A 1.6 daemon from before the album view: no kind, no copyright, no disc number.
    let old: AlbumSummary =
        serde_json::from_str(r#"{"id":"1","title":"t","artists":[],"explicit":false}"#).unwrap();
    assert_eq!((old.kind, old.copyright), (None, None));
    let track: TrackSummary = serde_json::from_str(r#"{"id":"1","title":"t"}"#).unwrap();
    assert_eq!(track.volume_number, None);
    // A kind of release, and a list, this version does not know.
    let kind: AlbumKind = serde_json::from_str(r#""compilation""#).unwrap();
    assert_eq!(kind, AlbumKind::Unknown);
    let list: AlbumListRef = serde_json::from_str(r#"{"type":"favorites"}"#).unwrap();
    assert_eq!(list, AlbumListRef::Unknown);
    // An artist answer without a bio.
    let payload: Payload = serde_json::from_str(
        r#"{"type":"artist","artist":{"id":"1","name":"a"},"top_tracks":{"items":[],"total":0,"offset":0},"albums":{"items":[],"total":0,"offset":0},"singles":{"items":[],"total":0,"offset":0}}"#,
    )
    .unwrap();
    assert!(matches!(payload, Payload::Artist { bio: None, .. }));
}

#[test]
fn a_1_9_daemon_has_no_lyrics_capability() {
    let hello = r#"{"type":"hello","protocol":{"major":1,"minor":9},"server":{"name":"phoniad","version":"0.1.0","pid":1},"capabilities":["volume","quality","catalog"]}"#;
    let ServerMessage::Hello(hello) = serde_json::from_str(hello).unwrap() else {
        panic!("not a hello")
    };
    assert!(!hello.capabilities.iter().any(|c| c == CAP_LYRICS));
}

#[test]
fn the_lyrics_request_and_its_answers() {
    request(
        1,
        Request::Lyrics {
            id: "233059491".into(),
        },
        r#"{"id":1,"request":{"type":"lyrics","id":"233059491"}}"#,
    );
    // Synced: timestamped lines, kept in milliseconds, with the provider credited.
    response(
        1,
        Reply::Ok(Payload::Lyrics {
            id: "233059491".into(),
            lyrics: Some(Lyrics {
                lines: vec![LyricLine {
                    at_ms: 12_440,
                    text: "You get a shiver in the dark".into(),
                }],
                plain: Some("You get a shiver in the dark".into()),
                right_to_left: false,
                provider: Some("MUSIXMATCH".into()),
            }),
        }),
        r#"{"type":"response","id":1,"ok":{"type":"lyrics","id":"233059491","lyrics":{"lines":[{"at_ms":12440,"text":"You get a shiver in the dark"}],"plain":"You get a shiver in the dark","right_to_left":false,"provider":"MUSIXMATCH"}}}"#,
    );
    // Plain only: TIDAL has the text but no time-synced version.
    response(
        2,
        Reply::Ok(Payload::Lyrics {
            id: "1".into(),
            lyrics: Some(Lyrics {
                lines: vec![],
                plain: Some("Some words".into()),
                right_to_left: false,
                provider: None,
            }),
        }),
        r#"{"type":"response","id":2,"ok":{"type":"lyrics","id":"1","lyrics":{"lines":[],"plain":"Some words","right_to_left":false,"provider":null}}}"#,
    );
    // None at all, for an instrumental.
    response(
        3,
        Reply::Ok(Payload::Lyrics {
            id: "518338".into(),
            lyrics: None,
        }),
        r#"{"type":"response","id":3,"ok":{"type":"lyrics","id":"518338","lyrics":null}}"#,
    );
}

#[test]
fn the_playlist_folder_request_and_its_answers() {
    request(
        1,
        Request::PlaylistFolder {
            folder: None,
            offset: 0,
            limit: None,
        },
        r#"{"id":1,"request":{"type":"playlist_folder","folder":null,"offset":0,"limit":null}}"#,
    );
    response(
        1,
        Reply::Ok(Payload::PlaylistFolder {
            folder: None,
            page: Page {
                items: vec![
                    FolderEntry::Folder {
                        id: "f1".into(),
                        name: "Moods".into(),
                        item_count: 3,
                    },
                    FolderEntry::Playlist(PlaylistSummary {
                        id: "p1".into(),
                        title: "Dark Jazz".into(),
                        creator: None,
                        description: None,
                        track_count: Some(75),
                        duration_ms: Some(26_712_000),
                        cover: Some("cover-id".into()),
                    }),
                ],
                total: 2,
                offset: 0,
            },
        }),
        r#"{"type":"response","id":1,"ok":{"type":"playlist_folder","folder":null,"page":{"items":[{"type":"folder","id":"f1","name":"Moods","item_count":3},{"type":"playlist","id":"p1","title":"Dark Jazz","creator":null,"description":null,"track_count":75,"duration_ms":26712000,"cover":"cover-id"}],"total":2,"offset":0}}}"#,
    );
}
