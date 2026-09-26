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
            duration_ms: Some(215_000),
        }),
        spec: Some(spec()),
        position_ms: 1_234,
        duration_ms: Some(215_000),
        output: Output::Open,
        route: None,
        volume: None,
    }
}

const STATUS_JSON: &str = r#"{"state":"playing","track":{"item_id":7,"source":"file:/music/a.flac","title":"a.flac","duration_ms":215000},"spec":{"sample_rate":96000,"channels":2,"bits_per_sample":24},"position_ms":1234,"duration_ms":215000,"output":{"state":"open"},"route":null,"volume":null}"#;

fn queue() -> Queue {
    Queue {
        version: 5,
        items: vec![
            QueueItem {
                id: ItemId(7),
                source: "file:/music/a.flac".into(),
                title: Some("a.flac".into()),
                duration_ms: Some(215_000),
            },
            QueueItem {
                id: ItemId(8),
                source: "tidal:233059491".into(),
                title: None,
                duration_ms: None,
            },
        ],
        order: vec![ItemId(8), ItemId(7)],
        current: Some(ItemId(7)),
        shuffle: true,
        repeat: Repeat::All,
    }
}

const QUEUE_JSON: &str = r#"{"version":5,"items":[{"id":7,"source":"file:/music/a.flac","title":"a.flac","duration_ms":215000},{"id":8,"source":"tidal:233059491","title":null,"duration_ms":null}],"order":[8,7],"current":7,"shuffle":true,"repeat":"all"}"#;

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
            ],
        }),
        r#"{"type":"hello","protocol":{"major":1,"minor":3},"server":{"name":"phoniad","version":"0.1.0","pid":1234},"capabilities":["output_release","output_select","volume"]}"#,
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
            r#"{{"type":"response","id":2,"ok":{{"type":"status","state":"playing","track":{},"spec":{},"position_ms":1234,"duration_ms":215000,"output":{{"state":"open"}},"route":null,"volume":null}}}}"#,
            r#"{"item_id":7,"source":"file:/music/a.flac","title":"a.flac","duration_ms":215000}"#,
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
            duration_ms: Some(1000),
            spec: spec(),
        },
        r#"{"type":"event","seq":2,"event":{"type":"track_started","item_id":7,"source":"tidal:1","title":"t","duration_ms":1000,"spec":{"sample_rate":96000,"channels":2,"bits_per_sample":24}}}"#,
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
        }),
        r#"{"type":"event","seq":12,"event":{"type":"sink_report","device":"hw:1,0","source":{"sample_rate":96000,"channels":2,"bits_per_sample":24},"negotiated_format":"S24_3LE","bit_perfect":true,"problem":null,"hw_params":"rate: 96000 (96000/1)\n","mode":"exclusive","resampled_to":null,"codec":null,"lossy":false}}"#,
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
    };
    event(
        19,
        Event::SinkReport(report),
        r#"{"type":"event","seq":19,"event":{"type":"sink_report","device":"Soundcore Life P2","source":{"sample_rate":96000,"channels":2,"bits_per_sample":24},"negotiated_format":"S32LE","bit_perfect":false,"problem":"shared through the sound server, and the SBC codec loses information","hw_params":null,"mode":"shared","resampled_to":48000,"codec":"SBC","lossy":true}}"#,
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
    assert_eq!(PROTOCOL, v(1, 3));
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
