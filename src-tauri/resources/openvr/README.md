# OpenVR client resource

`openvr_api.dll` is Valve's unmodified x64 OpenVR client from SDK 2.15.6,
commit `0924064316de3effbcd1acf1e309182a2deb1c05`:

- [SDK binary](https://github.com/ValveSoftware/openvr/blob/0924064316de3effbcd1acf1e309182a2deb1c05/bin/win64/openvr_api.dll)
- [C API / function-table ABI](https://github.com/ValveSoftware/openvr/blob/0924064316de3effbcd1acf1e309182a2deb1c05/headers/openvr_capi.h)
- [Redistribution license](https://github.com/ValveSoftware/openvr/blob/0924064316de3effbcd1acf1e309182a2deb1c05/LICENSE)

SHA-256: `bab8ac6ef64e68a9ca53315b0014d131088584b2efdfa6db511d67ec03cfcb4a`.

Tauri maps this DLL to `openvr_api.dll` beside the Windows executable and maps
`LICENSE` to `licenses/openvr-LICENSE`. Debug builds can use this source resource
when Tauri has not staged it. There is no PATH/current-directory DLL search.
The Rust adapter uses the `FnTable:IVRApplications_008` prefix through
`GetApplicationPropertyString`. Initialization uses `VRApplication_Background`;
the adapter does not start SteamVR. Other Windows architectures fall back to
manual copying; Linux/macOS keep the existing sidecar clipboard path.

Tests load the DLL and check its exports without initializing OpenVR. Session
lifetime, target selection and clipboard output are tested with injected fakes.
Tests do not prove an installed NSIS package or interaction with a live VR game.

The custom NSIS installer extracts the release ZIP. `utils/zip.py` includes the
staged DLL and `licenses` directory by default so the ZIP uses the same layout.
