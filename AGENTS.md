# AGENTS.md

This repository is inphish (internal codename: inphzugzwang), a UCI chess engine in Rust.
The full specification is in BRIEF.md. Read it before making any change and follow it over your defaults.

Hard rules, always:
- No emojis anywhere.
- Comments explain why, never what. No decorative comments.
- No placeholders, stubs, todo!() or dead code in finished work.
- No chess libraries. All chess logic is written in this repository.
- cargo fmt --check and cargo clippy --all-targets --all-features -- -D warnings must pass.
- This is an engine, not an app: UCI over stdin/stdout only, no GUI of any kind.
- Work only on the current phase. Do not scaffold future phases.
