fn main() {
    // LiveKit/libwebrtc on macOS ships ObjC *category* methods (e.g. NSString's
    // `stringForAbslStringView:` and the `RTCDefaultVideoEncoderFactory` codec
    // enumeration) inside static libraries. ObjC categories in a static lib are
    // only registered into the final binary if the linker force-loads all ObjC
    // symbols via `-ObjC`. `webrtc-sys` emits this flag, but a build script's
    // `cargo:rustc-link-arg` does NOT propagate to a *downstream* binary's link
    // step — so this consumer binary must add it itself. Without it the LiveKit
    // egress transport crashes at `Room::connect` while building the WebRTC
    // `PeerConnectionFactory`, with an "unrecognized selector" NSException in
    // `+[RTCVideoEncoderVP9 scalabilityModes]` (livekit/rust-sdks#795).
    //
    // Scoped to macOS + the `livekit` feature so non-macOS / non-livekit builds
    // (incl. the musl cross-build) are untouched.
    let livekit = std::env::var("CARGO_FEATURE_LIVEKIT").is_ok();
    let macos = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    if livekit && macos {
        println!("cargo:rustc-link-arg=-ObjC");
    }
}
