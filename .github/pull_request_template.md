## What changes

<!-- What a person using the app notices, or what changes in the code if nothing shows. -->

## Why

<!-- The problem this solves. Link the issue: "Closes #123". -->

## How you checked it

<!-- Commands you ran, what you did in the app, screenshots for anything on screen. -->

## Where it came from

<!-- Written by hand, with an AI tool (which one), or taken from another project (which, and its licence). -->

## Checklist

- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `scripts/update-po.sh --check` passes, run after the last edit
- [ ] `scripts/a11y-names.sh` passes, if the UI changed
- [ ] New user-facing strings go through `translate` and have a `po/pt_PT.po` translation
- [ ] New domain words are in `CONTEXT.md`
- [ ] No tokens, real addresses, or real mail in code, tests, or screenshots
- [ ] The change is mine to give under GPL-3.0-or-later
