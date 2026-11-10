//! Kargo through its CLI: projects, warehouses, stages, freight and promotions.

use std::collections::BTreeSet;

use anyhow::Result;
use serde_json::Value;

use crate::cli::{run, run_json};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectData {
    pub warehouses: Vec<Warehouse>,
    pub stages: Vec<Stage>,
    /// Newest first.
    pub freight: Vec<Freight>,
    /// Newest first.
    pub promotions: Vec<Promotion>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Warehouse {
    pub name: String,
    pub ready: Option<bool>,
    pub message: String,
    pub interval: String,
    pub subscriptions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Requested {
    pub warehouse: String,
    /// Takes freight straight from the warehouse.
    pub direct: bool,
    /// Takes freight verified in these stages.
    pub upstream: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FreightRef {
    pub name: String,
    pub warehouse: String,
    pub artifacts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub items: Vec<FreightRef>,
    /// The last verification: phase and when it started.
    pub verification: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stage {
    pub name: String,
    pub ready: Option<bool>,
    pub message: String,
    /// Kargo's health: Healthy, Unhealthy, Progressing, Unknown or empty.
    pub health: String,
    pub requested: Vec<Requested>,
    pub auto_promote: bool,
    /// Argo CD applications (namespace, name) the stage watches.
    pub apps: Vec<(String, String)>,
    /// Newest first; the first entry is what the stage runs now.
    pub history: Vec<HistoryEntry>,
    pub current_promotion: Option<String>,
    pub vars: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Freight {
    pub name: String,
    pub alias: String,
    pub warehouse: String,
    pub created: String,
    pub artifacts: Vec<String>,
    pub source: String,
    pub verified_in: Vec<String>,
    pub approved_for: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub alias: String,
    pub status: String,
    pub message: String,
    pub started: String,
    pub finished: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Promotion {
    pub name: String,
    pub stage: String,
    pub freight: String,
    pub phase: String,
    pub message: String,
    pub created: String,
    pub started: String,
    pub finished: String,
    pub total_steps: usize,
    pub steps: Vec<Step>,
}

impl Promotion {
    pub fn running(&self) -> bool {
        matches!(self.phase.as_str(), "Running" | "Pending" | "")
    }
}

impl Stage {
    pub fn current(&self) -> &[FreightRef] {
        self.history.first().map_or(&[], |h| h.items.as_slice())
    }

    pub fn is_current(&self, freight: &str) -> bool {
        self.current().iter().any(|f| f.name == freight)
    }

    pub fn takes_from(&self, warehouse: &str) -> bool {
        self.requested.iter().any(|r| r.warehouse == warehouse)
    }

    /// Whether `f` may be promoted here: it comes from a warehouse this stage requests from,
    /// straight from it or after verification in an upstream stage, or it's approved for it.
    pub fn accepts(&self, f: &Freight) -> bool {
        if f.approved_for.contains(&self.name) {
            return true;
        }
        self.requested.iter().any(|r| {
            r.warehouse == f.warehouse
                && (r.direct || r.upstream.iter().any(|u| f.verified_in.contains(u)))
        })
    }

    /// The stage name without its warehouse prefix: `shop-gke-staging-1` → `gke-staging-1`.
    pub fn short(&self) -> &str {
        self.requested
            .iter()
            .find_map(|r| {
                self.name
                    .strip_prefix(r.warehouse.as_str())
                    .and_then(|s| s.strip_prefix('-'))
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or(&self.name)
    }
}

impl ProjectData {
    pub fn stage(&self, name: &str) -> Option<&Stage> {
        self.stages.iter().find(|s| s.name == name)
    }

    pub fn freight(&self, name: &str) -> Option<&Freight> {
        self.freight.iter().find(|f| f.name == name)
    }

    /// Stages fed by `warehouse`, upstream before downstream.
    pub fn pipeline(&self, warehouse: &str) -> Vec<&Stage> {
        let mut out: Vec<(usize, &Stage)> = Vec::new();
        let fed: Vec<&Stage> = self
            .stages
            .iter()
            .filter(|s| s.takes_from(warehouse))
            .collect();
        let mut placed: BTreeSet<&str> = BTreeSet::new();
        let mut depth = 0;
        while placed.len() < fed.len() && depth <= fed.len() {
            let ready: Vec<&Stage> = fed
                .iter()
                .copied()
                .filter(|s| !placed.contains(s.name.as_str()))
                .filter(|s| {
                    s.requested
                        .iter()
                        .filter(|r| r.warehouse == warehouse)
                        .all(|r| r.upstream.iter().all(|u| placed.contains(u.as_str())))
                })
                .collect();
            // A cycle or an upstream from another warehouse: place the rest as they are.
            let ready = if ready.is_empty() {
                fed.iter()
                    .copied()
                    .filter(|s| !placed.contains(s.name.as_str()))
                    .collect()
            } else {
                ready
            };
            for s in ready {
                placed.insert(&s.name);
                out.push((depth, s));
            }
            depth += 1;
        }
        out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.name.cmp(&b.1.name)));
        out.into_iter().map(|(_, s)| s).collect()
    }

    /// Stages that take freight verified in `stage`.
    pub fn downstream(&self, stage: &str) -> Vec<&Stage> {
        self.stages
            .iter()
            .filter(|s| {
                s.requested
                    .iter()
                    .any(|r| r.upstream.iter().any(|u| u == stage))
            })
            .collect()
    }
}

pub fn list_projects() -> Result<Vec<String>> {
    let v = kargo_json(&["get", "projects"])?;
    let mut names: Vec<String> = items(&v).map(|p| s(&p["metadata"]["name"])).collect();
    names.sort();
    Ok(names)
}

pub fn load_project(project: &str) -> Result<ProjectData> {
    let p = format!("--project={project}");
    let get = |kind: &'static str| {
        let p = p.clone();
        std::thread::spawn(move || kargo_json(&["get", kind, &p]))
    };
    let (w, st, f, pr) = (
        get("warehouses"),
        get("stages"),
        get("freight"),
        get("promotions"),
    );
    let join = |h: std::thread::JoinHandle<Result<Value>>| {
        h.join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("kargo thread panicked")))
    };
    let (w, st, f, pr) = (join(w)?, join(st)?, join(f)?, join(pr)?);
    let mut freight: Vec<Freight> = items(&f).map(freight).collect();
    freight.sort_by(|a, b| b.created.cmp(&a.created));
    let mut promotions: Vec<Promotion> = items(&pr).map(promotion).collect();
    promotions.sort_by(|a, b| b.created.cmp(&a.created).then(b.name.cmp(&a.name)));
    let mut stages: Vec<Stage> = items(&st).map(stage).collect();
    stages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(ProjectData {
        warehouses: items(&w).map(warehouse).collect(),
        stages,
        freight,
        promotions,
    })
}

pub fn promote(project: &str, freight: &str, stage: &str) -> Result<()> {
    kargo(&[
        "promote",
        &format!("--project={project}"),
        &format!("--freight={freight}"),
        &format!("--stage={stage}"),
    ])
}

pub fn promote_downstream(project: &str, freight: &str, from: &str) -> Result<()> {
    kargo(&[
        "promote",
        &format!("--project={project}"),
        &format!("--freight={freight}"),
        &format!("--downstream-from={from}"),
    ])
}

pub fn abort(project: &str, promotion: &str) -> Result<()> {
    kargo(&[
        "promote",
        &format!("--project={project}"),
        &format!("--name={promotion}"),
        "--abort",
    ])
}

pub fn refresh_warehouse(project: &str, name: &str) -> Result<()> {
    kargo(&[
        "refresh",
        "warehouse",
        name,
        &format!("--project={project}"),
    ])
}

/// Where the Kargo UI is, from the CLI's config.
pub fn ui_url() -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let cfg =
        std::fs::read_to_string(std::path::Path::new(&home).join(".config/kargo/config")).ok()?;
    cfg.lines()
        .find_map(|l| l.trim().strip_prefix("apiAddress:"))
        .map(|u| u.trim().trim_matches('"').trim_end_matches('/').to_string())
}

fn kargo(args: &[&str]) -> Result<()> {
    run("kargo", args).map(|_| ())
}

fn kargo_json(args: &[&str]) -> Result<Value> {
    let mut a = args.to_vec();
    a.extend(["-o", "json"]);
    run_json("kargo", &a)
}

fn items(v: &Value) -> impl Iterator<Item = &Value> {
    v["items"].as_array().into_iter().flatten()
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn keys(v: &Value) -> Vec<String> {
    v.as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

fn condition(v: &Value, kind: &str) -> Option<(bool, String)> {
    v["status"]["conditions"]
        .as_array()?
        .iter()
        .find(|c| c["type"] == kind)
        .map(|c| (c["status"] == "True", s(&c["message"])))
}

fn last(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `shop:0.2.43`, `chart shop:0.2.43`, `deploy-config@1a2b3c4`.
fn artifacts(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for i in v["images"].as_array().into_iter().flatten() {
        out.push(format!(
            "{}:{}",
            last(i["repoURL"].as_str().unwrap_or("")),
            s(&i["tag"])
        ));
    }
    for c in v["charts"].as_array().into_iter().flatten() {
        let name = c["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| last(c["repoURL"].as_str().unwrap_or("")));
        out.push(format!("chart {name}:{}", s(&c["version"])));
    }
    for c in v["commits"].as_array().into_iter().flatten() {
        let id = s(&c["id"]);
        let repo = last(c["repoURL"].as_str().unwrap_or("")).trim_end_matches(".git");
        let tag = c["tag"].as_str().filter(|t| !t.is_empty());
        out.push(match tag {
            Some(t) => format!("{repo}@{t}"),
            None => format!("{repo}@{}", id.get(..7).unwrap_or(&id)),
        });
    }
    out
}

fn freight_ref(v: &Value) -> FreightRef {
    FreightRef {
        name: s(&v["name"]),
        warehouse: s(&v["origin"]["name"]),
        artifacts: artifacts(v),
    }
}

fn warehouse(v: &Value) -> Warehouse {
    let ready = condition(v, "Ready");
    let subscriptions = v["spec"]["subscriptions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|sub| {
            let (kind, body) = sub.as_object()?.iter().next()?;
            Some(format!(
                "{kind} {}",
                last(body["repoURL"].as_str().unwrap_or(""))
            ))
        })
        .collect();
    Warehouse {
        name: s(&v["metadata"]["name"]),
        ready: ready.as_ref().map(|r| r.0),
        message: ready.map(|r| r.1).unwrap_or_default(),
        interval: s(&v["spec"]["interval"]),
        subscriptions,
    }
}

fn stage(v: &Value) -> Stage {
    let st = &v["status"];
    let ready = condition(v, "Ready");
    let requested = v["spec"]["requestedFreight"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| Requested {
            warehouse: s(&r["origin"]["name"]),
            direct: r["sources"]["direct"].as_bool().unwrap_or(false),
            upstream: r["sources"]["stages"]
                .as_array()
                .into_iter()
                .flatten()
                .map(s)
                .collect(),
        })
        .collect();
    let mut apps: Vec<(String, String)> = Vec::new();
    for out in st["health"]["output"].as_array().into_iter().flatten() {
        for a in out["applicationStatuses"].as_array().into_iter().flatten() {
            let app = (s(&a["Namespace"]), s(&a["Name"]));
            if !app.1.is_empty() && !apps.contains(&app) {
                apps.push(app);
            }
        }
    }
    // Kargo clears the health output while a promotion runs; the last promotion's Argo CD
    // health checks name the same apps.
    if apps.is_empty() {
        for hc in st["lastPromotion"]["status"]["healthChecks"]
            .as_array()
            .into_iter()
            .flatten()
        {
            for a in hc["config"]["apps"].as_array().into_iter().flatten() {
                let ns = a["namespace"].as_str().unwrap_or("argocd").to_string();
                let app = (ns, s(&a["name"]));
                if !app.1.is_empty() && !apps.contains(&app) {
                    apps.push(app);
                }
            }
        }
    }
    let history = st["freightHistory"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|h| HistoryEntry {
            items: h["items"]
                .as_object()
                .map(|m| m.values().map(freight_ref).collect())
                .unwrap_or_default(),
            verification: h["verificationHistory"]
                .as_array()
                .and_then(|v| v.first())
                .map(|v| (s(&v["phase"]), s(&v["startTime"]))),
        })
        .collect();
    let vars = v["spec"]["promotionTemplate"]["spec"]["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|step| step["vars"].as_array().into_iter().flatten())
        .map(|v| (s(&v["name"]), s(&v["value"])))
        .collect();
    let current_promotion = st["currentPromotion"]["name"].as_str().map(str::to_string);
    Stage {
        name: s(&v["metadata"]["name"]),
        ready: ready.as_ref().map(|r| r.0),
        message: ready.map(|r| r.1).unwrap_or_default(),
        health: s(&st["health"]["status"]),
        requested,
        auto_promote: st["autoPromotionEnabled"].as_bool().unwrap_or(false),
        apps,
        history,
        current_promotion,
        vars,
    }
}

fn freight(v: &Value) -> Freight {
    let source = v["images"]
        .as_array()
        .and_then(|i| i.first())
        .and_then(|i| i["annotations"]["org.opencontainers.image.source"].as_str())
        .or_else(|| {
            v["commits"]
                .as_array()
                .and_then(|c| c.first())
                .and_then(|c| c["repoURL"].as_str())
        })
        .unwrap_or("")
        .to_string();
    let created = v["discoveredAt"]
        .as_str()
        .unwrap_or_else(|| v["metadata"]["creationTimestamp"].as_str().unwrap_or(""))
        .to_string();
    let alias = v["alias"]
        .as_str()
        .or_else(|| v["metadata"]["labels"]["kargo.akuity.io/alias"].as_str())
        .unwrap_or("")
        .to_string();
    Freight {
        name: s(&v["metadata"]["name"]),
        alias,
        warehouse: s(&v["origin"]["name"]),
        created,
        artifacts: artifacts(v),
        source,
        verified_in: keys(&v["status"]["verifiedIn"]),
        approved_for: keys(&v["status"]["approvedFor"]),
    }
}

fn promotion(v: &Value) -> Promotion {
    let st = &v["status"];
    Promotion {
        name: s(&v["metadata"]["name"]),
        stage: s(&v["spec"]["stage"]),
        freight: s(&v["spec"]["freight"]),
        phase: s(&st["phase"]),
        message: s(&st["message"]),
        created: s(&v["metadata"]["creationTimestamp"]),
        started: s(&st["startedAt"]),
        finished: s(&st["finishedAt"]),
        total_steps: v["spec"]["steps"].as_array().map_or(0, Vec::len),
        steps: st["stepExecutionMetadata"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| Step {
                alias: s(&m["alias"]),
                status: s(&m["status"]),
                message: s(&m["message"]),
                started: s(&m["startedAt"]),
                finished: s(&m["finishedAt"]),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads a real project: `KARGO_PROJECT=my-project cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn loads_a_project() {
        let project = std::env::var("KARGO_PROJECT").expect("set KARGO_PROJECT to a Kargo project");
        assert!(list_projects().unwrap().contains(&project));
        let d = load_project(&project).unwrap();
        assert!(!d.stages.is_empty() && !d.freight.is_empty());
        for w in &d.warehouses {
            let stages = d.pipeline(&w.name);
            assert_eq!(
                stages.len(),
                d.stages.iter().filter(|s| s.takes_from(&w.name)).count()
            );
        }
        let st = &d.stages[0];
        assert!(
            !st.apps.is_empty(),
            "stage {} names no Argo CD app",
            st.name
        );
        assert!(d.freight.iter().any(|f| st.is_current(&f.name)));
    }
}
