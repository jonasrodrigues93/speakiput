# Golden audio fixtures

Deterministic mono 16 kHz PCM WAV fixtures for the silence gate, phrase
splitter and auto-stop behavior. Regenerate them with:

```sh
cargo run -p speakiput-audio --example generate_golden_audio
```

Prompt-echo and repeated-filler hallucination defenses are covered in
`speakiput-asr` with text-level golden cases because their exact decoded audio
depends on the selected Whisper model.
