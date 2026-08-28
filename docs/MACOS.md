# macOS support

The macOS adapter is selected at compile time and keeps Apple APIs out of the
portable crates. It uses:

- CPAL's CoreAudio host for microphone capture;
- `rdev` to observe the configured global shortcut;
- `enigo` for keyboard text injection;
- `arboard` for the system clipboard;
- System Events through `/usr/bin/osascript` to query and restore the focused
  application;
- the macOS Keychain through the existing `keyring` repository.

## Build prerequisites

The following are required on a Mac:

1. macOS with Xcode Command Line Tools installed. The tools provide `clang`,
   the Apple SDK and the frameworks linked by CPAL, rdev and enigo.
2. A Rust toolchain with the host target installed. The supported builds are
   `x86_64-apple-darwin` and `aarch64-apple-darwin`.
3. Microphone permission for the GUI/daemon process on first recording.
4. Accessibility permission for the daemon binary. It is needed by the
   global shortcut listener, System Events focus restoration and keyboard
   injection. Add the exact installed binary under System Settings > Privacy &
   Security > Accessibility.

The application does not request these permissions from a portable core. The
first shortcut registration reports a permission error and the macOS adapter
describes the required System Settings location.

## Build and run

```sh
cargo build --release -p speakiput -p speakiputd \
  --features speakiputd/native
target/release/speakiputd
target/release/speakiput
```

Metal acceleration is optional and only applies to local Whisper builds:

```sh
cargo build --release -p speakiputd \
  --features speakiputd/metal
```

The daemon stores settings and history below
`~/Library/Application Support/speakiput`. Its Unix socket is below
`~/Library/Caches/speakiput`, so the GUI and daemon use the same location
without relying on Linux `XDG_*` variables.

## Validation without a Mac

No microphone, display server or Accessibility permission is needed for the
portable tests. The Linux CI job validates the contract/core behavior, while
the macOS CI job runs the complete workspace tests and compiles the native
daemon on a GitHub-hosted Mac. Runtime checks for microphone permission,
shortcut delivery, focus restoration, text insertion and Metal remain
hardware/OS-session checks and must be performed before release.
