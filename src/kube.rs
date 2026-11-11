//! Kubernetes through `kubectl`, in the kube context that matches an Argo CD cluster.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};

use crate::cli::run_text;

pub fn contexts() -> Result<Vec<String>> {
    Ok(
        run_text("kubectl", &["config", "get-contexts", "-o", "name"])?
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    )
}

/// The kube context for an Argo CD cluster name: an override, an exact name, or a context
/// ending in it (`arn:…:cluster/eks-prod-1`, `gke_project_region_gke-staging-1`).
pub fn resolve(
    cluster: &str,
    contexts: &[String],
    overrides: &BTreeMap<String, String>,
) -> Option<String> {
    if let Some(c) = overrides.get(cluster) {
        return Some(c.clone());
    }
    if cluster.is_empty() {
        return None;
    }
    if contexts.iter().any(|c| c == cluster) {
        return Some(cluster.to_string());
    }
    let mut hits = contexts.iter().filter(|c| {
        c.strip_suffix(cluster)
            .is_some_and(|rest| rest.ends_with('/') || rest.ends_with('_') || rest.ends_with('@'))
    });
    match (hits.next(), hits.next()) {
        (Some(c), None) => Some(c.clone()),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Normal or Warning.
    pub kind: String,
    pub reason: String,
    pub message: String,
    pub count: u64,
    pub first: String,
    pub last: String,
    pub source: String,
    /// The part of the object it's about, e.g. `spec.containers{api}`.
    pub field: String,
}

/// Where a resource lives and what kubectl calls it.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub context: String,
    pub namespace: String,
    pub kind: String,
    pub group: String,
    pub name: String,
}

impl Target {
    /// `deployment.apps/web`, `pod/web-1`, `virtualservice.networking.istio.io/web`.
    pub fn resource(&self) -> String {
        let kind = self.kind.to_lowercase();
        if self.group.is_empty() {
            format!("{kind}/{}", self.name)
        } else {
            format!("{kind}.{}/{}", self.group, self.name)
        }
    }

    fn base(&self) -> Vec<String> {
        let mut a = vec!["--context".to_string(), self.context.clone()];
        if !self.namespace.is_empty() {
            a.extend(["-n".to_string(), self.namespace.clone()]);
        }
        a
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let mut a = self.base();
        a.extend(args.iter().map(|s| s.to_string()));
        let refs: Vec<&str> = a.iter().map(String::as_str).collect();
        run_text("kubectl", &refs)
    }

    pub fn has_logs(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "Pod" | "Deployment" | "StatefulSet" | "DaemonSet" | "ReplicaSet" | "Job"
        )
    }

    pub fn has_rollout(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "Deployment" | "StatefulSet" | "DaemonSet"
        )
    }

    pub fn describe(&self) -> Result<String> {
        self.run(&["describe", &self.resource()])
    }

    pub fn yaml(&self) -> Result<String> {
        self.run(&["get", &self.resource(), "-o", "yaml"])
    }

    pub fn events(&self) -> Result<Vec<Event>> {
        let out = self.run(&["events", "--for", &self.resource(), "-o", "json"])?;
        // kubectl prints nothing at all, not an empty list, when there are no events.
        if out.trim().is_empty() {
            return Ok(Vec::new());
        }
        let v: serde_json::Value =
            serde_json::from_str(&out).map_err(|e| anyhow!("couldn't parse events: {e}"))?;
        let s = |v: &serde_json::Value| v.as_str().unwrap_or("").to_string();
        let mut list: Vec<Event> = v["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| {
                let last = [
                    &e["lastTimestamp"],
                    &e["eventTime"],
                    &e["metadata"]["creationTimestamp"],
                ]
                .into_iter()
                .map(s)
                .find(|t| !t.is_empty())
                .unwrap_or_default();
                let source = [&e["source"]["component"], &e["reportingComponent"]]
                    .into_iter()
                    .map(s)
                    .find(|t| !t.is_empty())
                    .unwrap_or_default();
                Event {
                    kind: s(&e["type"]),
                    reason: s(&e["reason"]),
                    message: s(&e["message"]),
                    count: e["count"]
                        .as_u64()
                        .or_else(|| e["series"]["count"].as_u64())
                        .unwrap_or(1),
                    first: s(&e["firstTimestamp"]),
                    last,
                    source,
                    field: s(&e["involvedObject"]["fieldPath"]),
                }
            })
            .collect();
        list.sort_by(|a, b| b.last.cmp(&a.last));
        Ok(list)
    }

    pub fn containers(&self) -> Result<Vec<String>> {
        let path = if self.kind == "Pod" {
            "{.spec.containers[*].name}"
        } else {
            "{.spec.template.spec.containers[*].name}"
        };
        let out = self.run(&["get", &self.resource(), "-o", &format!("jsonpath={path}")])?;
        Ok(out.split_whitespace().map(str::to_string).collect())
    }

    pub fn restart(&self) -> Result<()> {
        self.run(&["rollout", "restart", &self.resource()])
            .map(|_| ())
    }

    pub fn delete(&self) -> Result<()> {
        self.run(&["delete", &self.resource(), "--wait=false"])
            .map(|_| ())
    }

    pub fn logs(&self, container: Option<&str>, previous: bool) -> Result<Stream> {
        let mut a = self.base();
        a.extend(["logs".into(), self.resource(), "--tail=500".into()]);
        if previous {
            a.push("--previous".into());
        } else {
            a.push("-f".into());
        }
        match container {
            Some(c) => a.extend(["-c".into(), c.into()]),
            None => a.push("--all-containers=true".into()),
        }
        if self.kind != "Pod" {
            a.extend(["--all-pods=true".into(), "--prefix=true".into()]);
        }
        Stream::spawn(a)
    }

    pub fn rollout(&self) -> Result<Stream> {
        let mut a = self.base();
        a.extend([
            "rollout".into(),
            "status".into(),
            self.resource(),
            "-w".into(),
        ]);
        Stream::spawn(a)
    }
}

