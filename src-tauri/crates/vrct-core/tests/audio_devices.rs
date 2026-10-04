//! `audio::devices` naming, placeholders and saved-selection mapping, plus a
//! Windows WASAPI listing smoke test. The historical naming contract is frozen
//! at `16cb286c`; native test commands are in `fixtures/README.md`.
//! Actual device listings depend on the machine; the smoke test does not prove
//! parity with every historical Python installation.

use vrct_core::audio::devices::{
    loopback_name, Device, DeviceList, LOOPBACK_SUFFIX, NO_DEVICE, NO_HOST, WASAPI_HOST,
};

fn device(name: &str) -> Device {
    Device { name: name.to_string(), channels: 2, default_sample_rate: 48000 }
}

fn list(mics: &[&str], speakers: &[&str]) -> DeviceList {
    DeviceList {
        mics: mics.iter().map(|name| device(name)).collect(),
        default_mic: mics.first().map(|name| name.to_string()),
        speakers: speakers.iter().map(|name| device(&loopback_name(name))).collect(),
        default_speaker: speakers.first().map(|name| loopback_name(name)),
    }
}

#[test]
fn loopback_names_carry_the_suffix_python_shows() {
    assert_eq!(loopback_name("Headphones (3- BOMGE USB Audio Device)"), "Headphones (3- BOMGE USB Audio Device) [Loopback]");
    assert_eq!(LOOPBACK_SUFFIX, " [Loopback]");
}

#[test]
fn an_empty_list_shows_the_placeholders_the_ui_expects() {
    let empty = DeviceList::default();
    assert_eq!(empty.hosts(), vec![NO_HOST]);
    assert_eq!(empty.mic_names(), vec![NO_DEVICE]);
    assert_eq!(empty.speaker_names(), vec![NO_DEVICE]);
    assert!(empty.resolve_mic("anything").is_none());
}

#[test]
fn recording_inputs_and_playback_loopbacks_are_separate_stt_sources() {
    let mut devices = list(&[], &["VBMatrix In 1"]);
    devices.speakers.push(device("VBMatrix Out 1"));
    assert_eq!(devices.speaker_names(), vec!["VBMatrix In 1 [Loopback]", "VBMatrix Out 1"]);
    assert_eq!(devices.resolve_speaker("VBMatrix Out 1").unwrap().name, "VBMatrix Out 1");
    assert_eq!(devices.resolve_speaker("VBMatrix In 1").unwrap().name, "VBMatrix In 1 [Loopback]");
}

#[test]
fn there_is_one_host_when_something_is_plugged_in() {
    let devices = list(&["Microphone (UGREEN Camera)", "Line (Yamaha SYNCROOM Driver (WDM))"], &["Headphones (X)"]);
    assert_eq!(devices.hosts(), vec![WASAPI_HOST]);
    assert_eq!(devices.mic_names(), vec!["Microphone (UGREEN Camera)", "Line (Yamaha SYNCROOM Driver (WDM))"]);
    assert_eq!(devices.speaker_names(), vec!["Headphones (X) [Loopback]"]);
}

#[test]
fn a_saved_name_finds_its_device_exactly() {
    let devices = list(&["Microphone (A)", "Microphone (A) 2"], &["Speakers (B)"]);
    assert_eq!(devices.resolve_mic("Microphone (A)").unwrap().name, "Microphone (A)");
    assert_eq!(devices.resolve_speaker("Speakers (B) [Loopback]").unwrap().name, "Speakers (B) [Loopback]");
}

#[test]
fn a_name_cut_short_by_mme_finds_the_one_device_it_starts() {
    // MME keeps 31 characters: "VBMatrix Out 3 (VB-Audio Matrix" is all Python saved for this one.
    let devices = list(
        &["VBMatrix Out 3 (VB-Audio Matrix VAIO)", "VBMatrix Out 5 (VB-Audio Matrix VAIO)", "Microphone (WO Mic Device)"],
        &[],
    );
    let saved = "VBMatrix Out 3 (VB-Audio Matrix";
    assert_eq!(saved.chars().count(), 31);
    assert_eq!(devices.resolve_mic(saved).unwrap().name, "VBMatrix Out 3 (VB-Audio Matrix VAIO)");
}

#[test]
fn a_prefix_that_fits_several_devices_chooses_none() {
    let devices = list(&["VBMatrix Out 3 (VB-Audio Matrix VAIO)", "VBMatrix Out 5 (VB-Audio Matrix VAIO)"], &[]);
    assert!(devices.resolve_mic("VBMatrix Out").is_none());
}

#[test]
fn an_exact_name_beats_a_longer_name_that_starts_with_it() {
    let devices = list(&["Mic", "Mic (2)"], &[]);
    assert_eq!(devices.resolve_mic("Mic").unwrap().name, "Mic");
}

#[test]
fn unknown_and_empty_names_find_nothing() {
    let devices = list(&["Microphone (A)"], &["Speakers (B)"]);
    assert!(devices.resolve_mic("Microphone (Z)").is_none());
    assert!(devices.resolve_mic("(A)").is_none(), "the middle of a name is not a saved name");
    assert!(devices.resolve_mic("").is_none());
    assert!(devices.resolve_speaker(NO_DEVICE).is_none());
}

#[test]
fn a_playback_name_without_the_suffix_still_finds_its_loopback_twin() {
    let devices = list(&[], &["Speakers (B)"]);
    assert_eq!(devices.resolve_speaker("Speakers (B)").unwrap().name, "Speakers (B) [Loopback]");
}

#[cfg(windows)]
#[test]
fn the_wasapi_listing_is_well_formed() {
    let devices = vrct_core::audio::wasapi::list_devices().expect("listing devices");
    for speaker in &devices.speakers {
        if devices.mics.iter().any(|input| input == speaker) {
            continue;
        }
        assert!(speaker.name.ends_with(LOOPBACK_SUFFIX), "playback sources must only appear as loopback: {}", speaker.name);
        assert!(speaker.channels > 0 && speaker.default_sample_rate > 0, "{speaker:?}");
    }
    for mic in &devices.mics {
        assert!(devices.speakers.contains(mic), "recording input must also be selectable for receiving STT: {mic:?}");
        assert!(!mic.name.ends_with(LOOPBACK_SUFFIX), "{}", mic.name);
        assert!(mic.channels > 0 && mic.default_sample_rate > 0, "{mic:?}");
    }
    if let Some(default) = &devices.default_mic {
        assert!(devices.mics.iter().any(|mic| &mic.name == default), "default mic {default} is not in the list");
    }
    if let Some(default) = &devices.default_speaker {
        assert!(devices.speakers.iter().any(|speaker| &speaker.name == default), "default speaker {default} is not in the list");
    }
}
