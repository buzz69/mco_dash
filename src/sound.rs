//! Sons de notification synthétisés (aucun fichier audio) et leur lecture : winmm sous Windows,
//! PulseAudio (chargé à l'exécution, donc sans lib de dev) sous Linux / WSLg.
use serde::{Deserialize, Serialize};
use std::f32::consts::TAU;
use std::thread;

const RATE: u32 = 44_100;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Carillon,
    Bip,
    DoubleBip,
    Montant,
    Descendant,
    Alarme,
}

impl Tone {
    pub const ALL: [Tone; 6] = [Tone::Carillon, Tone::Bip, Tone::DoubleBip, Tone::Montant, Tone::Descendant, Tone::Alarme];

    pub fn label(self) -> &'static str {
        match self {
            Tone::Carillon => "Carillon",
            Tone::Bip => "Bip",
            Tone::DoubleBip => "Double bip",
            Tone::Montant => "Montant",
            Tone::Descendant => "Descendant",
            Tone::Alarme => "Alarme",
        }
    }

    /// (fréquence Hz — 0 = silence, durée ms)
    fn notes(self) -> &'static [(f32, u32)] {
        match self {
            Tone::Carillon => &[(880.0, 140), (1320.0, 320)],
            Tone::Bip => &[(1000.0, 140)],
            Tone::DoubleBip => &[(1000.0, 90), (0.0, 60), (1000.0, 90)],
            Tone::Montant => &[(660.0, 110), (880.0, 110), (1100.0, 200)],
            Tone::Descendant => &[(1100.0, 110), (880.0, 110), (660.0, 200)],
            Tone::Alarme => &[(880.0, 130), (660.0, 130), (880.0, 130), (660.0, 130)],
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sounds {
    pub muted: bool,
    /// 0 à 100.
    pub volume: u8,
    pub new_ticket: Tone,
    pub updated: Tone,
}

impl Default for Sounds {
    fn default() -> Self {
        Self { muted: false, volume: 60, new_ticket: Tone::Montant, updated: Tone::Carillon }
    }
}

/// PCM 16 bits mono ; enveloppe attaque/décroissance pour éviter les claquements.
fn synth(tones: &[Tone], volume: u8) -> Vec<i16> {
    let amp = f32::from(volume.min(100)) / 100.0 * 0.6 * f32::from(i16::MAX);
    let mut out = Vec::new();
    for (i, tone) in tones.iter().enumerate() {
        if i > 0 {
            out.resize(out.len() + RATE as usize * 150 / 1000, 0);
        }
        for &(freq, ms) in tone.notes() {
            let n = (RATE * ms / 1000) as usize;
            out.extend((0..n).map(|k| {
                let t = k as f32 / RATE as f32;
                let env = (k as f32 / 200.0).min(1.0) * (1.0 - k as f32 / n as f32).powf(0.7);
                (amp * env * (TAU * freq * t).sin()) as i16
            }));
        }
    }
    out
}

/// Joue les sons l'un après l'autre sans bloquer ; l'erreur éventuelle part dans `on_error`.
pub fn play(tones: &[Tone], volume: u8, on_error: impl FnOnce(String) + Send + 'static) {
    if tones.is_empty() || volume == 0 {
        return;
    }
    let samples = synth(tones, volume);
    thread::spawn(move || {
        if let Err(e) = output(&samples) {
            on_error(e);
        }
    });
}

#[cfg(windows)]
fn output(samples: &[i16]) -> Result<(), String> {
    #[link(name = "winmm")]
    unsafe extern "system" {
        fn PlaySoundW(sound: *const u8, module: *mut std::ffi::c_void, flags: u32) -> i32;
    }
    const SND_NODEFAULT: u32 = 0x2;
    const SND_MEMORY: u32 = 0x4;

    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + samples.len() * 2);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
    // SAFETY: `wav` reste valide pendant tout l'appel, synchrone (pas de SND_ASYNC).
    let ok = unsafe { PlaySoundW(wav.as_ptr(), std::ptr::null_mut(), SND_MEMORY | SND_NODEFAULT) };
    if ok == 0 { Err("lecture audio impossible (winmm)".into()) } else { Ok(()) }
}

#[cfg(not(windows))]
fn output(samples: &[i16]) -> Result<(), String> {
    use libloading::{Library, Symbol};
    use std::ffi::{c_char, c_int, c_void};

    #[repr(C)]
    struct SampleSpec {
        format: c_int,
        rate: u32,
        channels: u8,
    }
    const PA_STREAM_PLAYBACK: c_int = 1;
    const PA_SAMPLE_S16LE: c_int = 3;

    // SAFETY: signatures de pulse/simple.h ; la lib reste chargée tant que `lib` vit.
    unsafe {
        let lib = Library::new("libpulse-simple.so.0").map_err(|e| format!("PulseAudio introuvable : {e}"))?;
        let sym = |e: libloading::Error| format!("PulseAudio : {e}");
        let new: Symbol<
            unsafe extern "C" fn(*const c_char, *const c_char, c_int, *const c_char, *const c_char, *const SampleSpec, *const c_void, *const c_void, *mut c_int) -> *mut c_void,
        > = lib.get(b"pa_simple_new\0").map_err(sym)?;
        let write: Symbol<unsafe extern "C" fn(*mut c_void, *const c_void, usize, *mut c_int) -> c_int> = lib.get(b"pa_simple_write\0").map_err(sym)?;
        let drain: Symbol<unsafe extern "C" fn(*mut c_void, *mut c_int) -> c_int> = lib.get(b"pa_simple_drain\0").map_err(sym)?;
        let free: Symbol<unsafe extern "C" fn(*mut c_void)> = lib.get(b"pa_simple_free\0").map_err(sym)?;

        let spec = SampleSpec { format: PA_SAMPLE_S16LE, rate: RATE, channels: 1 };
        let mut err: c_int = 0;
        let stream = new(std::ptr::null(), c"MCO Dash".as_ptr(), PA_STREAM_PLAYBACK, std::ptr::null(), c"notification".as_ptr(), &spec, std::ptr::null(), std::ptr::null(), &mut err);
        if stream.is_null() {
            return Err(format!("sortie audio indisponible (PulseAudio, erreur {err})"));
        }
        let wrote = write(stream, samples.as_ptr().cast(), std::mem::size_of_val(samples), &mut err);
        if wrote >= 0 {
            drain(stream, &mut err);
        }
        free(stream);
        if wrote < 0 { Err(format!("écriture audio impossible (PulseAudio, erreur {err})")) } else { Ok(()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synth_length_follows_notes_and_volume_scales_amplitude() {
        let loud = synth(&[Tone::DoubleBip], 100);
        assert_eq!(loud.len(), (RATE * 90 / 1000 * 2 + RATE * 60 / 1000) as usize);
        let peak = |v: &[i16]| v.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak(&synth(&[Tone::DoubleBip], 30)) < peak(&loud) / 2);
        assert!(synth(&[Tone::Bip, Tone::Bip], 50).len() > 2 * synth(&[Tone::Bip], 50).len());
    }
}
