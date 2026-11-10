//! Argo CD: application status through the CLI, and the live resource tree (Deployments down
//! to Pods, in any cluster Argo CD manages) through its API with the CLI's saved login.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use serde_json::Value;

use crate::cli::{run, run_json};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArgoApp {
    pub name: String,
    pub namespace: String,
    pub project: String,
    /// Destination cluster: its Argo CD name, or its API server when only that is set.
    pub cluster: String,
    pub server: String,
    pub dest_namespace: String,
    pub sync: String,
    pub health: String,
    pub health_message: String,
    pub revisions: Vec<String>,
    /// Last operation: phase, message, finished at.
    pub operation: Option<(String, String, String)>,
    pub conditions: Vec<(String, String)>,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub uid: String,
    pub kind: String,
    pub group: String,
    pub name: String,
    pub namespace: String,
    pub parents: Vec<String>,
    pub health: String,
    pub health_message: String,
    pub info: Vec<(String, String)>,
    pub images: Vec<String>,
    pub created: String,
}

impl Node {
    pub fn info(&self, key: &str) -> Option<&str> {
        self.info
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

pub fn app(name: &str) -> Result<ArgoApp> {
    Ok(parse_app(&run_json(
        "argocd",
        &["app", "get", name, "-o", "json"],
    )?))
}

/// Asks Argo CD to compare the app with its sources now.
pub fn refresh(name: &str) -> Result<ArgoApp> {
    Ok(parse_app(&run_json(
        "argocd",
        &["app", "get", name, "--refresh", "-o", "json"],
    )?))
}

pub fn sync(name: &str) -> Result<()> {
    run("argocd", &["app", "sync", name, "--async"]).map(|_| ())
}

/// Argo CD cluster API servers → names.
pub fn clusters() -> Result<HashMap<String, String>> {
    let v = run_json("argocd", &["cluster", "list", "-o", "json"])?;
    Ok(v.as_array()
        .into_iter()
        .flatten()
        .map(|c| (s(&c["server"]), s(&c["name"])))
        .collect())
}

pub fn ui_url(app: &str) -> Option<String> {
    Some(format!("https://{}/applications/{app}", login()?.0))
}

pub fn tree(app: &str, app_ns: &str) -> Result<Vec<Node>> {
    let (server, _) = login().ok_or_else(|| anyhow!("not logged in to Argo CD (argocd login)"))?;
    let get = || -> Result<Value, Box<ureq::Error>> {
        let (_, token) = login().unwrap_or_default();
        let url = format!("https://{server}/api/v1/applications/{app}/resource-tree");
        let mut req = ureq::get(&url).set("Authorization", &format!("Bearer {token}"));
        if !app_ns.is_empty() {
            req = req.query("appNamespace", app_ns);
        }
        Ok(req
            .call()?
            .into_json::<Value>()
            .map_err(ureq::Error::from)?)
    };
    let v = match get() {
        Ok(v) => v,
        // The CLI renews an expired token when it next talks to the server.
        Err(e) if matches!(*e, ureq::Error::Status(401, _)) => {
            run("argocd", &["account", "get-user-info"])?;
            get().map_err(|e| anyhow!("Argo CD resource tree: {e}"))?
        }
        Err(e) => return Err(anyhow!("Argo CD resource tree: {e}")),
    };
    Ok(v["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|n| Node {
            uid: s(&n["uid"]),
            kind: s(&n["kind"]),
            group: s(&n["group"]),
            name: s(&n["name"]),
            namespace: s(&n["namespace"]),
            parents: n["parentRefs"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|p| s(&p["uid"]))
                .collect(),
            health: s(&n["health"]["status"]),
            health_message: s(&n["health"]["message"]),
            info: n["info"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|i| (s(&i["name"]), s(&i["value"])))
                .collect(),
            images: n["images"]
                .as_array()
                .into_iter()
                .flatten()
                .map(s)
                .collect(),
            created: s(&n["createdAt"]),
        })
        .collect())
}

/// The server and auth token of the CLI's current context, from `~/.config/argocd/config`.
fn login() -> Option<(String, String)> {
    let home = std::env::var_os("HOME")?;
    let path = std::env::var_os("ARGOCD_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(home).join(".config/argocd/config"));
    parse_login(&std::fs::read_to_string(path).ok()?)
}

/// Just enough YAML for the CLI's config: the current context's server and its user's token.
fn parse_login(cfg: &str) -> Option<(String, String)> {
    let field = |line: &str, key: &str| {
        line.trim()
            .trim_start_matches("- ")
            .strip_prefix(key)
            .and_then(|r| r.strip_prefix(':'))
            .map(|v| v.trim().trim_matches('"').to_string())
    };
    let current = cfg.lines().find_map(|l| field(l, "current-context"))?;
    // Context, server and user entries are lists of maps; collect each map's fields.
    let mut section = "";
    let mut entries: Vec<(String, HashMap<String, String>)> = Vec::new();
    for line in cfg.lines() {
        if !line.starts_with(' ') && !line.starts_with('-') {
            section = match line.trim_end_matches(':') {
                "contexts" => "contexts",
                "users" => "users",
                _ => "",
            };
            continue;
        }
        if section.is_empty() {
            continue;
        }
        if line.trim_start().starts_with("- ") {
            entries.push((section.to_string(), HashMap::new()));
        }
        if let (Some((_, map)), Some((k, v))) = (
            entries.last_mut(),
            line.trim().trim_start_matches("- ").split_once(':'),
        ) {
            map.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
    }
    let ctx = entries
        .iter()
        .find(|(sec, m)| sec == "contexts" && m.get("name") == Some(&current))?;
    let server = ctx.1.get("server")?.clone();
    let user = ctx.1.get("user")?;
    let token = entries
        .iter()
        .find(|(sec, m)| sec == "users" && m.get("name") == Some(user))?
        .1
        .get("auth-token")?
        .clone();
    Some((server, token))
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn parse_app(v: &Value) -> ArgoApp {
    let st = &v["status"];
    let dest = &v["spec"]["destination"];
    let sources: Vec<&Value> = match v["spec"]["sources"].as_array() {
        Some(list) => list.iter().collect(),
        None => vec![&v["spec"]["source"]],
    };
    let sources = sources
        .into_iter()
        .filter(|src| src.is_object())
        .map(|src| {
            let what = src["chart"]
                .as_str()
                .or_else(|| src["path"].as_str())
                .filter(|x| !x.is_empty())
                .unwrap_or_else(|| src["repoURL"].as_str().unwrap_or(""));
            format!(
                "{what} @ {}",
                src["targetRevision"].as_str().unwrap_or("HEAD")
            )
        })
        .collect();
    let mut revisions: Vec<String> = st["sync"]["revisions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(s)
        .collect();
    if revisions.is_empty()
        && let Some(r) = st["sync"]["revision"].as_str()
    {
        revisions.push(r.to_string());
    }
    let op = &st["operationState"];
    ArgoApp {
        name: s(&v["metadata"]["name"]),
        namespace: s(&v["metadata"]["namespace"]),
        project: s(&v["spec"]["project"]),
        cluster: s(&dest["name"]),
        server: s(&dest["server"]),
        dest_namespace: s(&dest["namespace"]),
        sync: s(&st["sync"]["status"]),
        health: s(&st["health"]["status"]),
        health_message: s(&st["health"]["message"]),
        revisions,
        operation: op
            .is_object()
            .then(|| (s(&op["phase"]), s(&op["message"]), s(&op["finishedAt"]))),
        conditions: st["conditions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| (s(&c["type"]), s(&c["message"])))
            .collect(),
        sources,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cli_config() {
        let cfg = "contexts:\n- name: argo.example.com\n  server: argo.example.com\n  user: argo.example.com\ncurrent-context: argo.example.com\nservers:\n- grpc-web: true\n  server: argo.example.com\nusers:\n- auth-token: abc.def\n  name: argo.example.com\n  refresh-token: xyz\n";
        assert_eq!(
            parse_login(cfg),
            Some(("argo.example.com".into(), "abc.def".into()))
        );
    }

    /// Talks to Argo CD: `ARGO_APP=my-app cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn reads_an_app_and_its_tree() {
        let name = std::env::var("ARGO_APP").expect("set ARGO_APP to an Argo CD application");
        let a = app(&name).unwrap();
        assert!(!a.dest_namespace.is_empty() && !a.health.is_empty());
        let nodes = tree(&name, &a.namespace).unwrap();
        assert!(nodes.iter().any(|n| n.kind == "Pod"));
    }
}
