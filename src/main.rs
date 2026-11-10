//! kargoslop: Kargo projects and their stages on the left, a stage's pipeline, promotions and
//! live Kubernetes resources in the middle, and the selected resource's logs, rollout,
//! description, events or YAML on the right.

mod appbundle;
mod argo;
mod cli;
mod format;
mod inspector;
mod kargo;
mod kube;
mod pipeline;
mod stage;
mod store;
mod view;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use rune::{AppSpec, Chrome, KeyEvent, TextEdit, Tx, Waker};
use rune_kit::TableState;
use rune_kit::shell::{self, PaneState};

use argo::{ArgoApp, Node};
use kargo::ProjectData;
use store::Store;

type Job = Box<dyn FnOnce(&mut App, &mut Tx) + Send>;

/// The selected project refreshes this often (seconds); the others less often.
const PROJECT_ACTIVE: f64 = 5.0;
const PROJECT_IDLE: f64 = 30.0;
const APP_REFRESH: f64 = 4.0;
const MAX_LINES: usize = 5000;

#[derive(Clone, PartialEq)]
enum View {
    Pipeline(String),
    Stage(String, String),
}

#[derive(Clone, Copy, PartialEq)]
enum StageTab {
    Resources,
    Promotions,
    History,
}

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Logs,
    Rollout,
    Describe,
    Events,
    Yaml,
}

#[derive(Default)]
struct Project {
    data: Option<ProjectData>,
    loading: bool,
    error: Option<String>,
    collapsed: bool,
    next: f64,
}

#[derive(Default)]
struct AppState {
    app: Option<ArgoApp>,
    nodes: Vec<Node>,
    loading: bool,
    error: Option<String>,
    next: f64,
}

/// A row of the resources table: a node of an Argo CD app's tree, indented under its owner.
struct ResRow {
    key: String,
    app: String,
    depth: usize,
    node: Node,
}

struct Selected {
    key: String,
    app: String,
    node: Node,
    target: Result<kube::Target, String>,
}

/// What the inspector's text view shows: a stream (logs, rollout) or a command's output.
#[derive(Default)]
struct Output {
    lines: Vec<String>,
    /// For streams: `lines` parsed as log lines, one each.
    parsed: Vec<format::LogLine>,
    /// Id of `lines[0]`; ids stay put as old lines are dropped.
    first_id: u64,
    events: Option<Vec<kube::Event>>,
    describe: Vec<format::Entry>,
    /// Indexes of `lines` matching the filter.
    shown: Vec<usize>,
    version: u64,
    loading: bool,
    done: Option<String>,
    stream: Option<kube::Stream>,
}

enum Confirm {
    Promote {
        project: String,
        stage: String,
        freight: String,
    },
    Downstream {
        project: String,
        from: String,
        freight: String,
    },
    Abort {
        project: String,
        promotion: String,
    },
    Sync(String),
    Restart(kube::Target),
    Delete(kube::Target),
    InstallApp,
}

struct AddProject {
    edit: TextEdit,
    available: Option<Result<Vec<String>, String>>,
}

struct App {
    store: Store,
    jobs: (Sender<Job>, Receiver<Job>),
    waker: Option<Waker>,
    sidebar: PaneState,
    inspector: PaneState,
    projects: BTreeMap<String, Project>,
    view: Option<View>,
    tab: StageTab,
    apps: HashMap<String, AppState>,
    rows: Vec<ResRow>,
    table: TableState,
    show_all: bool,
    selected: Option<Selected>,
    pane: Pane,
    out: Output,
    out_gen: u64,
    filter: TextEdit,
    /// The filter text `out.shown` was computed for.
    filtered_for: String,
    containers: Option<Vec<String>>,
    container: Option<String>,
    /// Log line ids flipped from the `expand_all` default.
    expanded: HashSet<u64>,
    expand_all: bool,
    raw_logs: bool,
    /// Describe sections flipped from their default open state.
    toggled: HashSet<String>,
    container_open: bool,
    previous: bool,
    contexts: Vec<String>,
    clusters: HashMap<String, String>,
    confirm: Option<Confirm>,
    promote_pick: Option<(String, String)>,
    add: Option<AddProject>,
    busy: bool,
    toast: Option<(String, bool)>,
    toast_until: Option<f64>,
}

