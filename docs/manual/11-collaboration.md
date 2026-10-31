# 11. Collaboration: diff, merge and Git

Because a project is a folder of plain text with stable IDs, Atidarla can compare two versions
of a patch, and combine the work of two people, **by node** and not by line of text. Reference:
[`docs/merge.md`](../merge.md).

## Using Git with projects

[Git](https://git-scm.com) is a tool that records the history of a folder. Putting your
`.atidarla` project in a Git repository gives you:

- a history you can go back through;
- a way to share work with classmates;
- a safe place to try an idea on a **branch** and throw it away if it does not work.

A change to one parameter is a one-line change in the file, so history is easy to read. Do not
put model weights in a repository. Atidarla never writes them into a project.

## Diff: what changed?

```sh
atidarla diff old.atidarla new.atidarla
atidarla diff git:HEAD~1 myshow.atidarla      # the project as it was one commit ago
atidarla diff git:main git:feature --project myshow.atidarla
```

The report lists, for each network, nodes **added** (`+`), **removed** (`-`) and **changed**
(`~`): renamed, moved, retyped, each parameter (old value to new value) and each input wire. It
also lists changes to the project's settings, Packs, mappings, timeline, AI settings and models.

In the app choose **View, Diff** to compare the open project with a folder or a Git revision
(for example `git:HEAD`). The canvas shows:

| Change | Shown as |
|---|---|
| Added | green, with `+` |
| Removed | red, dashed and faded, with a minus mark |
| Changed | amber, with `~` |
| Only moved | marked with a double arrow |

The mark, the outline style and the list beside the canvas carry the meaning, not the colour
alone. Click a node to see its table of changes.

## Merge: combining two people's work

If you and a classmate both changed the same project, **merge** combines your work:

```sh
atidarla merge base.atidarla ours.atidarla theirs.atidarla --out merged.atidarla
atidarla merge git:main~3 git:main git:feature --project myshow.atidarla --out merged.atidarla
```

*Base* is the version you both started from. A change made by only one side is taken. The same
change by both is taken once. **Different changes to the same thing are a conflict.** The exit
status is 0 for a clean merge, 1 for conflicts and 2 for an error.

Conflict kinds include: the same parameter set differently, the same input wired differently,
a node renamed differently, a node edited on one side and deleted on the other, and a wire left
pointing at a node the merge removed. If you both move the same node to different places, the
merge keeps *your* position and tells you. Positions do not change what a patch does.

In the app choose **View, Merge**. Pick the three versions (yours defaults to the open project).
For each conflict you see base, yours and theirs. Choose one or type a value. **Save merged**
stays off until every conflict is resolved. Merging into the open project is one undo step for
the patch.

An unresolved merge never writes a normal project. If a merge has conflicts it writes a
placeholder that has `# atidarla-conflict` blocks. The project does not open normally in that
state. Open it with **View, Merge, Open conflicted**.

## Git merge driver

Git by itself sees two edits to neighbouring lines of one node as a conflict. To let Atidarla do
the merging inside Git, add this to `.gitattributes` in the repository:

```gitattributes
*.atidarla/networks/*.toml merge=atidarla
*.atidarla/packs/*/network.toml merge=atidarla
```

and run this once per clone (or with `--global`):

```sh
git config merge.atidarla.name "Atidarla network merge"
git config merge.atidarla.driver "atidarla merge-file %O %A %B %P"
```

After that, `git merge` merges network files by node. If there are conflicts, Git stops as
usual. Open the project with **View, Merge, Open conflicted**, resolve and **Save merged**,
then `git add` and `git commit`.

## Tips for working in a team

- Agree who edits which network or area, even with merging.
- Commit small and often. A message such as "slower tunnel" is more useful than "changes".
- Add a `.gitignore` entry for `.cache/` (the supervisor's log and heartbeat live there).
- Save before you diff or merge.
