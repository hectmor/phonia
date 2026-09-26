use super::inner::Inner;
use super::*;
use crate::engine::Advance;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use std::collections::HashSet;

fn track(name: &str) -> QueueTrack {
    QueueTrack {
        source: TrackRef(name.to_string()),
        title: Some(name.to_string()),
        duration: None,
    }
}

/// A queue holding `names` in order, and the ids of its entries.
fn queue_of(names: &[&str]) -> (Inner, Vec<ItemId>) {
    let mut inner = Inner::new(1);
    let ids = inner.add(names.iter().map(|name| track(name)));
    (inner, ids)
}

/// The queue as a list shows it.
fn listed(inner: &Inner) -> Vec<String> {
    inner
        .snapshot()
        .items
        .iter()
        .map(|item| item.track.source.0.clone())
        .collect()
}

fn name_of(inner: &Inner, id: Option<ItemId>) -> Option<String> {
    id.map(|id| inner.item(id).unwrap().track.source.0.clone())
}

/// Asks what comes next and, like the engine, opens it.
fn play(inner: &mut Inner, how: Advance) -> Option<String> {
    let id = inner.advance(how)?;
    assert!(inner.commit(id));
    name_of(inner, Some(id))
}

fn set_current(inner: &mut Inner, id: ItemId) {
    assert!(inner.commit(id));
}

// ---- identity ------------------------------------------------------------------------------

#[test]
fn item_ids_round_trip_through_track_refs() {
    let id = ItemId(42);
    assert_eq!(ItemId::from_ref(&id.track_ref()), Some(id));
    assert_eq!(ItemId::from_ref(&TrackRef("not an id".into())), None);
}

#[test]
fn the_same_track_queued_twice_is_two_entries() {
    let mut inner = Inner::new(1);
    let ids = inner.add([track("a"), track("a"), track("b")]);
    assert_eq!(
        ids.iter().collect::<HashSet<_>>().len(),
        3,
        "each entry has its own id"
    );

    set_current(&mut inner, ids[1]);
    assert_eq!(answer(&mut inner, Advance::Next), Some("b".into()));
    inner.commit(ids[1]); // the offer was only looked at
    assert_eq!(
        inner.advance(Advance::Previous),
        Some(ids[0]),
        "back to the FIRST a, which is a different entry"
    );
}

#[test]
fn ids_are_never_reused_after_a_removal() {
    let (mut inner, ids) = queue_of(&["a"]);
    inner.remove(&ids);
    let again = inner.add([track("a")]);
    assert_ne!(again[0], ids[0]);
}

// ---- the Advance x Repeat table ------------------------------------------------------------

/// A queue [a, b, c] with `current` opened directly (no history), so `Previous` follows the order.
fn positioned(repeat: Repeat, current: usize) -> (Inner, Vec<ItemId>) {
    let (mut inner, ids) = queue_of(&["a", "b", "c"]);
    inner.set_repeat(repeat);
    set_current(&mut inner, ids[current]);
    (inner, ids)
}

fn answer(inner: &mut Inner, how: Advance) -> Option<String> {
    let id = inner.advance(how);
    name_of(inner, id)
}

#[test]
fn repeat_off_in_the_middle() {
    for (how, expected) in [
        (Advance::Auto, Some("c")),
        (Advance::Next, Some("c")),
        (Advance::Previous, Some("a")),
        (Advance::Restart, Some("b")),
    ] {
        let (mut inner, _) = positioned(Repeat::Off, 1);
        assert_eq!(
            answer(&mut inner, how),
            expected.map(String::from),
            "{how:?}"
        );
    }
}

#[test]
fn repeat_off_at_the_end_and_at_the_start() {
    let (mut inner, _) = positioned(Repeat::Off, 2);
    assert_eq!(answer(&mut inner, Advance::Auto), None, "the queue ends");
    let (mut inner, _) = positioned(Repeat::Off, 2);
    assert_eq!(answer(&mut inner, Advance::Next), None);

    let (mut inner, _) = positioned(Repeat::Off, 0);
    assert_eq!(
        answer(&mut inner, Advance::Previous),
        None,
        "nothing before the first"
    );
}

