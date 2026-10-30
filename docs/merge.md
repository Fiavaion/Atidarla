# Diff and merge (FR-F2, FR-F3)

Project folders are text with stable node IDs (PRD §5.5, A11), so two states of a
project can be compared and merged by ID rather than by line.

## Diff

```sh
atidarla diff old.atidarla new.atidarla
atidarla diff git:HEAD~1 myshow.atidarla     # the folder at a Git revision
atidarla diff git:main git:feature --project myshow.atidarla
```

The report lists, per network, nodes added (`+`), removed (`-`) and changed (`~`):
renamed, moved, retyped, each parameter (value, expression or binding: `old → new`,
`default` when reset) and each input wire (`n3.out → n5.out`, `(none)` when
unconnected). Comment boxes, project settings (name, description, resolution) and
the sections Packs, mappings, timeline, `[ai]` and models are listed at a coarser
grain. The order is always ID or name order, so the same two states give the same
report.

In the app: **View > Diff** compares the open project with a folder or a Git
revision (`git:HEAD`, `git:main`). Its canvas shows added nodes green with `+`,
removed nodes red, dashed and faded with `−`, changed nodes amber with `~`, and
nodes that only moved with `↔`; the sign, the outline style and the list beside the
canvas carry the meaning too, not the colour alone. Click a node for its table of
changes.

## Three-way merge

```sh
atidarla merge base.atidarla ours.atidarla theirs.atidarla --out merged.atidarla
atidarla merge git:main~3 git:main git:feature --project myshow.atidarla --out merged.atidarla
```

Every node field, parameter, input wire, comment box, network and project section is
merged on its own: a change on one side is taken, the same change on both sides is
taken once, different changes on both sides are a **conflict**:

| Kind | When |
|---|---|
| `param` | the same parameter set differently (value, expression or binding) |
| `input` | the same input rewired differently, or rewired on one side and unwired on the other |
| `name` | a node renamed differently on both sides, or two nodes now share a name |
| `type` | a node's operator type changed differently |
| `edit-delete` | a node or network edited on one side and deleted on the other |
| `add-add` | both sides added the same ID with different content (IDs come from a per-network counter, so two branches that each add a node usually do this) |
| `dangling` | a wire or binding refers to a node the merge removed |
| `comment` | a comment box changed differently |
| `project` | a project setting, Pack, mappings, timeline, `[ai]` or model changed differently |

**Rule, not a conflict:** when both sides moved the same node to different places,
the merge keeps **ours'** position and reports it (`auto` lines). Positions do not
change what a patch does.

Exit status: 0 clean, 1 conflicts, 2 an error. With `--out`, a clean merge is written
in canonical form; a merge with conflicts is written with `# atidarla-conflict`
blocks (below) when all its conflicts are inside networks, and not at all otherwise.
An unresolved merge never writes a normal project.

In the app: **View > Merge** runs a three-way merge of three folders or revisions
(ours defaults to the open project) or opens a conflicted project. Each conflict
shows base, ours and theirs; pick one or type a value. **Save merged** stays disabled
until every conflict is resolved. Merging into the open project is one undo step for
the patch (settings such as mappings and the timeline are not in the undo history);
otherwise the result is written to the folder.

## Git merge driver

Git's line merge sees two edits to neighbouring lines of one node as a conflict, and
cannot tell an edit from a delete. Let Atidarla merge network files instead.

`.gitattributes` in the repository:

```gitattributes
*.atidarla/networks/*.toml merge=atidarla
*.atidarla/packs/*/network.toml merge=atidarla
```

Once per clone (or `--global`):

```sh
git config merge.atidarla.name "Atidarla network merge"
git config merge.atidarla.driver "atidarla merge-file %O %A %B %P"
```

`atidarla merge-file BASE OURS THEIRS [PATH]` writes the merged network over OURS and
exits 0 when clean. With conflicts it exits 1 (Git stops the merge) and the file is
the merge with ours at each conflict, followed by a block of comments:

```toml
# atidarla-conflict: 1 unresolved; the network above is a placeholder (ours). ...
# [[conflict]]
# kind = "param"
# slot = "node/1/param/scale"
# label = "main / n1 noise1: param scale"
# base = "{\"value\":2.0}"
# ours = "{\"value\":3.0}"
# theirs = "{\"value\":9.0}"
```

A project with such a block does not open normally (so the placeholder cannot be
saved by accident). Open it with **View > Merge > Open conflicted**, resolve, **Save
merged** (canonical files, block removed), then `git add` and `git commit`. Without
the app, edit the network above the block and delete the block.
