# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# Key Information

NEVER guess or make assumptions about the codebase or external dependencies. Always ask yourself if you are 100% confident in something, and if not ask for clarification or investigate further before proceeding.

## Development Commands

- **Build**: `cargo build`
- **Run**: `cargo run`
- **Check**: `cargo check`
- **Format**: `cargo fmt`
- **Lint**: `cargo clippy`

## Memories and Development Notes

- Whenever you make a change that affects the project structure or just something that you find might be important to remember, please write it to your memory file yourself.
- Always take your time to write clean, well designed and organised code. Do not repeat yourself, but also don't break things up to a ridiculous extent before it is needed.
- You have full access to the bevy repository (in ../bevy) if you need to check anything about bevy itself.
- Instead of running the program as it's a visual app you can't interact with, just ask me. You can however run tests and check if things compile.