#[test]
fn repeat_all_wraps_in_both_directions() {
    let (mut inner, _) = positioned(Repeat::All, 2);
    assert_eq!(answer(&mut inner, Advance::Auto), Some("a".into()));
    let (mut inner, _) = positioned(Repeat::All, 2);
    assert_eq!(answer(&mut inner, Advance::Next), Some("a".into()));
    let (mut inner, _) = positioned(Repeat::All, 0);
    assert_eq!(answer(&mut inner, Advance::Previous), Some("c".into()));
}

#[test]
fn repeat_one_repeats_on_auto_but_a_skip_moves_on() {
    let (mut inner, _) = positioned(Repeat::One, 1);
    assert_eq!(
        answer(&mut inner, Advance::Auto),
        Some("b".into()),
        "the track ended by itself"
    );
    let (mut inner, _) = positioned(Repeat::One, 1);
    assert_eq!(
        answer(&mut inner, Advance::Next),
        Some("c".into()),
        "the user skipped"
    );
    let (mut inner, _) = positioned(Repeat::One, 2);
    assert_eq!(
        answer(&mut inner, Advance::Next),
        Some("a".into()),
        "and a skip from the last wraps"
    );
    let (mut inner, _) = positioned(Repeat::One, 1);
    assert_eq!(answer(&mut inner, Advance::Restart), Some("b".into()));
}

#[test]
fn with_nothing_playing_everything_but_restart_starts_at_the_first_entry() {
    for how in [Advance::Auto, Advance::Next, Advance::Previous] {
        let (mut inner, _) = queue_of(&["a", "b"]);
        assert_eq!(answer(&mut inner, how), Some("a".into()), "{how:?}");
    }
    let (mut inner, _) = queue_of(&["a", "b"]);
    assert_eq!(answer(&mut inner, Advance::Restart), None);
}

#[test]
fn an_empty_queue_has_nothing_for_anything() {
    for repeat in [Repeat::Off, Repeat::One, Repeat::All] {
        for how in [
            Advance::Auto,
            Advance::Next,
            Advance::Previous,
            Advance::Restart,
        ] {
            let mut inner = Inner::new(1);
            inner.set_repeat(repeat);
            assert_eq!(inner.advance(how), None, "{repeat:?} {how:?}");
        }
    }
}

#[test]
fn a_single_entry_ends_unless_it_repeats() {
    let (mut inner, ids) = queue_of(&["a"]);
    set_current(&mut inner, ids[0]);
    assert_eq!(inner.advance(Advance::Auto), None);
    inner.set_repeat(Repeat::All);
    assert_eq!(inner.advance(Advance::Auto), Some(ids[0]));
    inner.set_repeat(Repeat::One);
    assert_eq!(inner.advance(Advance::Auto), Some(ids[0]));
}

// ---- the cursor commits on open, not on advance --------------------------------------------

#[test]
fn advance_does_not_move_the_cursor_only_opening_does() {
    let (mut inner, ids) = queue_of(&["a", "b", "c"]);
    set_current(&mut inner, ids[0]);

    assert_eq!(inner.advance(Advance::Next), Some(ids[1]));
    assert_eq!(
        inner.snapshot().current,
        Some(ids[0]),
        "offered, not opened"
    );

    inner.commit(ids[1]);
    assert_eq!(inner.snapshot().current, Some(ids[1]));
}

#[test]
fn advancing_twice_before_an_open_chains() {
    let (mut inner, ids) = queue_of(&["a", "b", "c", "d"]);
    set_current(&mut inner, ids[0]);
    assert_eq!(inner.advance(Advance::Next), Some(ids[1]));
    assert_eq!(
        inner.advance(Advance::Next),
        Some(ids[2]),
        "two skips in a row are two entries ahead"
    );
}

