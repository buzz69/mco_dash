use crate::secret;
use crate::sound::Sounds;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

pub struct CentreonCfg {
    pub base: String,
    pub user: String,
    pub pwd: String,
}

pub struct FreshCfg {
    pub base: String,
    pub api_key: String,
    pub group_id: u64,
}

#[derive(Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    /// A — sombre, rail de navigation, tout sur une page.
    #[default]
    Mission,
    /// C — clair, barre du haut, tickets en liste.
    Board,
}

/// Paramètres éditables depuis l'onglet Paramètres, persistés dans `settings.json`.
/// Un champ vide retombe sur la variable d'environnement / `.env` correspondante.
#[derive(Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub centreon_url: String,
    pub centreon_user: String,
    pub centreon_pwd: String,
    pub fresh_url: String,
    pub fresh_key: String,
    pub fresh_group: String,
    /// Id Freshservice de l'agent pour le filtre « Mes tickets ».
    pub fresh_me: String,
    pub style: Style,
    /// Mode discret : petite barre toujours visible.
    pub compact: bool,
    /// Mode discret : barre non épinglée (reste en premier plan par défaut).
    pub unpinned: bool,
    /// Désactive les notifications système quand l'appli est en arrière-plan.
    pub no_os_notify: bool,
    pub sounds: Sounds,
}

pub fn settings_path() -> PathBuf {
    #[cfg(windows)]
    let dir = env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    #[cfg(not(windows))]
    let dir = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_default();
    dir.join("mco-dash").join("settings.json")
}

fn var(name: &str) -> String {
    env::var(name).unwrap_or_default().trim().to_string()
}

fn secs(name: &str, default: u64, min: u64) -> u64 {
    var(name).parse().unwrap_or(default).max(min)
}

pub fn refresh_secs() -> (u64, u64) {
    (secs("CENTREON_REFRESH_INTERVAL", 15, 5), secs("CACHE_REFRESH_INTERVAL", 60, 30))
}

impl Settings {
    pub fn load() -> Self {
        dotenvy::dotenv().ok();
        let mut s: Self = fs::read_to_string(settings_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let legacy = !(secret::is_protected(&s.centreon_pwd) && secret::is_protected(&s.fresh_key));
        s.centreon_pwd = secret::reveal(&s.centreon_pwd);
        s.fresh_key = secret::reveal(&s.fresh_key);
        if legacy && cfg!(windows) {
            // Ancien fichier en clair : on le rechiffre tout de suite, avant la fusion avec l'environnement.
            let _ = s.save();
        }
        for (field, name) in [
            (&mut s.centreon_url, "CENTREON_URL"),
            (&mut s.centreon_user, "CENTREON_USER"),
            (&mut s.centreon_pwd, "CENTREON_PWD"),
            (&mut s.fresh_url, "FRESHSERVICE_BASE_URL"),
            (&mut s.fresh_key, "FRESHSERVICE_API_KEY"),
            (&mut s.fresh_group, "FRESHSERVICE_GROUP_ID"),
        ] {
            if field.trim().is_empty() {
                *field = var(name);
            }
        }
        s
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = settings_path();
        if let Some(dir) = path.parent() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder.create(dir)?;
        }
        let mut stored = self.clone();
        stored.centreon_pwd = secret::protect(&self.centreon_pwd);
        stored.fresh_key = secret::protect(&self.fresh_key);
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        // Contient des secrets : lisible par l'utilisateur seul (sous Windows, le profil utilisateur l'est déjà).
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(path)?;
        // `mode` ne vaut qu'à la création : on resserre aussi un fichier existant.
        #[cfg(unix)]
        f.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        f.write_all(serde_json::to_string_pretty(&stored)?.as_bytes())
    }

    pub fn centreon(&self) -> Option<CentreonCfg> {
        let (url, user) = (self.centreon_url.trim(), self.centreon_user.trim());
        (!url.is_empty() && !user.is_empty()).then(|| CentreonCfg {
            base: format!("{}/api/latest", url.trim_end_matches('/')),
            user: user.to_string(),
            pwd: self.centreon_pwd.clone(),
        })
    }

    /// (URL de l'API normalisée, clé) — suffisant pour lister groupes et agents.
    pub fn fresh_credentials(&self) -> Option<(String, String)> {
        let (url, key) = (self.fresh_url.trim().trim_end_matches('/'), self.fresh_key.trim());
        // Accepte « https://x.freshservice.com » comme « https://x.freshservice.com/api/v2 ».
        let base = if url.ends_with("/api/v2") { url.to_string() } else { format!("{url}/api/v2") };
        (!url.is_empty() && !key.is_empty()).then(|| (base, key.to_string()))
    }

    pub fn fresh(&self) -> Option<FreshCfg> {
        let group_id = self.fresh_group.trim().parse().ok().filter(|g| *g != 0)?;
        let (base, api_key) = self.fresh_credentials()?;
        Some(FreshCfg { base, api_key, group_id })
    }

    pub fn me(&self) -> Option<u64> {
        self.fresh_me.trim().parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_url_is_normalised_and_group_required() {
        let mut s = Settings { fresh_url: "https://x.freshservice.com/".into(), fresh_key: "k".into(), fresh_group: "42".into(), ..Default::default() };
        assert_eq!(s.fresh().unwrap().base, "https://x.freshservice.com/api/v2");
        s.fresh_url = "https://x.freshservice.com/api/v2".into();
        assert_eq!(s.fresh().unwrap().base, "https://x.freshservice.com/api/v2");
        s.fresh_group = "abc".into();
        assert!(s.fresh().is_none());
    }
}