fn init() -> App {
    let store = Store::load();
    let projects = store
        .projects
        .iter()
        .map(|p| (p.clone(), Project::default()))
        .collect();
    App {
        store,
        jobs: channel(),
        waker: None,
        sidebar: PaneState::new(280.0).range(200.0, 480.0),
        inspector: PaneState::new(560.0).range(360.0, 1200.0),
        projects,
        view: None,
        tab: StageTab::Resources,
        apps: HashMap::new(),
        rows: Vec::new(),
        table: TableState::new().key("resources"),
        show_all: false,
        selected: None,
        pane: Pane::Logs,
        out: Output::default(),
        out_gen: 0,
        filter: TextEdit::new(""),
        filtered_for: String::new(),
        containers: None,
        container: None,
        expanded: HashSet::new(),
        expand_all: false,
        raw_logs: false,
        toggled: HashSet::new(),
        container_open: false,
        previous: false,
        contexts: Vec::new(),
        clusters: HashMap::new(),
        confirm: None,
        promote_pick: None,
        add: None,
        busy: false,
        toast: None,
        toast_until: None,
    }
}

impl App {
    /// Runs `work` on a thread and `done` with its result on the UI thread.
    fn bg<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&mut App, T, &mut Tx) + Send + 'static,
    ) {
        let (sender, waker) = (self.jobs.0.clone(), self.waker.clone());
        std::thread::spawn(move || {
            let out = work();
            let _ = sender.send(Box::new(move |s: &mut App, tx: &mut Tx| done(s, out, tx)));
            if let Some(w) = waker {
                w();
            }
        });
    }

    fn notify(&mut self, msg: impl Into<String>, error: bool) {
        self.toast = Some((msg.into(), error));
        self.toast_until = None;
    }

    fn save(&mut self) {
        if let Err(e) = self.store.save() {
            self.notify(format!("Couldn't save state: {e}"), true);
        }
    }

    fn project_name(&self) -> Option<&str> {
        match self.view.as_ref()? {
            View::Pipeline(p) | View::Stage(p, _) => Some(p),
        }
    }

    fn data(&self, project: &str) -> Option<&ProjectData> {
        self.projects.get(project)?.data.as_ref()
    }

    fn stage(&self) -> Option<(&str, &kargo::Stage)> {
        let Some(View::Stage(p, st)) = &self.view else {
            return None;
        };
        Some((p, self.data(p)?.stage(st)?))
    }

    fn open(&mut self, view: View) {
        if self.view.as_ref() == Some(&view) {
            return;
        }
        let same_stage = matches!(
            (&self.view, &view),
            (Some(View::Stage(..)), View::Stage(..))
        );
        self.view = Some(view.clone());
        self.promote_pick = None;
        if !same_stage {
            self.tab = StageTab::Resources;
        }
        self.select(None);
        if let Some(p) = self.projects.get_mut(match &view {
            View::Pipeline(p) | View::Stage(p, _) => p,
        }) {
            p.next = 0.0;
        }
        for app in self.stage_apps() {
            self.apps.entry(app).or_default().next = 0.0;
        }
        self.rebuild_rows();
    }

    fn stage_apps(&self) -> Vec<String> {
        self.stage()
            .map(|(_, st)| st.apps.iter().map(|a| a.1.clone()).collect())
            .unwrap_or_default()
    }

    fn refresh_project(&mut self, name: &str) {
        let Some(p) = self.projects.get_mut(name) else {
            return;
        };
        if p.loading {
            return;
        }
        p.loading = true;
        let name = name.to_string();
        self.bg(
            {
                let name = name.clone();
                move || kargo::load_project(&name)
            },
            move |s, res, tx| {
                let active = s.project_name() == Some(name.as_str());
                let Some(p) = s.projects.get_mut(&name) else {
                    return;
                };
                p.loading = false;
                p.next = tx.clock() + if active { PROJECT_ACTIVE } else { PROJECT_IDLE };
                match res {
                    Ok(mut d) => {
                        p.error = None;
                        if let Some(old) = &p.data {
                            for st in d.stages.iter_mut().filter(|st| st.apps.is_empty()) {
                                if let Some(prev) = old.stage(&st.name) {
                                    st.apps = prev.apps.clone();
                                }
                            }
                        }
                        if p.data.as_ref() != Some(&d) {
                            p.data = Some(d);
                        }
                    }
                    Err(e) => p.error = Some(e.to_string()),
                }
                // A stage view learns its Argo CD apps from the stage.
                for app in s.stage_apps() {
                    s.apps.entry(app).or_default();
                }
            },
        );
    }

    fn refresh_app(&mut self, name: &str, hard: bool) {
        let Some(a) = self.apps.get_mut(name) else {
            return;
        };
        if a.loading {
            return;
        }
        a.loading = true;
        let name = name.to_string();
        self.bg(
            {
                let name = name.clone();
                move || -> anyhow::Result<(ArgoApp, Vec<Node>)> {
                    let app = if hard {
                        argo::refresh(&name)?
                    } else {
                        argo::app(&name)?
                    };
                    let nodes = argo::tree(&name, &app.namespace)?;
                    Ok((app, nodes))
                }
            },
            move |s, res, tx| {
                let Some(a) = s.apps.get_mut(&name) else {
                    return;
                };
                a.loading = false;
                a.next = tx.clock() + APP_REFRESH;
                match res {
                    Ok((app, nodes)) => {
                        a.error = None;
                        let changed = a.nodes != nodes;
                        a.app = Some(app);
                        a.nodes = nodes;
                        if changed {
                            s.rebuild_rows();
                        }
                        // A resource picked before its app loaded gets its kube context now.
                        if s.selected
                            .as_ref()
                            .is_some_and(|x| x.app == name && x.target.is_err())
                        {
                            let key = s.selected.as_ref().map(|x| x.key.clone());
                            s.select(key.as_deref());
                        }
                    }
                    Err(e) => a.error = Some(e.to_string()),
                }
            },
        );
    }

    fn refresh_all(&mut self) {
        for p in self.store.projects.clone() {
            self.refresh_project(&p);
        }
        for app in self.stage_apps() {
            self.refresh_app(&app, false);
        }
    }

    /// Lays the stage's apps' resource trees out as indented rows: workloads first, then the
    /// rest; ReplicaSets with no pods (old rollouts) only with "All".
    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        for app in self.stage_apps() {
            let Some(state) = self.apps.get(&app) else {
                continue;
            };
            let nodes = &state.nodes;
            let uids: std::collections::HashSet<&str> =
                nodes.iter().map(|n| n.uid.as_str()).collect();
            let mut children: HashMap<&str, Vec<&Node>> = HashMap::new();
            let mut roots = Vec::new();
            for n in nodes {
                let parent = n.parents.iter().find(|p| uids.contains(p.as_str()));
                match parent {
                    Some(p) => children.entry(p.as_str()).or_default().push(n),
                    None => roots.push(n),
                }
            }
            roots.sort_by(|a, b| (kind_rank(&a.kind), &a.name).cmp(&(kind_rank(&b.kind), &b.name)));
            for list in children.values_mut() {
                list.sort_by(|a, b| b.created.cmp(&a.created).then(a.name.cmp(&b.name)));
            }
            fn walk<'a>(
                n: &'a Node,
                depth: usize,
                app: &str,
                all: bool,
                children: &HashMap<&str, Vec<&'a Node>>,
                rows: &mut Vec<ResRow>,
            ) {
                let kids = children.get(n.uid.as_str());
                if !all && n.kind == "ReplicaSet" && kids.is_none_or(|k| k.is_empty()) {
                    return;
                }
                rows.push(ResRow {
                    key: format!("{app}|{}", n.uid),
                    app: app.to_string(),
                    depth,
                    node: n.clone(),
                });
                for c in kids.into_iter().flatten() {
                    walk(c, depth + 1, app, all, children, rows);
                }
            }
            for r in roots {
                if !self.show_all && kind_rank(&r.kind) >= WORKLOAD_KINDS.len() {
                    continue;
                }
                walk(r, 0, &app, self.show_all, &children, &mut rows);
            }
        }
        self.rows = rows;
        if let Some(sel) = &mut self.selected
            && let Some(r) = self.rows.iter().find(|r| r.key == sel.key)
        {
            sel.node = r.node.clone();
        }
    }

    fn target(&self, app: &str, node: &Node) -> Result<kube::Target, String> {
        let a = self
            .apps
            .get(app)
            .and_then(|a| a.app.as_ref())
            .ok_or_else(|| "Loading the Argo CD app…".to_string())?;
        let cluster = if !a.cluster.is_empty() {
            a.cluster.clone()
        } else if let Some(n) = self.clusters.get(&a.server) {
            n.clone()
        } else if a.server == "https://kubernetes.default.svc" {
            "in-cluster".into()
        } else {
            a.server.clone()
        };
        let context = kube::resolve(&cluster, &self.contexts, &self.store.contexts).ok_or_else(|| {
            format!(
                "No kube context matches Argo CD cluster \"{cluster}\". Map it under \"contexts\" in {}.",
                store::data_dir().join("state.json").display()
            )
        })?;
        Ok(kube::Target {
            context,
            namespace: if node.namespace.is_empty() {
                a.dest_namespace.clone()
            } else {
                node.namespace.clone()
            },
            kind: node.kind.clone(),
            group: node.group.clone(),
            name: node.name.clone(),
        })
    }

    /// Selects a resources row (or nothing) and opens its default pane.
    fn select(&mut self, key: Option<&str>) {
        let Some(row) = key.and_then(|k| self.rows.iter().find(|r| r.key == k)) else {
            self.selected = None;
            self.out = Output::default();
            self.table.clear_selection();
            return;
        };
        let (app, node) = (row.app.clone(), row.node.clone());
        let target = self.target(&app, &node);
        let same = self
            .selected
            .as_ref()
            .is_some_and(|s| s.key == row.key && s.target.is_ok() == target.is_ok());
        if same {
            return;
        }
        self.selected = Some(Selected {
            key: row.key.clone(),
            app,
            node,
            target: target.clone(),
        });
        self.containers = None;
        self.container = None;
        self.previous = false;
        self.inspector.set_open(true);
        let Ok(t) = target else {
            self.out = Output::default();
            return;
        };
        if t.has_logs() {
            self.pane = Pane::Logs;
            self.load_containers(t);
        } else {
            self.open_pane(Pane::Describe);
        }
    }

    fn load_containers(&mut self, t: kube::Target) {
        self.out = Output {
            loading: true,
            ..Output::default()
        };
        let key = self.selected.as_ref().map(|s| s.key.clone());
        self.bg(
            move || t.containers(),
            move |s, res, _| {
                if s.selected.as_ref().map(|x| &x.key) != key.as_ref() {
                    return;
                }
                let list = res.unwrap_or_default();
                // Skip service-mesh sidecars by default.
                s.container = list
                    .iter()
                    .find(|c| !SIDECARS.contains(&c.as_str()))
                    .or(list.first())
                    .cloned();
                s.containers = Some(list);
                if s.pane == Pane::Logs {
                    s.open_pane(Pane::Logs);
                }
            },
        );
    }

    fn open_pane(&mut self, pane: Pane) {
        self.pane = pane;
        self.expanded.clear();
        self.expand_all = false;
        self.out_gen += 1;
        self.out = Output {
            loading: true,
            ..Output::default()
        };
        let Some(Ok(t)) = self.selected.as_ref().map(|s| s.target.clone()) else {
            self.out.loading = false;
            return;
        };
        let stream = match pane {
            Pane::Logs => Some(t.logs(self.container.as_deref(), self.previous)),
            Pane::Rollout => Some(t.rollout()),
            _ => None,
        };
        if let Some(st) = stream {
            match st {
                Ok(st) => self.out.stream = Some(st),
                Err(e) => self.set_text(Err(e)),
            }
            return;
        }
        let generation = self.out_gen;
        if pane == Pane::Events {
            self.bg(
                move || t.events(),
                move |s, res, _| {
                    if s.out_gen != generation {
                        return;
                    }
                    s.out.loading = false;
                    match res {
                        Ok(list) => s.out.events = Some(list),
                        Err(e) => s.set_text(Err(e)),
                    }
                },
            );
            return;
        }
        self.bg(
            move || match pane {
                Pane::Describe => t.describe(),
                _ => t.yaml(),
            },
            move |s, res, _| {
                if s.out_gen == generation {
                    if let (Pane::Describe, Ok(text)) = (s.pane, &res) {
                        s.out.describe = format::describe(text);
                    }
                    s.set_text(res);
                }
            },
        );
    }

    fn set_text(&mut self, res: anyhow::Result<String>) {
        self.out.loading = false;
        self.out.lines = match res {
            Ok(t) => t.lines().map(str::to_string).collect(),
            Err(e) => e.to_string().lines().map(|l| format!("! {l}")).collect(),
        };
        self.refilter();
    }

    fn refilter(&mut self) {
        self.filtered_for = self.filter.text.clone();
        let f = self.filter.text.trim().to_lowercase();
        self.out.shown = self
            .out
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| f.is_empty() || l.to_lowercase().contains(&f))
            .map(|(i, _)| i)
            .collect();
        self.out.version += 1;
    }

    /// Moves new stream lines into the view; true when something changed.
    fn drain_stream(&mut self) -> bool {
        let Some(st) = &self.out.stream else {
            return false;
        };
        let (lines, done) = st.take();
        if lines.is_empty() && done == self.out.done {
            return false;
        }
        self.out.loading = false;
        self.out.done = done;
        self.out
            .parsed
            .extend(lines.iter().map(|l| format::log_line(l)));
        self.out.lines.extend(lines);
        if self.out.lines.len() > MAX_LINES {
            let cut = self.out.lines.len() - MAX_LINES;
            self.out.lines.drain(..cut);
            self.out.parsed.drain(..cut);
            self.out.first_id += cut as u64;
        }
        self.refilter();
        true
    }

    fn act(
        &mut self,
        what: &'static str,
        work: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
    ) {
        self.confirm = None;
        self.busy = true;
        self.bg(work, move |s, res, _| {
            s.busy = false;
            match res {
                Ok(()) => s.notify(what, false),
                Err(e) => s.notify(e.to_string(), true),
            }
            if let Some(p) = s.project_name().map(str::to_string)
                && let Some(p) = s.projects.get_mut(&p)
            {
                p.next = 0.0;
            }
            for a in s.stage_apps() {
                if let Some(a) = s.apps.get_mut(&a) {
                    a.next = 0.0;
                }
            }
        });
    }

    fn run_confirm(&mut self) {
        let Some(c) = self.confirm.take() else { return };
        match c {
            Confirm::Promote {
                project,
                stage,
                freight,
            } => {
                self.promote_pick = None;
                self.tab = StageTab::Resources;
                self.act("Promotion started", move || {
                    kargo::promote(&project, &freight, &stage)
                });
            }
            Confirm::Downstream {
                project,
                from,
                freight,
            } => self.act("Promotions started", move || {
                kargo::promote_downstream(&project, &freight, &from)
            }),
            Confirm::Abort { project, promotion } => self.act("Promotion aborted", move || {
                kargo::abort(&project, &promotion)
            }),
            Confirm::Sync(app) => self.act("Sync started", move || argo::sync(&app)),
            Confirm::Restart(t) => {
                let watch = self
                    .selected
                    .as_ref()
                    .is_some_and(|s| s.target.as_ref().ok() == Some(&t));
                self.act("Rollout restarted", move || t.restart());
                if watch {
                    self.open_pane(Pane::Rollout);
                }
            }
            Confirm::Delete(t) => self.act("Pod deleted", move || t.delete()),
            Confirm::InstallApp => self.install_app(),
        }
    }

    fn install_app(&mut self) {
        self.confirm = None;
        self.bg(appbundle::install, |s, res, _| match res {
            Ok(app) => s.notify(
                format!(
                    "Added {}. Open kargoslop from Spotlight or Launchpad.",
                    app.display()
                ),
                false,
            ),
            Err(e) => s.notify(format!("Couldn't add kargoslop.app: {e:#}"), true),
        });
    }

    fn open_add(&mut self) {
        self.add = Some(AddProject {
            edit: TextEdit::new(""),
            available: None,
        });
        self.bg(kargo::list_projects, |s, res, _| {
            if let Some(a) = &mut s.add {
                a.available = Some(res.map_err(|e| e.to_string()));
            }
        });
    }

    fn add_project(&mut self, name: &str) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        self.add = None;
        if !self.store.projects.contains(&name) {
            self.store.projects.push(name.clone());
            self.save();
            self.projects.insert(name.clone(), Project::default());
        }
        self.open(View::Pipeline(name));
    }

    fn remove_project(&mut self, name: &str) {
        self.store.projects.retain(|p| p != name);
        self.projects.remove(name);
        if self.project_name() == Some(name) {
            self.view = None;
            self.select(None);
        }
        self.save();
    }
}

