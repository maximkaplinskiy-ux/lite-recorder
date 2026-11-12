//! Deterministic endpoint selection. Silence never triggers a fallback to a raw mic.

pub struct Candidate {
    pub id: String,
    pub name: String,
}

fn contains_any(name: &str, terms: &[&str]) -> bool {
    terms.iter().any(|term| name.contains(term))
}

fn voice_processor(name: &str) -> bool {
    contains_any(
        name,
        &[
            "voicemod",
            "clownfish",
            "morphvox",
            "voice.ai",
            "magicmic",
            "voicewave",
            "w-okada",
            "wokada",
            "rvc",
            "altered",
            "steelseries sonar - microphone",
            "nvidia broadcast",
            "nvidia rtx voice",
            "krisp",
            "virtual microphone",
            "virtual mic",
            "виртуальный микрофон",
            "voice changer",
        ],
    )
}

fn mic_score(
    candidate: &Candidate,
    default: Option<&str>,
    communications: Option<&str>,
) -> Option<u32> {
    let name = candidate.name.to_lowercase();
    // Loopbacks, line inputs and mixing buses are not microphone signals.
    if contains_any(
        &name,
        &[
            "stereo mix",
            "стерео микшер",
            "стереомикшер",
            "what u hear",
            "loopback",
            "line in",
            "line-in",
            "линейный вход",
            "xsplit",
            "broadcaster",
            "cable output",
            "cable-out",
            "voicemeeter",
            "monitor of",
        ],
    ) {
        return None;
    }
    let is_mic = contains_any(
        &name,
        &[
            "microphone",
            "микрофон",
            "headset",
            "гарнитур",
            " mic",
            "mic ",
        ],
    ) || name == "mic";
    let preferred = candidate.id == default.unwrap_or("");
    let chat = candidate.id == communications.unwrap_or("");
    let tier = if voice_processor(&name) {
        400
    } else if is_mic {
        300
    } else if contains_any(&name, &["virtual", "виртуальн", "mixer", "микшер"]) {
        return None;
    } else if chat || preferred {
        100
    } else {
        return None;
    };
    Some(tier + if chat { 20 } else { 0 } + if preferred { 10 } else { 0 })
}

pub fn choose_microphone(
    candidates: &[Candidate],
    default: Option<&str>,
    communications: Option<&str>,
) -> Option<usize> {
    // Keep enumeration order on ties, independent of signal level or background noise.
    candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| mic_score(c, default, communications).map(|s| (i, s)))
        .fold(None, |best, candidate| match best {
            Some((_, score)) if score >= candidate.1 => best,
            _ => Some(candidate),
        })
        .map(|(i, _)| i)
}

pub fn playback_is_router(name: &str) -> bool {
    let name = name.to_lowercase();
    voice_processor(&name)
        || contains_any(
            &name,
            &[
                "dummy output",
                "xsplit",
                "voicemeeter",
                "vb-audio",
                "cable input",
                "cable-input",
                "virtual cable",
                "sonar - microphone",
            ],
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidates(names: &[&str]) -> Vec<Candidate> {
        names
            .iter()
            .enumerate()
            .map(|(i, name)| Candidate {
                id: i.to_string(),
                name: name.to_string(),
            })
            .collect()
    }
    #[test]
    fn processed_mic_wins_over_default_hardware_and_broadcast_bus() {
        let c = candidates(&[
            "Микрофон (fifine Headset)",
            "XSplit Audio (Broadcaster)",
            "Микрофон (Voicemod)",
        ]);
        assert_eq!(choose_microphone(&c, Some("0"), Some("0")), Some(2));
    }
    #[test]
    fn default_resolves_ties_within_same_kind() {
        let c = candidates(&[
            "Microphone (MagicMic)",
            "Microphone (Voicemod)",
            "Microphone USB",
        ]);
        assert_eq!(choose_microphone(&c, Some("2"), Some("1")), Some(1));
        let c = candidates(&["Микрофон веб-камеры", "Микрофон гарнитуры"]);
        assert_eq!(choose_microphone(&c, Some("1"), None), Some(1));
    }
    #[test]
    fn ignores_mixing_buses_even_when_default() {
        let c = candidates(&[
            "Stereo Mix",
            "XSplit Audio (Broadcaster)",
            "CABLE Output (VB-Audio Virtual Cable)",
            "Line In",
            "Voicemeeter Output",
        ]);
        assert_eq!(choose_microphone(&c, Some("0"), Some("1")), None);
    }
    #[test]
    fn unknown_default_supported_and_physical_mic_preferred() {
        let c = candidates(&["USB Audio Device"]);
        assert_eq!(choose_microphone(&c, Some("0"), None), Some(0));
        let c = candidates(&["USB Audio Device", "Microphone (USB)"]);
        assert_eq!(choose_microphone(&c, Some("0"), None), Some(1));
        assert_eq!(choose_microphone(&[], None, None), None);
    }
    #[test]
    fn excludes_voice_router_outputs_but_keeps_headphones() {
        for name in [
            "Dummy Output (Voicemod)",
            "Динамики (XSplit Stream Audio Renderer)",
            "CABLE Input (VB-Audio Virtual Cable)",
        ] {
            assert!(playback_is_router(name));
        }
        assert!(!playback_is_router("Динамики (fifine Headset)"));
        assert!(!playback_is_router(
            "Динамики (High Definition Audio Device)"
        ));
    }
}
