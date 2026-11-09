use crate::config::FreshCfg;
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const NON_CLOSED: &str = "(status:2 OR status:3 OR status:6 OR status:7 OR status:8 OR status:9)";
const PAGE_SIZE: usize = 30;
const MAX_PAGES: usize = 10;
const ATTEMPTS: usize = 3;
const AGENTS_TTL: Duration = Duration::from_secs(300);

pub fn status_label(s: u8) -> &'static str {
    match s {
        2 => "Ouvert",
        3 => "En attente",
        4 => "Résolu",
        5 => "Clos",
        6 => "En cours d'analyse",
        7 => "En cours de résolution",
        8 => "En cours de vérification",
        9 => "En cours de validation",
        _ => "Inconnu",
    }
}

pub fn priority_label(p: u8) -> &'static str {
    match p {
        1 => "Basse",
        2 => "Moyenne",
        3 => "Haute",
        4 => "Urgente",
        _ => "Inconnue",
    }
}

pub struct Ticket {
    pub id: u64,
    pub subject: String,
    pub status: u8,
    pub priority: u8,
    pub responder_id: Option<u64>,
    pub assignee: Option<String>,
    pub created: Option<DateTime<Utc>>,
    pub updated: Option<DateTime<Utc>>,
    pub due_by: Option<DateTime<Utc>>,
}

impl Ticket {
    pub fn overdue(&self, now: DateTime<Utc>) -> bool {
        // Le SLA est suspendu quand le ticket est « En attente » (3).
        self.status != 3 && self.due_by.is_some_and(|d| d < now)
    }
}

/// Ids des tickets non assignés présents dans `new` mais pas dans `old`.
pub fn newly_unassigned(old: &[Ticket], new: &[Ticket]) -> Vec<u64> {
    new.iter()
        .filter(|t| t.responder_id.is_none() && !old.iter().any(|o| o.id == t.id))
        .map(|t| t.id)
        .collect()
}

/// Ids de mes tickets (`me`) apparus ou modifiés entre `old` et `new`.
pub fn mine_changed(old: &[Ticket], new: &[Ticket], me: u64) -> Vec<u64> {
    new.iter()
        .filter(|t| t.responder_id == Some(me))
        .filter(|t| old.iter().find(|o| o.id == t.id).is_none_or(|o| o.updated != t.updated))
        .map(|t| t.id)
        .collect()
}

/// Groupes et agents du compte, pour les sélecteurs des paramètres.
pub struct Directory {
    pub groups: Vec<(u64, String)>,
    pub agents: Vec<(u64, String)>,
}

fn sorted_names(items: impl Iterator<Item = (u64, String)>) -> Vec<(u64, String)> {
    let mut v: Vec<_> = items.collect();
    v.sort_by_key(|i| i.1.to_lowercase());
    v
}

pub struct Data {
    pub group_name: String,
    /// (id, nom) triés par nom, pour choisir « Mon compte » dans les paramètres.
    pub agents: Vec<(u64, String)>,
    pub tickets: Vec<Ticket>,
    pub truncated: bool,
    pub web_base: String,
}

pub struct Client {
    agent: ureq::Agent,
    cfg: FreshCfg,
    auth: String,
    agents: Option<(Instant, HashMap<u64, String>)>,
}

impl Client {
    pub fn new(cfg: FreshCfg) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        let auth = format!("Basic {}", STANDARD.encode(format!("{}:X", cfg.api_key)));
        Self { agent, cfg, auth, agents: None }
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, String> {
        let mut req = self.agent.get(format!("{}/{path}", self.cfg.base)).header("Authorization", &self.auth);
        for (k, v) in query {
            req = req.query(k, v);
        }
        let mut resp = req.call().map_err(|e| format!("connexion impossible : {e}"))?;
        match resp.status().as_u16() {
            200 => resp.body_mut().read_json().map_err(|e| e.to_string()),
            401 => Err("clé API refusée (HTTP 401)".into()),
            429 => Err("limite d'appels Freshservice atteinte (HTTP 429)".into()),
            s => Err(format!("{path} : HTTP {s}")),
        }
    }

    fn agents(&mut self) -> Result<&HashMap<u64, String>, String> {
        if self.agents.as_ref().is_none_or(|(at, _)| at.elapsed() > AGENTS_TTL) {
            let mut map = HashMap::new();
            for page in 1..=5 {
                let v = self.get("agents", &[("per_page", "100"), ("page", &page.to_string())])?;
                let batch = v["agents"].as_array().map(Vec::as_slice).unwrap_or_default();
                for a in batch {
                    let name = format!(
                        "{} {}",
                        a["first_name"].as_str().unwrap_or_default(),
                        a["last_name"].as_str().unwrap_or_default()
                    );
                    if let Some(id) = a["id"].as_u64() {
                        map.insert(id, name.trim().to_string());
                    }
                }
                if batch.len() < 100 {
                    break;
                }
            }
            self.agents = Some((Instant::now(), map));
        }
        Ok(&self.agents.as_ref().expect("rempli ci-dessus").1)
    }

