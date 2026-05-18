# SMAF loop patch notes

This package keeps the existing `parse_smaf()` API intact and adds finite repeat helpers in `smaf_player`:

- `smaf_duration(&[(usize, SmafEvent)]) -> usize`
- `repeat_smaf_events(&[(usize, SmafEvent)], play_count: usize) -> Vec<(usize, SmafEvent)>`
- `parse_smaf_repeated(raw: &[u8], play_count: usize) -> Vec<(usize, SmafEvent)>`

`play_count` is the total number of plays. `1` means play once, `2` means play twice, and `0` returns only an immediate `SmafEvent::End`.

The repeat helper removes intermediate `SmafEvent::End` markers and emits only one final `End` marker after the last pass. This prevents a higher-level player from treating the first loop boundary as final playback completion.

`smaf_cli` now supports:

```bash
cargo run -p smaf_cli -- --repeat 2 test_data/midi.mmf
cargo run -p smaf_cli -- --loop test_data/midi.mmf
```

For WIE integration, do not materialize an infinite event stream. For BGM/infinite loop playback, keep the parsed event list and replay it from the async playback task until the corresponding stop flag is set. Use `parse_smaf_repeated()` only for finite loop counts.

I could not run `cargo test` in the sandbox because Rust/Cargo is not installed here. Please run it locally after unpacking:

```bash
cargo test
```