#[derive(Default)]
struct Shared {
    pending: Vec<String>,
    done: Option<String>,
}

/// A running `kubectl` whose output lines are collected for the UI to take.
/// Dropping it stops the process.
pub struct Stream {
    shared: Arc<Mutex<Shared>>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Stream {
    fn spawn(args: Vec<String>) -> Result<Stream> {
        let mut child = Command::new("kubectl")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| anyhow!("couldn't start kubectl: {e}"))?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (out, err) = (child.stdout.take(), child.stderr.take());
        let child = Arc::new(Mutex::new(Some(child)));
        if let Some(err) = err {
            let sh = shared.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(err).lines().map_while(|l| l.ok()) {
                    sh.lock().unwrap().pending.push(format!("! {line}"));
                }
            });
        }
        if let Some(out) = out {
            let (sh, ch) = (shared.clone(), child.clone());
            std::thread::spawn(move || {
                let mut reader = BufReader::new(out);
                let mut buf = Vec::new();
                while matches!(reader.read_until(b'\n', &mut buf), Ok(n) if n > 0) {
                    let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                    sh.lock().unwrap().pending.push(line);
                    buf.clear();
                }
                let status = ch.lock().unwrap().as_mut().and_then(|c| c.wait().ok());
                sh.lock().unwrap().done = Some(match status {
                    Some(s) if s.success() => "Stream ended.".into(),
                    Some(s) => format!("kubectl exited ({s})."),
                    None => "Stopped.".into(),
                });
            });
        }
        Ok(Stream { shared, child })
    }

    /// Lines since the last call, and why the stream ended once it has.
    pub fn take(&self) -> (Vec<String>, Option<String>) {
        let mut sh = self.shared.lock().unwrap();
        (std::mem::take(&mut sh.pending), sh.done.clone())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Live usage of each pod in a namespace: name → (millicores, bytes). Needs metrics-server.
pub fn top_pods(
    context: &str,
    namespace: &str,
) -> Result<std::collections::HashMap<String, (u64, u64)>> {
    let out = run_text(
        "kubectl",
        &[
            "--context",
            context,
            "-n",
            namespace,
            "top",
            "pods",
            "--no-headers",
            "--request-timeout=15s",
        ],
    )?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (name, cpu, mem) = (f.next()?, f.next()?, f.next()?);
            Some((name.to_string(), (millicores(cpu)?, bytes(mem)?)))
        })
        .collect())
}

/// `250m` → 250, `2` → 2000, `1500000n` → 1.
pub fn millicores(q: &str) -> Option<u64> {
    if let Some(n) = q.strip_suffix('m') {
        return n.parse().ok();
    }
    if let Some(n) = q.strip_suffix('n') {
        return n.parse::<u64>().ok().map(|n| n / 1_000_000);
    }
    if let Some(n) = q.strip_suffix('u') {
        return n.parse::<u64>().ok().map(|n| n / 1_000);
    }
    q.parse::<f64>().ok().map(|c| (c * 1000.0).round() as u64)
}

/// `530Mi` → bytes; `1G`, `512Ki`, plain numbers too.
pub fn bytes(q: &str) -> Option<u64> {
    let units = [
        ("Ki", 1u64 << 10),
        ("Mi", 1 << 20),
        ("Gi", 1 << 30),
        ("Ti", 1 << 40),
        ("k", 1000),
        ("M", 1_000_000),
        ("G", 1_000_000_000),
    ];
    for (suffix, mult) in units {
        if let Some(n) = q.strip_suffix(suffix) {
            return n.parse::<f64>().ok().map(|n| (n * mult as f64) as u64);
        }
    }
    q.parse().ok()
}

pub fn fmt_cpu(m: u64) -> String {
    if m >= 1000 {
        format!("{:.2}", m as f64 / 1000.0)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    } else {
        format!("{m}m")
    }
}

pub fn fmt_mem(b: u64) -> String {
    let mi = b as f64 / (1u64 << 20) as f64;
    if mi >= 1024.0 {
        format!("{:.1}Gi", mi / 1024.0)
    } else {
        format!("{mi:.0}Mi")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantities() {
        assert_eq!(millicores("250m"), Some(250));
        assert_eq!(millicores("2"), Some(2000));
        assert_eq!(bytes("530Mi"), Some(530 << 20));
        assert_eq!(fmt_cpu(1500), "1.5");
        assert_eq!(fmt_mem(528 << 20), "528Mi");
    }

    #[test]
    fn matches_contexts_by_suffix() {
        let ctx: Vec<String> = [
            "arn:aws:eks:us-west-2:1:cluster/eks-prod-1",
            "gke_p_us-central1_gke-staging-1",
            "gke_p_us-central1_gke-staging-10",
            "docker-desktop",
        ]
        .map(String::from)
        .to_vec();
        let none = BTreeMap::new();
        assert_eq!(
            resolve("eks-prod-1", &ctx, &none).as_deref(),
            Some("arn:aws:eks:us-west-2:1:cluster/eks-prod-1")
        );
        assert_eq!(
            resolve("gke-staging-1", &ctx, &none).as_deref(),
            Some("gke_p_us-central1_gke-staging-1")
        );
        assert_eq!(resolve("prod", &ctx, &none), None);
        let over = BTreeMap::from([("prod".to_string(), "docker-desktop".to_string())]);
        assert_eq!(
            resolve("prod", &ctx, &over).as_deref(),
            Some("docker-desktop")
        );
    }
}