    pub fn directory(&mut self) -> Result<Directory, String> {
        let mut groups = Vec::new();
        for page in 1..=5 {
            let v = self.get("groups", &[("per_page", "100"), ("page", &page.to_string())])?;
            let batch = v["groups"].as_array().map(Vec::as_slice).unwrap_or_default();
            groups.extend(batch.iter().filter_map(|g| Some((g["id"].as_u64()?, g["name"].as_str()?.to_string()))));
            if batch.len() < 100 {
                break;
            }
        }
        let agents = sorted_names(self.agents()?.iter().map(|(id, n)| (*id, n.clone())));
        Ok(Directory { groups: sorted_names(groups.into_iter()), agents })
    }

    pub fn fetch(&mut self) -> Result<Data, String> {
        let mut last = String::new();
        for _ in 0..ATTEMPTS {
            match self.fetch_once() {
                Ok(d) => return Ok(d),
                Err(e) => last = e,
            }
        }
        Err(format!("échec après {ATTEMPTS} tentatives ({last})"))
    }

    fn fetch_once(&mut self) -> Result<Data, String> {
        let query = format!("\"group_id:{} AND {NON_CLOSED}\"", self.cfg.group_id);
        let mut raw = Vec::new();
        let mut truncated = false;
        for page in 1..=MAX_PAGES {
            let v = self.get("tickets/filter", &[("query", &query), ("page", &page.to_string())])?;
            let batch = v["tickets"].as_array().cloned().unwrap_or_default();
            let full = batch.len() >= PAGE_SIZE;
            raw.extend(batch);
            if !full {
                break;
            }
            truncated = page == MAX_PAGES;
        }

        let group = self.get(&format!("groups/{}", self.cfg.group_id), &[])?;
        let group_name = group["group"]["name"].as_str().unwrap_or("Groupe").to_string();
        let agents = self.agents()?;

        let date = |v: &Value| {
            v.as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc))
        };
        // Résolu (4) et Clos (5) ne sont plus à traiter, même si l'API en renvoie.
        let tickets = raw
            .iter()
            .filter(|t| !matches!(t["status"].as_u64(), Some(4 | 5)))
            .map(|t| Ticket {
                id: t["id"].as_u64().unwrap_or_default(),
                subject: t["subject"].as_str().unwrap_or("Sans sujet").to_string(),
                status: t["status"].as_u64().unwrap_or_default() as u8,
                priority: t["priority"].as_u64().unwrap_or_default() as u8,
                responder_id: t["responder_id"].as_u64(),
                assignee: t["responder_id"]
                    .as_u64()
                    .map(|id| agents.get(&id).cloned().unwrap_or_else(|| format!("#{id}"))),
                created: date(&t["created_at"]),
                updated: date(&t["updated_at"]),
                due_by: date(&t["due_by"]),
            })
            .collect();

        let agents = sorted_names(agents.iter().map(|(id, n)| (*id, n.clone())));
        let web_base = self.cfg.base.trim_end_matches("/api/v2").to_string();
        Ok(Data { group_name, agents, tickets, truncated, web_base })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket(id: u64, responder_id: Option<u64>) -> Ticket {
        Ticket { id, subject: String::new(), status: 2, priority: 1, responder_id, assignee: None, created: None, updated: None, due_by: None }
    }

    #[test]
    fn mine_changed_reports_new_and_updated_tickets_of_mine_only() {
        let at = |m: i64| DateTime::from_timestamp(1_700_000_000 + m * 60, 0);
        let mut old = [ticket(1, Some(7)), ticket(2, Some(7)), ticket(3, Some(8))];
        old.iter_mut().for_each(|t| t.updated = at(0));
        let mut new = [ticket(1, Some(7)), ticket(2, Some(7)), ticket(3, Some(8)), ticket(4, Some(7))];
        new.iter_mut().for_each(|t| t.updated = at(0));
        new[1].updated = at(5);
        new[2].updated = at(5);
        assert_eq!(mine_changed(&old, &new, 7), vec![2, 4]);
    }

    #[test]
    fn only_new_unassigned_tickets_are_reported() {
        let old = [ticket(1, None), ticket(2, Some(9))];
        let new = [ticket(1, None), ticket(2, Some(9)), ticket(3, None), ticket(4, Some(9))];
        assert_eq!(newly_unassigned(&old, &new), vec![3]);
    }
}
