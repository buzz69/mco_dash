//! Chiffrement au repos des secrets de `settings.json`.
//! Windows : DPAPI (clé liée au compte Windows de l'utilisateur, rien à gérer). Ailleurs : pas de coffre
//! disponible partout (absent de WSL), le fichier reste en clair mais lisible par l'utilisateur seul.
use base64::{Engine, engine::general_purpose::STANDARD};

const PREFIX: &str = "dpapi:";

/// Valeur à écrire dans le fichier.
pub fn protect(plain: &str) -> String {
    if plain.is_empty() {
        return String::new();
    }
    imp::protect(plain.as_bytes()).map_or_else(|| plain.to_string(), |b| format!("{PREFIX}{}", STANDARD.encode(b)))
}

/// Valeur lue dans le fichier ; une valeur en clair (ancien format) passe telle quelle.
/// Vide si le secret chiffré est illisible (autre compte ou autre machine) : mieux vaut le ressaisir.
pub fn reveal(stored: &str) -> String {
    let Some(b64) = stored.strip_prefix(PREFIX) else { return stored.to_string() };
    STANDARD
        .decode(b64)
        .ok()
        .and_then(|b| imp::reveal(&b))
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}

pub fn is_protected(stored: &str) -> bool {
    stored.is_empty() || stored.starts_with(PREFIX)
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    #[repr(C)]
    struct Blob {
        len: u32,
        data: *mut u8,
    }

    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(input: *const Blob, descr: *const u16, entropy: *const Blob, reserved: *const c_void, prompt: *const c_void, flags: u32, out: *mut Blob) -> i32;
        fn CryptUnprotectData(input: *const Blob, descr: *mut *mut u16, entropy: *const Blob, reserved: *const c_void, prompt: *const c_void, flags: u32, out: *mut Blob) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(mem: *mut c_void) -> *mut c_void;
    }
    const UI_FORBIDDEN: u32 = 0x1;

    fn run(data: &[u8], encrypt: bool) -> Option<Vec<u8>> {
        let input = Blob { len: u32::try_from(data.len()).ok()?, data: data.as_ptr().cast_mut() };
        let mut out = Blob { len: 0, data: std::ptr::null_mut() };
        let none: *const c_void = std::ptr::null();
        // SAFETY: `input` pointe sur `data` pendant tout l'appel ; `out` est alloué par DPAPI et libéré par LocalFree après copie.
        unsafe {
            let ok = if encrypt {
                CryptProtectData(&input, std::ptr::null(), std::ptr::null(), none, none, UI_FORBIDDEN, &mut out)
            } else {
                CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), none, none, UI_FORBIDDEN, &mut out)
            };
            if ok == 0 || out.data.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(out.data, out.len as usize).to_vec();
            LocalFree(out.data.cast());
            Some(bytes)
        }
    }

    pub fn protect(plain: &[u8]) -> Option<Vec<u8>> {
        run(plain, true)
    }

    pub fn reveal(cipher: &[u8]) -> Option<Vec<u8>> {
        run(cipher, false)
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn protect(_: &[u8]) -> Option<Vec<u8>> {
        None
    }

    pub fn reveal(_: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_values_pass_through_and_unreadable_secrets_become_empty() {
        assert_eq!(reveal("abc"), "abc");
        assert_eq!(protect(""), "");
        assert!(is_protected("") && !is_protected("abc"));
        #[cfg(not(windows))]
        {
            assert_eq!(protect("abc"), "abc");
            assert_eq!(reveal("dpapi:YWJj"), "");
        }
    }
}
