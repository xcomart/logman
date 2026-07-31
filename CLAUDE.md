# CLAUDE.md

Guidance for Claude Code instances working in this repository.

## Branch and release flow

Work happens on `dev`; `main` only moves through a PR merged with a merge
commit (`gh pr merge --merge`), after the CI checks pass on all three
platforms. A release is published by pushing an annotated tag `vX.Y.Z`
pointing at the merge commit on `main` — `.github/workflows/release.yml`
builds the platform artifacts and creates the GitHub release from the tag.
Bump `[workspace.package] version` in `Cargo.toml` (and `Cargo.lock` via
`cargo update --workspace`) in its own `chore:` commit before tagging.

## Release tag annotations

The release page body is generated from the annotated tag's *body*
(`release.yml` reads `%(contents:body)`, falling back to the subject only
when the body is empty), so the annotation has to be written as the release
notes the user will read:

- Subject line: `logman vX.Y.Z`.
- Body: at most one short lead-in sentence, then a **markdown bullet list,
  one bullet per user-visible change**. Bullet lists, not prose paragraphs —
  a paragraph renders as an unreadable wall on the release page.
- Describe changes from the user's point of view ("Deleting asks first"),
  not the implementation's, and wrap lines at ~72 characters.

To fold a late change into an already-published release: merge it to `main`,
delete the remote tag (`git push origin :refs/tags/vX.Y.Z`), recreate the
annotated tag on the new merge commit and push it. GitHub turns the old
release into a draft when its tag disappears; the re-triggered workflow
republishes it with the rebuilt artifacts, and any leftover draft should be
checked for and removed afterwards. One caveat, learned the hard way: the
release action replaces the *assets* of a pre-existing release but keeps its
old *body*, so after a re-release the notes must be pushed by hand with
`gh release edit vX.Y.Z --notes-file <file>` (tag-annotation bullets plus
the "What's Changed" PR links).