#[test]
fn opening_something_else_supersedes_what_was_offered() {
    let (mut inner, ids) = queue_of(&["a", "b", "c"]);
    set_current(&mut inner, ids[0]);
    inner.advance(Advance::Next); // offered b ...
    inner.commit(ids[2]); // ... but the user jumped to c
    assert_eq!(
        inner.advance(Advance::Next),
        None,
        "measured from c, not from the abandoned offer"
    );
}

#[test]
fn reopening_the_current_entry_changes_nothing() {
    let (mut inner, ids) = queue_of(&["a", "b"]);
    set_current(&mut inner, ids[0]);
    set_current(&mut inner, ids[1]);
    let before = inner.snapshot();
    // A restart, a repeat and a seek that reopens the stream all open the current entry again.
    set_current(&mut inner, ids[1]);
    assert_eq!(inner.snapshot().current, before.current);
    assert_eq!(
        answer(&mut inner, Advance::Previous),
        Some("a".into()),
        "history is untouched"
    );
}

#[test]
fn opening_an_entry_that_no_longer_exists_is_refused() {
    let (mut inner, ids) = queue_of(&["a"]);
    inner.remove(&ids);
    assert!(!inner.commit(ids[0]));
}

// ---- Previous and history ------------------------------------------------------------------

#[test]
fn previous_goes_back_to_what_was_actually_played() {
    let (mut inner, ids) = queue_of(&["a", "b", "c", "d"]);
    for id in [ids[0], ids[2], ids[3]] {
        set_current(&mut inner, id); // a, then jumped to c, then d
    }
    assert_eq!(
        answer(&mut inner, Advance::Previous),
        Some("c".into()),
        "not b, which was never played"
    );
    inner.commit(ids[2]);
    assert_eq!(answer(&mut inner, Advance::Previous), Some("a".into()));
    inner.commit(ids[0]);
    assert_eq!(
        answer(&mut inner, Advance::Previous),
        None,
        "history is used up and a is first"
    );
}

#[test]
fn going_back_then_forward_does_not_replay_the_same_entries_backwards() {
    let (mut inner, ids) = queue_of(&["a", "b", "c"]);
    for id in [ids[0], ids[1], ids[2]] {
        set_current(&mut inner, id);
    }
    assert_eq!(play(&mut inner, Advance::Previous), Some("b".into()));
    assert_eq!(play(&mut inner, Advance::Previous), Some("a".into()));
    assert_eq!(play(&mut inner, Advance::Next), Some("b".into()));
}

#[test]
fn history_is_bounded() {
    let mut inner = Inner::new(1);
    let ids = inner.add((0..500).map(|i| track(&i.to_string())));
    for id in &ids {
        set_current(&mut inner, *id);
    }
    assert_eq!(
        inner.history_len(),
        200,
        "only the most recent entries are remembered"
    );
}

#[test]
fn going_back_past_the_history_follows_the_order_without_bouncing() {
    let mut inner = Inner::new(1);
    let ids = inner.add((0..500).map(|i| track(&i.to_string())));
    for id in &ids {
        set_current(&mut inner, *id);
    }

    // 200 steps come from the history, then the order takes over. It must keep going backwards
    // to the first entry instead of bouncing between the last two entries it visited.
    let mut visited = vec![inner.snapshot().current.unwrap()];
    while let Some(id) = inner.advance(Advance::Previous) {
        inner.commit(id);
        visited.push(id);
        assert!(visited.len() <= 500, "walking back never ends");
    }
    let expected: Vec<ItemId> = ids.iter().rev().copied().collect();
    assert_eq!(visited, expected, "every entry once, newest to oldest");
}

#[test]
fn previous_through_the_order_alone_walks_to_the_start_and_stops() {
    let (mut inner, ids) = queue_of(&["a", "b", "c", "d"]);
    set_current(&mut inner, ids[3]); // opened directly: no history
    assert_eq!(play(&mut inner, Advance::Previous), Some("c".into()));
    assert_eq!(play(&mut inner, Advance::Previous), Some("b".into()));
    assert_eq!(play(&mut inner, Advance::Previous), Some("a".into()));
    assert_eq!(inner.advance(Advance::Previous), None);
}

// ---- editing keeps the cursor on its entry -------------------------------------------------

