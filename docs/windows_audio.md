# Windows audio hosts

Microphone and Speaker each have their own Host and Device selectors. Disable
Auto select on the corresponding side before choosing ASIO. Auto select uses
the Windows WASAPI default microphone or playback device.

Speaker Device selects an **audio source for receiving STT**, not a destination
for playing sound. WASAPI lists recording inputs (including `VBMatrix Out`),
alongside playback loopback sources. Selecting a recording input captures it
directly; selecting a playback device captures its playback through loopback. ASIO Speaker
devices capture the selected driver's **inputs**; route the desired playback
mix to those inputs in VB-Matrix or your interface's loopback mixer. ASIO does
not provide the Windows playback loopback used by WASAPI. Input channels are
mixed to mono for transcription.

WASAPI playback sources appear only with `[Loopback]` (for example `VBMatrix In 1
[Loopback]`). Recording sources such as `VBMatrix Out 1` appear without that suffix.
After selecting ASIO, use
**ASIO Control Panel** under Microphone or Speaker to open the selected driver's
native settings. A different driver cannot be opened while the current one is
capturing or has its control panel open.
VB-Matrix VASIO drivers can report that no ASIO panel is provided; in that case
the button shows the existing VB-Audio Matrix window or starts its installed app.

Registered 64-bit ASIO drivers appear in both device lists. Listing devices does
not load drivers or start recording. An unavailable or output-only driver
returns an error when capture starts. There is no automatic host substitution
after a stream fails.

Both captures can share one ASIO driver instance. ASIO allows one loaded driver
per process, so stop both captures before changing to a different ASIO driver.
Selecting the same driver on both sides captures the same set of input channels.

## Building

The Windows build enables CPAL's ASIO backend and requires x64 Visual Studio
C++ build tools, libclang, and the ASIO SDK. The npm build/dev commands initialize
Visual Studio through `vswhere` and locate libclang in the local native cache or
the standard LLVM installation. For direct Cargo commands, use Developer
PowerShell and set `LIBCLANG_PATH` to the folder containing `libclang.dll`.
Optionally set `CPAL_ASIO_DIR` to an existing SDK;
otherwise `asio-sys` downloads it into the temporary directory. The SDK's own
license applies to its files; check its distribution terms before publishing an
ASIO-enabled build.

```powershell
$env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin'
npm run dev
```

The optional driver-format test opens only the named driver and queries its
format; it does not start a recording stream:

```powershell
$env:VRCT_ASIO_PROBE_DRIVER = 'VB-Matrix VASIO-8'
cargo test --manifest-path src-tauri/Cargo.toml -p vrct-core --lib installed_asio_driver -- --ignored --nocapture
```
