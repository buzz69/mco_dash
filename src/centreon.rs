use crate::config::CentreonCfg;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Status {
    Ok,
    Unknown,
    Warning,
    Critical,
}

impl Status {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "OK" => Some(Self::Ok),
            "UNKNOWN" => Some(Self::Unknown),
            "WARNING" => Some(Self::Warning),
            "CRITICAL" => Some(Self::Critical),
            _ => None,
        }
    }
}

pub struct Service {
    pub host: String,
    pub name: String,
    pub status: Status,
    pub output: String,
    pub ack: bool,
    pub hard: bool,
    pub since: Option<DateTime<Utc>>,
}

impl Service {
    /// Alerte "active" : état hard et non acquittée.
    pub fn active(&self) -> bool {
        self.hard && !self.ack
    }
}

pub struct Product {
    pub name: &'static str,
    pub status: Status,
    pub output: String,
}

#[derive(Clone, Copy, PartialEq)]
pub enum PollerState {
    Ok,
    Reload,
    Stopped,
}

pub struct Poller {
    pub name: String,
    pub address: String,
    pub state: PollerState,
}

#[derive(Default)]
pub struct Data {
    /// Compteurs [OK, WARNING, CRITICAL, UNKNOWN] ; les problèmes acquittés/soft comptent comme OK.
    pub counts: [usize; 4],
    pub masked: usize,
    pub problems: Vec<Service>,
    pub products: Vec<Product>,
    pub pollers: Vec<Poller>,
}

/// (nom affiché, [(service, hôte optionnel)])
const PRODUCTS: &[(&str, &[(&str, Option<&str>)])] = &[
    ("DataHub", &[("check_datahub", None)]),
    ("Dexto", &[("check_dexto", None)]),
    ("Izikap", &[("check_izikap", None)]),
    ("XAM", &[("check_xam", None)]),
    ("Ubika", &[("check_ubika", None)]),
    ("SOLR", &[("Check_indexation_SOLR", None)]),
    ("Réplication EC", &[("Check_Replication_EC", None)]),
    ("Espace Client", &[("check_espace_client", None)]),
    (
        "VITAM",
        &[
            ("check_vitam_status", None),
            ("check_vitam_atr", None),
            ("check_vitam_manifest", None),
        ],
    ),
    ("Tyk", &[("check_tyk_stats", Some("tyk_prod_1"))]),
];

pub struct Client {
    agent: ureq::Agent,
    cfg: CentreonCfg,
    token: Option<String>,
}