const WORKLOAD_KINDS: &[&str] = &[
    "Rollout",
    "Deployment",
    "StatefulSet",
    "DaemonSet",
    "CronJob",
    "Job",
    "Pod",
];

const SIDECARS: &[&str] = &["istio-proxy", "linkerd-proxy", "istio-init", "linkerd-init"];

fn kind_rank(kind: &str) -> usize {
    WORKLOAD_KINDS
        .iter()
        .position(|k| *k == kind)
        .unwrap_or(WORKLOAD_KINDS.len())
}

fn setup(s: &mut App, waker: Waker) {
    s.waker = Some(waker);
    // Started from a terminal on a Mac without the app installed: offer to add it.
    if appbundle::supported()
        && !appbundle::launched_from_app()
        && !appbundle::installed()
        && !s.store.skip_app_prompt
        && rune::tooling::headless_run().is_none()
    {
        s.confirm = Some(Confirm::InstallApp);
    }
    s.bg(
        || (kube::contexts(), argo::clusters()),
        |s, (ctx, clusters), _| {
            match ctx {
                Ok(c) => s.contexts = c,
                Err(e) => s.notify(e.to_string(), true),
            }
            if let Ok(c) = clusters {
                s.clusters = c;
            }
            let key = s.selected.as_ref().map(|x| x.key.clone());
            if key.is_some() {
                s.selected = None;
                s.select(key.as_deref());
            }
        },
    );
}

