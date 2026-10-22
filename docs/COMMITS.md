# Commits, branches and pull requests

The rules every change follows. The *how* of building and testing is in
[CONTRIBUTING.md](../CONTRIBUTING.md); this file is only about what goes into git and GitHub.

Everything here is written in English: issues, branches, commits, tags, pull requests. Nothing
carries a `Co-Authored-By` trailer, a `Claude-Session` link or a "Generated with" footer.

## Commit messages

```
<type>: <subject>
```

No scope. The subject is imperative, lowercase, no trailing period, under 72 characters, and says
what the change does — it names the area in its own words when that matters. A body is added only
when the *why* is not obvious from the subject.

| Type | Description | Example |
| --- | --- | --- |
| `feat` | New functionality | `feat: add the lyrics panel` |
| `fix` | Bug fix | `fix: handle an empty segment list in dash` |
| `update` | Change to existing functionality | `update: move the queue to the right pane` |
| `refactor` | Refactor with no functional change | `refactor: extract the sample clock into a helper` |
| `chore` | Dependencies, configuration, maintenance | `chore: upgrade symphonia to 0.5.5` |
| `docs` | Documentation | `docs: add the commit convention` |
| `merge` | Merge commit, left as git writes it | |

Commits before 2026-10-22 do not follow this convention; that is history, not the convention.

## Issues

Every issue is assigned to @hectmor and carries a label (`enhancement`, `bug` or
`documentation`) and the milestone it belongs to. Issues are closed by hand once their pull
requests merge.

## Branches

One branch per issue, named `<issue-number>-<short-slug>`, cut off `develop` — never off another
open feature branch. An issue split into parts gets one branch per part, each off `develop`.
Branches are deleted, local and remote, once their pull request merges.

## Pull requests

Pull requests target `develop`, never `main`. The title follows the commit format above; the
description is concise — what changed, why, and how it was verified. Each one is assigned to
@hectmor and carries the label and milestone of the issue it belongs to.
