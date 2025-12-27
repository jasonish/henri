Review uncommitted changes.

* Look for correctness.
* Look for AI sloppiness such as code or comments that would never be written
  by a human.
* Are the changes as simple as possible? As maintainable as possible?
* Are Rust functions and types as private as possible? Prefer `pub(crate)` to
  `pub`.
* DO NOT COMMIT, this is a review only.

Follow any special instructions provided by the user in the following arguments (may be empty):
```
$ARGUMENTS
```