fn poll(s: &mut App, now: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    while let Ok(job) = s.jobs.1.try_recv() {
        job(s, tx);
        changed = true;
    }
    for name in s.store.projects.clone() {
        if s.projects
            .get(&name)
            .is_some_and(|p| !p.loading && now >= p.next)
        {
            s.refresh_project(&name);
        }
    }
    for app in s.stage_apps() {
        if s.apps
            .get(&app)
            .is_some_and(|a| !a.loading && now >= a.next)
        {
            s.refresh_app(&app, false);
        }
    }
    changed |= s.drain_stream();
    if s.filter.text != s.filtered_for {
        s.refilter();
        changed = true;
    }
    if s.toast.is_some() {
        match s.toast_until {
            None => {
                let secs = if s.toast.as_ref().is_some_and(|t| t.1) {
                    8.0
                } else {
                    4.0
                };
                s.toast_until = Some(now + secs);
            }
            Some(t) if now >= t => {
                s.toast = None;
                s.toast_until = None;
                changed = true;
            }
            _ => {}
        }
    }
    let live = s.out.stream.is_some() && s.out.done.is_none();
    tx.wake_after(Duration::from_millis(if live { 250 } else { 1000 }));
    changed
}

fn on_key(s: &mut App, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if shell::is_sidebar_toggle(ev) {
        s.sidebar.toggle();
        return true;
    }
    if ev.is_command('r') {
        s.refresh_all();
        return true;
    }
    if ev.is_command('i') {
        s.inspector.toggle();
        return true;
    }
    if ev.key == rune::Key::Escape && s.container_open {
        s.container_open = false;
        return true;
    }
    // k9s-style keys for the selected resource, unless typing or in a dialog.
    let typing =
        s.filter.focused || s.add.is_some() || s.confirm.is_some() || s.promote_pick.is_some();
    if typing || ev.mods.cmd || ev.mods.ctrl {
        return false;
    }
    let Some(Ok(t)) = s.selected.as_ref().map(|x| x.target.clone()) else {
        return false;
    };
    let pane = match &ev.key {
        rune::Key::Text(c) => match c.as_str() {
            "l" if t.has_logs() => Pane::Logs,
            "r" if t.has_rollout() => Pane::Rollout,
            "d" => Pane::Describe,
            "e" => Pane::Events,
            "y" => Pane::Yaml,
            "/" => {
                s.filter.focused = true;
                tx.wake_after(Duration::from_millis(1));
                return true;
            }
            _ => return false,
        },
        _ => return false,
    };
    s.open_pane(pane);
    true
}