#[test]
fn edits_elsewhere_leave_the_cursor_on_the_same_entry() {
    let (mut inner, ids) = queue_of(&["a", "b", "c", "d", "e"]);
    set_current(&mut inner, ids[2]);

    inner.remove(&[ids[0]]);
    inner.move_to(ids[4], 0);
    inner.insert(1, [track("x")]);
    inner.add([track("y")]);

    assert_eq!(name_of(&inner, inner.snapshot().current), Some("c".into()));
    assert_eq!(listed(&inner), ["e", "x", "b", "c", "d", "y"]);
    assert_eq!(answer(&mut inner, Advance::Next), Some("d".into()));
}

#[test]
fn move_clamps_and_reports_unknown_entries() {
    let (mut inner, ids) = queue_of(&["a", "b", "c"]);
    assert!(inner.move_to(ids[0], 99));
    assert_eq!(listed(&inner), ["b", "c", "a"]);
    assert!(!inner.move_to(ItemId(9999), 0));
}

#[test]
fn removing_entries_reports_how_many_existed() {
    let (mut inner, ids) = queue_of(&["a", "b", "c"]);
    assert_eq!(inner.remove(&[ids[0], ItemId(9999), ids[2]]), 2);
    assert_eq!(listed(&inner), ["b"]);
}

#[test]
fn play_next_goes_right_after_the_playing_entry() {
    let (mut inner, ids) = queue_of(&["a", "b", "c"]);
    set_current(&mut inner, ids[0]);
    inner.play_next([track("x"), track("y")]);
    assert_eq!(listed(&inner), ["a", "x", "y", "b", "c"]);
    assert_eq!(answer(&mut inner, Advance::Next), Some("x".into()));
}

#[test]
fn play_next_with_nothing_playing_goes_first() {
    let (mut inner, _) = queue_of(&["a", "b"]);
    inner.play_next([track("x")]);
    assert_eq!(listed(&inner), ["x", "a", "b"]);
}

#[test]
fn clear_forgets_everything_but_never_reuses_ids() {
    let (mut inner, ids) = queue_of(&["a", "b"]);
    set_current(&mut inner, ids[0]);
    inner.clear();
    let snapshot = inner.snapshot();
    assert!(snapshot.items.is_empty() && snapshot.order.is_empty() && snapshot.current.is_none());
    assert_eq!(inner.advance(Advance::Auto), None);
}

// ---- removing the entry that is playing ----------------------------------------------------

#[test]
fn removing_the_playing_entry_makes_the_next_one_take_its_place() {
    let (mut inner, ids) = queue_of(&["a", "b", "c"]);
    set_current(&mut inner, ids[1]);
    inner.remove(&[ids[1]]);

    assert_eq!(inner.snapshot().current, None);
    assert_eq!(
        answer(&mut inner, Advance::Auto),
        Some("c".into()),
        "c slid into b's slot"
    );
}

#[test]
fn after_removing_the_playing_entry_previous_still_finds_what_came_before() {
    let (mut inner, ids) = queue_of(&["a", "b", "c"]);
    set_current(&mut inner, ids[1]);
    inner.remove(&[ids[1]]);
    assert_eq!(answer(&mut inner, Advance::Previous), Some("a".into()));
}

#[test]
fn removing_the_playing_last_entry_ends_the_queue_unless_it_wraps() {
    let (mut inner, ids) = queue_of(&["a", "b"]);
    set_current(&mut inner, ids[1]);
    inner.remove(&[ids[1]]);
    assert_eq!(inner.advance(Advance::Auto), None);

    let (mut inner, ids) = queue_of(&["a", "b"]);
    inner.set_repeat(Repeat::All);
    set_current(&mut inner, ids[1]);
    inner.remove(&[ids[1]]);
    assert_eq!(answer(&mut inner, Advance::Auto), Some("a".into()));
}