impl Client {
    pub fn new(cfg: CentreonCfg) -> Self {
        let agent = ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .disable_verification(true)
                    .build(),
            )
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        Self { agent, cfg, token: None }
    }

    fn login(&mut self) -> Result<(), String> {
        let body = json!({"security": {"credentials": {"login": self.cfg.user, "password": self.cfg.pwd}}});
        let mut resp = self
            .agent
            .post(format!("{}/login", self.cfg.base))
            .send_json(body)
            .map_err(|e| format!("connexion impossible : {e}"))?;
        if resp.status().as_u16() != 200 {
            return Err(format!("authentification refusée (HTTP {})", resp.status().as_u16()));
        }
        let v: Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;
        let token = v["security"]["token"].as_str().ok_or("token absent de la réponse")?;
        self.token = Some(token.to_string());
        Ok(())
    }

    fn get(&mut self, path: &str) -> Result<Value, String> {
        for _ in 0..2 {
            if self.token.is_none() {
                self.login()?;
            }
            let mut resp = self
                .agent
                .get(format!("{}/{path}", self.cfg.base))
                .header("X-AUTH-TOKEN", self.token.as_deref().unwrap_or_default())
                .call()
                .map_err(|e| format!("connexion impossible : {e}"))?;
            match resp.status().as_u16() {
                200 => {
                    return resp
                        .body_mut()
                        .with_config()
                        .limit(256 * 1024 * 1024)
                        .read_json()
                        .map_err(|e| e.to_string());
                }
                401 => self.token = None,
                s => return Err(format!("{path} : HTTP {s}")),
            }
        }
        Err("session expirée, réauthentification impossible".into())
    }

    pub fn fetch(&mut self) -> Result<Data, String> {
        let services = self.get("monitoring/services?limit=10000")?;
        let mut data = Data::default();
        let mut all: Vec<(Service, Status)> = Vec::new();
        for s in services["result"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let name = s["status"]["name"].as_str().or(s["status"].as_str()).unwrap_or_default();
            let Some(status) = Status::parse(name) else { continue };
            let svc = parse_service(s, status);
            if status != Status::Ok && svc.active() {
                data.counts[match status {
                    Status::Warning => 1,
                    Status::Critical => 2,
                    _ => 3,
                }] += 1;
            } else {
                data.counts[0] += 1;
                if status != Status::Ok {
                    data.masked += 1;
                }
            }
            all.push((svc, status));
        }

        data.products = PRODUCTS
            .iter()
            .map(|(name, checks)| product(name, checks, &all))
            .collect();
        data.problems = all
            .into_iter()
            .filter(|(_, st)| matches!(st, Status::Warning | Status::Critical | Status::Unknown))
            .map(|(s, _)| s)
            .collect();
        // Les pollers sont secondaires : leur échec ne doit pas masquer les alertes.
        data.pollers = self.pollers().unwrap_or_default();
        Ok(data)
    }

    fn pollers(&mut self) -> Result<Vec<Poller>, String> {
        let topo = self.get("platform/topology")?;
        let servers = self.get("monitoring/servers").unwrap_or(Value::Null);
        let list = servers["result"].as_array().or(servers.as_array());
        let find = |id: &str| {
            list.and_then(|l| {
                l.iter().find(|s| {
                    let sid = s.get("id").or(s.get("server_id"));
                    sid.map(|v| v.to_string().trim_matches('"').to_string()).as_deref() == Some(id)
                })
            })
        };
        let flag = |s: Option<&Value>, keys: &[&str], default: bool| {
            keys.iter()
                .find_map(|k| s.and_then(|s| s.get(*k)).and_then(Value::as_bool))
                .unwrap_or(default)
        };

        let mut pollers = Vec::new();
        for (node_id, node) in topo["graph"]["nodes"].as_object().into_iter().flatten() {
            let meta = &node["metadata"];
            let id = meta["centreon-id"].as_str().unwrap_or(node_id);
            let st = find(id);
            let running = flag(st, &["running", "is_running"], true);
            let reload = flag(st, &["has_changed", "configuration_changed"], false)
                || meta["pending"].as_str() == Some("true");
            pollers.push(Poller {
                name: node["label"]
                    .as_str()
                    .or(meta["hostname"].as_str())
                    .unwrap_or("Inconnu")
                    .to_string(),
                address: meta["address"].as_str().unwrap_or_default().to_string(),
                state: if !running {
                    PollerState::Stopped
                } else if reload {
                    PollerState::Reload
                } else {
                    PollerState::Ok
                },
            });
        }
        pollers.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(pollers)
    }
}

fn parse_service(s: &Value, status: Status) -> Service {
    let host = s["host"]["name"].as_str().or(s["host"].as_str()).unwrap_or_default();
    let since = match &s["last_state_change"] {
        Value::String(t) => DateTime::parse_from_rfc3339(t).ok().map(|d| d.with_timezone(&Utc)),
        Value::Number(n) => n.as_i64().and_then(|t| DateTime::from_timestamp(t, 0)),
        _ => None,
    };
    Service {
        host: host.to_string(),
        name: s["display_name"].as_str().or(s["name"].as_str()).unwrap_or_default().to_string(),
        status,
        output: s["information"].as_str().or(s["output"].as_str()).unwrap_or_default().trim().to_string(),
        ack: s["is_acknowledged"].as_bool().or(s["acknowledged"].as_bool()).unwrap_or(false),
        hard: s["state_type"].as_i64().unwrap_or(1) == 1,
        since,
    }
}

fn product(name: &'static str, checks: &[(&str, Option<&str>)], all: &[(Service, Status)]) -> Product {
    let mut status = Status::Ok;
    let mut output = String::new();
    for (check, host) in checks {
        let found = all
            .iter()
            .find(|(s, _)| s.name == *check && host.is_none_or(|h| s.host == h));
        let (st, out) = match found {
            Some((s, st)) => (*st, s.output.clone()),
            None => (Status::Unknown, format!("Service « {check} » introuvable")),
        };
        if st > status || (output.is_empty() && st != Status::Ok) {
            status = st;
            output = out;
        }
    }
    Product { name, status, output }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(host: &str, name: &str, out: &str) -> Service {
        Service { host: host.into(), name: name.into(), status: Status::Ok, output: out.into(), ack: false, hard: true, since: None }
    }

    #[test]
    fn product_takes_worst_status_and_flags_missing_service() {
        let all = vec![(svc("a", "c1", "ok"), Status::Ok), (svc("a", "c2", "boom"), Status::Critical), (svc("b", "c3", "warn"), Status::Warning)];
        let p = product("P", &[("c1", None), ("c2", None)], &all);
        assert_eq!((p.status, p.output.as_str()), (Status::Critical, "boom"));

        let p = product("P", &[("c3", Some("a"))], &all);
        assert_eq!(p.status, Status::Unknown);

        let p = product("P", &[("c1", None)], &all);
        assert_eq!(p.status, Status::Ok);
    }
}
