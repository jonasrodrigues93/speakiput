# whisrs reference and reuse map

## Pinned reference

- Repository: <https://github.com/y0sif/whisrs>
- Local reference during initial planning:
  `/home/jonas/Projects/opensource/whisrs`
- Reviewed commit: `28139bd8c4ff17e8d0fd156a0d903a7baa423d48`
- License: MIT. Preserve the whisrs copyright/license notice in copied or
  substantially derived files and record their origin in commit messages.

Pinning records what was reviewed; later upstream fixes should be ported
deliberately rather than copied from an unknown moving revision.

## Copy first, then rename/test

These are already isolated Rust crates with limited platform coupling and are
the best candidates for direct reuse:

| whisrs source | speakiput destination | action |
|---|---|---|
| `crates/audio-silence-gate` | `crates/speakiput-audio` or retained crate | Copy with tests; preserve API initially |
| `crates/asr-dedup` | `crates/speakiput-asr` support | Copy with tests |
| `crates/filler-remove` | `crates/speakiput-core` support | Copy with tests |
| `crates/prompt-echo` | `crates/speakiput-asr` support | Copy with tests |
| `src/audio/wav.rs` | `crates/speakiput-audio` | Copy/adapt |
| `src/transcription/phrase_split.rs` | `crates/speakiput-audio` | Copy with golden fixtures |

Avoid redesigning these during the first port. Establish parity first and
refactor only after their original tests pass in the new workspace.

## Adapt behind new interfaces

| whisrs source | knowledge to retain | required change |
|---|---|---|
| `src/audio/capture.rs` | CPAL capture and 16 kHz pipeline | Implement the new audio source trait; remove daemon globals |
| `src/transcription/mod.rs` | batch/streaming ASR trait | Separate domain events from Tokio channels |
| `src/transcription/local_whisper.rs` | phrase decoding, no-context behavior, silence/hallucination protections | Move into ASR crate and report structured progress/errors |
| `src/llm.rs` | OpenAI-compatible cleanup and output sanitation | Separate provider client from dictation policy |
| `src/state.rs` | explicit validated transitions | Extend with post-processing/injection and session IDs |
| `src/history.rs` | simple append/read behavior | Hide behind repository trait; retain raw and processed text |
| `src/config/types.rs` | defaults and validation lessons | Replace Linux-shaped config with versioned cross-platform settings |
| `src/daemon/pipeline.rs` | orchestration, fallbacks and history semantics | Split into core use cases with injected ports |
| `src/daemon/injection.rs` | safe insertion policy | Keep policy in core; primitives in per-OS adapters |

## Linux-only reuse

The following should inform or seed the Linux adapter, never the portable core:

- `crates/xkb-type`;
- `src/hotkey`;
- `src/window`;
- `src/overlay`;
- `src/tray`;
- `src/service.rs` and `contrib/whisrs.service`.

## Reference, do not copy as architecture

- `src/ipc.rs`: retain length-prefixed JSON and the 1 MiB safety limit, but the
  current Unix-only one-request/one-response protocol lacks negotiation,
  subscriptions, event sequencing and Windows transport support. Use the
  speakiput v1 contract instead.
- `src/ui`: reuse workflows, labels and validation lessons where useful, but
  build the new interface in Slint rather than carrying GTK/libadwaita types.
- daemon-wide context structs: do not reproduce a large shared context. Inject
  narrow traits into core use cases.

## Porting rule

For every copied module:

1. copy its tests and MIT notice with it;
2. make those tests pass without behavioral redesign;
3. wrap it behind a speakiput interface;
4. add a source comment naming the whisrs commit;
5. only then refactor names or ownership.
