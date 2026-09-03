# Project instructions

## Reference implementation

Before implementing audio capture, WAV encoding, silence handling,
transcription, LLM cleanup, history, text injection or daemon lifecycle, inspect
the existing whisrs implementation instead of starting from zero:

- upstream: `https://github.com/y0sif/whisrs`
- initially reviewed commit: `28139bd8c4ff17e8d0fd156a0d903a7baa423d48`
- detailed reuse map: `docs/WHISRS_REFERENCE.md`

Copy proven, portable modules where the reuse map permits it. Preserve the MIT
license notice and identify the source commit in substantially derived files.
Port behavior and tests before refactoring.

## Architecture invariants

- The application is native Rust with a Slint GUI; do not introduce a web UI.
- Keep GUI, backend and OS integration in separate crates.
- The GUI communicates with backend behavior only through `BackendClient` and
  the versioned contract under `contracts/`.
- Keep `speakiput-contract` free of Slint, audio, networking and OS-specific
  dependencies.
- Keep portable core crates free of Wayland, X11, systemd, Win32 and macOS APIs.
- Use explicit start/stop commands and backend-owned state; do not add a GUI-
  owned toggle state.
- Update protocol docs, schemas, fixtures, Rust DTOs, fake backend and
  compatibility tests together whenever the contract changes.

## Implementation order

Establish the executable contract and in-memory fake before implementing GUI or
backend production behavior.
