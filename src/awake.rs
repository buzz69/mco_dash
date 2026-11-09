#[cfg(windows)]
pub use native::Awake;
#[cfg(not(windows))]
pub use via_process::Awake;

/// Windows : appel direct de SetThreadExecutionState depuis un thread dédié (l'état vit autant que le thread).
#[cfg(windows)]
mod native {
    use std::sync::mpsc;
    use std::thread;

    // ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED, comme un lecteur vidéo.
    const FLAGS: u32 = 0x8000_0000 | 0x0000_0001 | 0x0000_0002;
    const ES_CONTINUOUS: u32 = 0x8000_0000;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetThreadExecutionState(flags: u32) -> u32;
    }

    /// Garde le poste éveillé tant que la valeur existe.
    // L'émetteur n'est jamais lu : sa fermeture (drop) est le signal d'arrêt du thread.
    pub struct Awake(#[allow(dead_code)] mpsc::Sender<()>);

    impl Awake {
        pub fn start() -> Result<Self, String> {
            let (stop, stopped) = mpsc::channel::<()>();
            let (ready, started) = mpsc::channel::<bool>();
            thread::spawn(move || {
                // SAFETY: appel Win32 sans pointeur.
                let ok = unsafe { SetThreadExecutionState(FLAGS) } != 0;
                let _ = ready.send(ok);
                // Bloque jusqu'à la fermeture du canal (drop de Awake), puis relâche l'état.
                let _ = stopped.recv();
                unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
            });
            match started.recv() {
                Ok(true) => Ok(Self(stop)),
                _ => Err("SetThreadExecutionState a échoué".into()),
            }
        }

        pub fn failure(&mut self) -> Option<String> {
            None
        }
    }
}

#[cfg(not(windows))]
mod via_process {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;
    use std::{env, thread};
    /// Garde le poste éveillé tant que la valeur existe (SetThreadExecutionState sous Windows via WSL,
    /// inhibiteur systemd sous Linux). Le processus enfant lit son stdin : le fermer (drop, ou mort de
    /// l'appli) suffit à lever l'inhibition.
    pub struct Awake(Option<Child>);

    const EXIT_RESTRICTED: i32 = 3;

    // ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED, comme un lecteur vidéo.
    const SCRIPT: &str = "\
    if ($ExecutionContext.SessionState.LanguageMode -ne 'FullLanguage') { exit 3 }
    Add-Type -Namespace W -Name P -MemberDefinition '[DllImport(\"kernel32.dll\")] public static extern uint SetThreadExecutionState(uint f);'
    [W.P]::SetThreadExecutionState(2147483651) | Out-Null
    [Console]::In.ReadToEnd() | Out-Null";

    impl Awake {
        pub fn start() -> Result<Self, String> {
            let mut cmd = if env::var_os("WSL_DISTRO_NAME").is_some() {
                let utf16: Vec<u8> = SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
                let mut c = Command::new("powershell.exe");
                c.args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &STANDARD.encode(utf16)]);
                c
            } else {
                let mut c = Command::new("systemd-inhibit");
                c.args(["--what=idle:sleep", "--who=MCO Dash", "--why=Poste maintenu éveillé", "cat"]);
                c
            };
            let child = cmd
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("lancement impossible : {e}"))?;
            Ok(Self(Some(child)))
        }

        /// Some(erreur) si le processus s'est arrêté tout seul.
        pub fn failure(&mut self) -> Option<String> {
            match self.0.as_mut()?.try_wait() {
                Ok(None) => None,
                Ok(Some(st)) if st.code() == Some(EXIT_RESTRICTED) => {
                    Some("PowerShell est en mode restreint sur ce poste (stratégie de sécurité) : impossible d'empêcher la veille".into())
                }
                Ok(Some(st)) => Some(format!("arrêt inattendu ({st})")),
                Err(e) => Some(e.to_string()),
            }
        }
    }

    impl Drop for Awake {
        fn drop(&mut self) {
            let Some(mut child) = self.0.take() else { return };
            drop(child.stdin.take());
            // Fin propre attendue ; sinon on force. Hors du thread d'interface (powershell.exe est lent à sortir).
            thread::spawn(move || {
                for _ in 0..30 {
                    if matches!(child.try_wait(), Ok(Some(_))) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }
}
