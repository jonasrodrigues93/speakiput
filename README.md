# speakiput

Native desktop dictation for Linux, implemented in Rust with a Slint GUI and
an independently managed background service.

## Build and run

The production daemon enables microphone capture and local Whisper explicitly:

```sh
cargo build --release -p speakiput -p speakiputd --features speakiputd/native
target/release/speakiputd
target/release/speakiput
```

On Linux systems with a working Vulkan driver, build the daemon with GPU
acceleration using `--features speakiputd/vulkan` instead of
`--features speakiputd/native`.

Choose the microphone, Whisper model file, overlay, global shortcut and text
insertion behavior in Settings. Remote OpenAI-compatible cleanup credentials
are stored through the desktop keyring and are never written to settings.

Direct insertion uses a native virtual keyboard on Wayland and X11, with
`wtype`, `ydotool` and `xdotool` retained as fallbacks. The system package
installs the udev rule required by the `uinput` fallback. Clipboard mode uses
`wl-copy`, `xclip` or `xsel`.

After installing the system package directly from this checkout, reload the
new device rule once:

```sh
sudo udevadm control --reload-rules
sudo udevadm trigger
```

## Linux packaging

After the release build, a staged package can be assembled without modifying
the host:

```sh
DESTDIR=/tmp/speakiput-package PREFIX=/usr packaging/linux/install.sh
```

The files under `packaging/linux` include the desktop entry, tray autostart and
systemd user service.

For an installation limited to the current user, without `sudo`:

```sh
packaging/linux/install-user.sh
systemctl --user enable --now speakiputd.service
```

## Benchmarks

`cargo bench -p speakiputd --bench pipeline_benchmark` reports silence-gate,
WAV encoding and resident-memory measurements. To include local Whisper
model-load latency, run the benchmark with `--features native` and provide a
model through `SPEAKIPUT_MODEL_PATH`.

## Current developer commands

```sh
cargo test --workspace --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo run -p speakiput-testing --bin speakiput-conformance --offline
```

The canonical GUI/backend protocol is documented in `contracts/v1`. The
conformance command exercises the same in-memory transport used by deterministic
GUI tests.

This repository intentionally starts from the proven behavior of
[`whisrs`](https://github.com/y0sif/whisrs) instead of rebuilding the audio and
transcription pipeline from scratch.

Planning artifacts:

- [Implementation plan](IMPLEMENTATION_PLAN.md)
- [Architecture](docs/ARCHITECTURE.md)
- [whisrs reuse map](docs/WHISRS_REFERENCE.md)
- [GUI/backend contract](contracts/v1/README.md)

The implementation follows the phase boundaries and definition of done in the
implementation plan.