#[test]
fn the_vacated_slot_follows_edits_around_it() {
    let (mut inner, ids) = queue_of(&["a", "b", "c", "d"]);
    set_current(&mut inner, ids[2]);
    inner.remove(&[ids[2]]); // c leaves; d is next
    inner.remove(&[ids[0]]); // an entry before the slot goes: d is still next
    assert_eq!(answer(&mut inner, Advance::Auto), Some("d".into()));

    let (mut inner, ids) = queue_of(&["a", "b", "c", "d"]);
    set_current(&mut inner, ids[2]);
    inner.remove(&[ids[2]]);
    inner.insert(0, [track("x")]); // an entry added before the slot: d is still next
    assert_eq!(answer(&mut inner, Advance::Auto), Some("d".into()));
}

// ---- shuffle -------------------------------------------------------------------------------

fn is_permutation(inner: &Inner) -> bool {
    let snapshot = inner.snapshot();
    let mut a: Vec<_> = snapshot.items.iter().map(|item| item.id).collect();
    let mut b = snapshot.order.clone();
    a.sort();
    b.sort();
    a == b
}

#[test]
fn shuffle_keeps_the_current_entry_first_and_off_restores_queue_order() {
    let mut inner = Inner::new(7);
    let ids = inner.add((0..20).map(|i| track(&i.to_string())));
    set_current(&mut inner, ids[5]);

    inner.set_shuffle(true);
    let shuffled = inner.snapshot();
    assert!(is_permutation(&inner));
    assert_eq!(
        shuffled.order[0], ids[5],
        "the playing entry stays where the cursor is"
    );
    assert_ne!(shuffled.order, ids, "and the rest is in a different order");

    inner.set_shuffle(false);
    assert_eq!(inner.snapshot().order, ids);
    assert_eq!(
        inner.snapshot().current,
        Some(ids[5]),
        "the cursor did not move"
    );
}

#[test]
fn a_shuffled_queue_plays_every_entry_exactly_once_per_cycle() {
    for seed in 0..30 {
        let mut inner = Inner::new(seed);
        let ids = inner.add((0..12).map(|i| track(&i.to_string())));
        inner.set_shuffle(true);

        let mut played = Vec::new();
        while let Some(id) = inner.advance(Advance::Auto) {
            inner.commit(id);
            played.push(id);
        }
        let mut sorted = played.clone();
        sorted.sort();
        let mut all = ids.clone();
        all.sort();
        assert_eq!(sorted, all, "seed {seed}: each entry once");
    }
}

#[test]
fn repeat_all_reshuffles_each_cycle_without_repeating_at_the_boundary() {
    for seed in 0..60 {
        let mut inner = Inner::new(seed);
        let ids = inner.add((0..5).map(|i| track(&i.to_string())));
        inner.set_shuffle(true);
        inner.set_repeat(Repeat::All);

        let mut played = Vec::new();
        for _ in 0..ids.len() * 4 {
            let id = inner.advance(Advance::Auto).expect("repeat-all never ends");
            inner.commit(id);
            played.push(id);
            assert!(is_permutation(&inner));
        }
        for cycle in played.chunks(ids.len()) {
            let mut sorted = cycle.to_vec();
            sorted.sort();
            let mut all = ids.clone();
            all.sort();
            assert_eq!(
                sorted, all,
                "seed {seed}: every cycle plays each entry once"
            );
        }
        for pair in played.windows(2) {
            assert_ne!(
                pair[0], pair[1],
                "seed {seed}: the same entry twice in a row"
            );
        }
    }
}

#[test]
fn entries_added_while_shuffled_go_to_the_end_of_the_play_order() {
    let mut inner = Inner::new(3);
    inner.add((0..6).map(|i| track(&i.to_string())));
    inner.set_shuffle(true);
    let before = inner.snapshot().order;

    let added = inner.add([track("x")]);
    let after = inner.snapshot().order;
    assert_eq!(
        after[..before.len()],
        before[..],
        "the existing order is untouched"
    );
    assert_eq!(after.last(), added.last());
    assert!(is_permutation(&inner));
}