fn main() {
    if appbundle::cli(&std::env::args().collect::<Vec<_>>()) {
        return;
    }
    // A bare binary has no bundle icon; this gives the Dock and app switcher the logo.
    let _ = rune::set_app_icon(include_bytes!("../assets/icon.png").to_vec());
    let spec = AppSpec {
        scenes: &[
            ("demo", view::demo),
            ("pipeline", |s: &mut App| {
                view::demo(s);
                s.view = Some(View::Pipeline("shop".into()));
            }),
            ("describe", |s: &mut App| {
                view::demo(s);
                s.pane = Pane::Describe;
                s.out = Output::default();
                s.out.describe = format::describe(view::DESCRIBE);
            }),
            ("events", |s: &mut App| {
                view::demo(s);
                s.pane = Pane::Events;
                let ev = |kind: &str, reason: &str, msg: &str, count| kube::Event {
                    kind: kind.into(),
                    reason: reason.into(),
                    message: msg.into(),
                    count,
                    first: "2026-11-10T20:01:00Z".into(),
                    last: "2026-11-10T20:03:00Z".into(),
                    source: "kubelet".into(),
                    field: "spec.containers{api}".into(),
                };
                s.out = Output {
                    events: Some(vec![
                        ev(
                            "Warning",
                            "Unhealthy",
                            "Readiness probe failed: Get \"http://10.59.12.70:8080/ready\": connect: connection refused",
                            7,
                        ),
                        ev(
                            "Normal",
                            "Pulled",
                            "Container image \"registry/shop:1.4.0\" already present on machine",
                            1,
                        ),
                        ev(
                            "Normal",
                            "Scheduled",
                            "Successfully assigned shop/shop-api-7c9-fghij to gke-staging-1-pool-a",
                            1,
                        ),
                    ]),
                    ..Output::default()
                };
            }),
            ("yaml", |s: &mut App| {
                view::demo(s);
                s.pane = Pane::Yaml;
                s.out = Output::default();
                s.set_text(Ok("apiVersion: v1\nkind: Pod\nmetadata:\n  name: shop-api-7c9-fghij\n  namespace: shop\n  labels:\n    app: shop-api\nspec:\n  containers:\n  - name: api\n    image: registry/shop:1.4.0\n    ports:\n    - containerPort: 8080\n      protocol: TCP\n    resources:\n      limits:\n        cpu: \"2\"\n        memory: 1Gi\nstatus:\n  phase: Pending\n".into()));
            }),
            ("install-app", |s: &mut App| {
                view::demo(s);
                s.confirm = Some(Confirm::InstallApp);
            }),
            ("promote", |s: &mut App| {
                view::demo(s);
                s.promote_pick = Some(("shop".into(), "shop-prod".into()));
            }),
        ],
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        fonts: rune_theme_opencode::FONTS,
        families: rune_theme_opencode::FAMILIES,
        min_size: Some((1000.0, 600.0)),
        ..AppSpec::new("kargoslop", (1600.0, 960.0), init, view::view)
    };
    let chrome = Chrome {
        full_size_content: true,
        traffic_lights: Some((16.0, 25.0)),
        drag_regions: true,
    };
    rune::App::new(spec).chrome(chrome).modifier_keys().run();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Follows a real stage down to a pod's logs:
    /// `KARGO_PROJECT=my-project cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn stage_to_pod_logs() {
        let project = std::env::var("KARGO_PROJECT").expect("set KARGO_PROJECT to a Kargo project");
        let mut s = init();
        s.contexts = kube::contexts().unwrap();
        s.clusters = argo::clusters().unwrap();
        let data = kargo::load_project(&project).unwrap();
        let stage = data.stages[0].clone();
        s.projects.insert(
            project.clone(),
            Project {
                data: Some(data),
                ..Project::default()
            },
        );
        s.view = Some(View::Stage(project, stage.name.clone()));
        let app = &stage.apps[0].1;
        let a = argo::app(app).unwrap();
        let nodes = argo::tree(app, &a.namespace).unwrap();
        s.apps.insert(
            app.clone(),
            AppState {
                app: Some(a),
                nodes,
                ..AppState::default()
            },
        );
        s.rebuild_rows();
        let pod = s.rows.iter().find(|r| r.node.kind == "Pod").expect("a pod");
        let t = s.target(&pod.app, &pod.node).unwrap();
        let described = format::describe(&t.describe().unwrap());
        assert!(
            described
                .iter()
                .any(|e| e.key == "Name" && e.value == t.name)
        );
        assert!(
            described
                .iter()
                .any(|e| e.key == "Containers" && !e.is_field())
        );
        let events = t.events().unwrap();
        println!(
            "{} events, {} describe entries",
            events.len(),
            described.len()
        );
        let containers = t.containers().unwrap();
        let c = containers
            .iter()
            .find(|c| !SIDECARS.contains(&c.as_str()))
            .or(containers.first());
        let stream = t.logs(c.map(String::as_str), false).unwrap();
        std::thread::sleep(Duration::from_secs(4));
        let (lines, _) = stream.take();
        assert!(!lines.is_empty(), "no log lines from {}", t.resource());
        println!(
            "{} via {}: {} lines, first: {}",
            t.resource(),
            t.context,
            lines.len(),
            lines[0]
        );
    }
}
