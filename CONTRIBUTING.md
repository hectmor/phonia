# Contributing

How phonia is built, tested and worked on: the commands CI runs, the tests that
need real hardware and never run by themselves, and the process every change
goes through. What the project is is in the [README](README.md); what is done
and what remains is in [ROADMAP.md](ROADMAP.md); why things are the way they
are is in [docs/DECISIONS.md](docs/DECISIONS.md).

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
any single assistant's own memory of past sessions. That is what this file, [ROADMAP.md](ROADMAP.md) and
[docs/DECISIONS.md](docs/DECISIONS.md) are for together: the roadmap is the current *what*, the
decisions log is the *why*, and this file is the *how*, so an audit can start here and check the other two against
the issue tracker and the git history.

**The people and the process.** One person (@hectmor) directs the project and makes every product
and architectural call; an AI pair does the planning and the implementing. A harder design
question (a new subsystem, a protocol change, a UI decision with real trade-offs) is planned first
by a more capable model, presented as a short written plan with the open decisions called out
explicitly, and only implemented once the person has approved it; a well-scoped, already-decided
piece of work goes straight to implementation. Nothing is committed, pushed, or opened as a pull
request without the person's explicit go-ahead in that session — approval for one piece of work
does not carry over to the next.

**Branching and pull requests.** The exact format of a commit message, a branch name and a
pull request is in [docs/COMMITS.md](docs/COMMITS.md). Every issue (or a part of one, when it is split into several) gets
its own branch off `develop`, never off another open feature branch (no stacking): a stacked branch
can't be merged independently, and a base branch that gets deleted before retargeting orphans it.
Pull requests target `develop`, not `main`. Each one is assigned to the project owner, carries the
milestone of the issue it belongs to, and an `enhancement` (or `bug`/`documentation`) label — GitHub's
own filtering is meant to stay a second, independent source of truth alongside this file. Commits
and pull request descriptions carry no `Co-Authored-By` trailer, no `Claude-Session` link and no
"Generated with" footer, by the project owner's explicit standing instruction — reaffirmed
2026-10-19. Some commits from before that date do carry them; that is history, not the convention.
Issues are closed by hand after their
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

**Language.** Everything written from 2026-10-19 on is in English: issues, pull requests, commit and
tag messages, and every file in the repository. Milestone titles and the issues opened before that
date keep their original Spanish names on GitHub; where this repository quotes one, it translates
the title there, not on GitHub.

**Licensing.** phonia is dual-licensed, `MIT OR Apache-2.0`, the Rust ecosystem's own convention:
MIT for brevity, Apache-2.0 for its explicit patent grant, whichever the user prefers. Unless you
state otherwise, any contribution you submit for inclusion is licensed under both, with no
additional terms. Before a binary package is published, `THIRD-PARTY-LICENSES.md` has to ship with
it — nearly every dependency requires its copyright notice to be reproduced in a binary
distribution, and that file is generated, never edited by hand.

**Keeping this current.** `README.md` stays an entry point: it links to the documents under `docs/`
and never restates a flag, a config key or a protocol field, because that is how it grew into a
889-line manual the first time. `ROADMAP.md` is updated in the same session a part of an issue
merges, not as a later cleanup pass; `docs/DECISIONS.md` gets a new, dated entry for a decision
worth recording at the time it's made, never edited afterwards to hide that something changed — a
decision that gets revisited gets a new entry saying so.
