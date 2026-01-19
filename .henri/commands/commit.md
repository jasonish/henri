---
---

Create a git commit for the current changes.

Respect any of the users custom instructions: $ARGUMENTS

* Make sure we are "just check" clean.
* If not "just check" clean, then run "just fix".
* If still not "just check" clean, resolve any issues.
* Commit all changes, not just those relevant to the work in the
  context.
* Make sure CHANGELOG.md is up to date. If this was a simple commit
  like a version bump or purely a dependency update, the CHANGELOG.md
  update can be skipped.

The commit should follow any other commit guidelines you may have been given.

Unless asked not to include extra attributions, end the commit message with:

Co-authored-by: Henri 🐕 <henri@codemonkey.net>
