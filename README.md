# LiteRecorder

A lightweight command-line screen recorder for **Windows 10 (2004+) and Windows 11, x64**, written in Rust.

Records the primary display, system audio and a microphone to MP4. Includes timed recording and a disk-backed replay buffer for saving the last few minutes. Uses native Windows media APIs without an external FFmpeg executable.

[Подробная инструкция на русском](README.ru.md)

## Features

- Primary-display capture with the cursor, at native resolution, up to 30 fps.
- H.264 video (6 Mbps) and stereo AAC audio (48 kHz, 160 kbps).
- System audio and one automatically selected microphone mixed into a single audio track.
- Preference for supported voice-processing microphones; routing and loopback devices are filtered to reduce duplicate audio.
- Background recording, optional timers, configurable output directory and device diagnostics.
- Disk-backed replay buffer: save a recent segment while buffering continues.
- Native Windows Graphics Capture, WASAPI and Windows encoding APIs. Hardware acceleration depends on driver support.

## Download

Get the Windows x64 ZIP from [Releases](https://github.com/maximkaplinskiy-ux/lite-recorder/releases). Extract it and open PowerShell in the extracted folder.

The application currently displays its built-in help and status messages in Russian. The commands below use English names.

## Quick start

```powershell
.\lite-recorder.exe start
.\lite-recorder.exe stop

# Record for 30 seconds, or choose a new output file
.\lite-recorder.exe start --duration 30s
.\lite-recorder.exe start --duration 5m --output "D:\Recordings\demo.mp4"

# Save the preferred output folder for future recordings
.\lite-recorder.exe set-dir "D:\Recordings"
.\lite-recorder.exe status
.\lite-recorder.exe devices
```

`start` launches a separate background process, so the launching terminal can be closed. Use `stop` to finalize the MP4 before shutting down. Existing output files are not overwritten.

### Replay buffer

```powershell
.\lite-recorder.exe replay-start 5
.\lite-recorder.exe replay-save
.\lite-recorder.exe replay-stop
```

Here `5` means five minutes. Explicit durations such as `30s`, `5m` and `1h` are also supported. `replay-save` exports the available recent footage to a new MP4 and keeps buffering. Recording and replay buffering cannot run simultaneously.

Compressed segments are stored on disk, not entirely in RAM. Replay exports can take time and require additional disk space. See the Russian documentation for detailed limits and cache behavior.

## Build and verify

Install Rust with the MSVC toolchain and Visual Studio Build Tools with the Windows SDK, then run:

```powershell
cargo build --release --locked
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
```

The binary is written to `target\release\lite-recorder.exe`.

To build a distribution ZIP with SHA-256 checksums and third-party notices:

```powershell
.\tools\package.ps1
```

`tools/verify_media.py` is an optional development utility using PyAV. Python and PyAV are not required to run the recorder.

## Implementation

| Module | Responsibility |
| --- | --- |
| `main.rs` | Command-line commands and background-process lifecycle |
| `recording.rs`, `encoder.rs` | Capture and MP4 encoding |
| `audio.rs`, `audio_selection.rs` | Audio capture, mixing and device selection |
| `replay.rs` | Rolling segments and replay export |
| `config.rs`, `system.rs` | Persistent settings and Windows integration |

Main dependencies: `windows-capture`, `wasapi` and `windows`.

## Limitations

- Windows only; records the primary display rather than an arbitrary window.
- No graphical interface; built-in instructions and messages are currently in Russian.
- Audio devices are selected at startup. Restart recording after device or display changes.
- No echo cancellation. Monitoring a microphone through recorded speakers may duplicate voice audio.
- Protected content, exclusive audio and desktop/session changes may prevent capture.
- An interrupted or forcibly terminated recording may leave an unplayable MP4. Always use the stop command.
- Windows N editions may require the Media Feature Pack.

See [README.ru.md](README.ru.md) for full operating instructions, microphone-selection rules and troubleshooting.