#[test]
fn the_same_seed_shuffles_the_same_way() {
    let shuffled = |seed| {
        let mut inner = Inner::new(seed);
        inner.add((0..10).map(|i| track(&i.to_string())));
        inner.set_shuffle(true);
        inner.snapshot().order
    };
    assert_eq!(shuffled(5), shuffled(5));
    assert_ne!(shuffled(5), shuffled(6));
}

// ---- metadata ------------------------------------------------------------------------------

#[test]
fn opening_a_track_fills_in_only_what_was_missing() {
    let mut inner = Inner::new(1);
    let ids = inner.add([QueueTrack {
        source: TrackRef("a".into()),
        title: None,
        duration: None,
    }]);
    let before = inner.snapshot().version;

    inner.record_meta(
        ids[0],
        Some("Title"),
        Some(std::time::Duration::from_secs(9)),
    );
    let item = inner.snapshot().items[0].clone();
    assert_eq!(item.track.title.as_deref(), Some("Title"));
    assert_eq!(item.track.duration, Some(std::time::Duration::from_secs(9)));
    assert!(inner.snapshot().version > before);

    inner.record_meta(
        ids[0],
        Some("Other"),
        Some(std::time::Duration::from_secs(1)),
    );
    assert_eq!(
        inner.snapshot().items[0],
        item,
        "what is already known is kept"
    );
}

// ---- random edit scripts -------------------------------------------------------------------

/// Whatever is done to a queue, its invariants hold: the play order is a permutation of the
/// entries, the ids are unique, and the cursor and history only mention entries that exist.
#[test]
fn invariants_hold_under_random_edit_scripts() {
    for seed in 0..40u64 {
        let mut script = StdRng::seed_from_u64(seed);
        let mut inner = Inner::new(seed);
        let mut counter = 0;
        let new_track = |counter: &mut i32| {
            *counter += 1;
            track(&counter.to_string())
        };
        inner.add((0..3).map(|_| new_track(&mut counter)));

        for step in 0..300 {
            let snapshot = inner.snapshot();
            let ids: Vec<ItemId> = snapshot.items.iter().map(|item| item.id).collect();
            let pick =
                |script: &mut StdRng| ids.get(script.random_range(0..ids.len().max(1))).copied();

            match script.random_range(0..11) {
                0 => {
                    inner.add([new_track(&mut counter)]);
                }
                1 => {
                    let at = script.random_range(0..=ids.len());
                    inner.insert(at, [new_track(&mut counter), new_track(&mut counter)]);
                }
                2 => {
                    inner.play_next([new_track(&mut counter)]);
                }
                3 => {
                    if let Some(id) = pick(&mut script) {
                        inner.remove(&[id]);
                    }
                }
                4 => {
                    if let Some(id) = pick(&mut script) {
                        inner.move_to(id, script.random_range(0..=ids.len()));
                    }
                }
                5 => inner.set_shuffle(script.random_bool(0.5)),
                6 => inner
                    .set_repeat([Repeat::Off, Repeat::One, Repeat::All][script.random_range(0..3)]),
                7 => {
                    if script.random_range(0..10) == 0 {
                        inner.clear();
                    }
                }
                8 => {
                    if let Some(id) = pick(&mut script) {
                        inner.commit(id);
                    }
                }
                _ => {
                    let how = [
                        Advance::Auto,
                        Advance::Next,
                        Advance::Previous,
                        Advance::Restart,
                    ][script.random_range(0..4)];
                    if let Some(id) = inner.advance(how) {
                        assert!(
                            inner.commit(id),
                            "seed {seed} step {step}: advance offered a missing entry"
                        );
                    }
                }
            }

            let snapshot = inner.snapshot();
            assert!(
                is_permutation(&inner),
                "seed {seed} step {step}: order is not a permutation of the entries"
            );
            let unique: HashSet<_> = snapshot.items.iter().map(|item| item.id).collect();
            assert_eq!(
                unique.len(),
                snapshot.items.len(),
                "seed {seed} step {step}: duplicate ids"
            );
            if let Some(current) = snapshot.current {
                assert!(
                    unique.contains(&current),
                    "seed {seed} step {step}: the cursor is on an entry that is gone"
                );
            }
        }
    }
}
