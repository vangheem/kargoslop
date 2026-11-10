//! The window: sidebar of projects and stages, toolbar, dialogs and shared bits.

use rune::{Color, Cx, El, TextEdit, Tx, col, el, row, text};
use rune_kit::shell::{
    self, Sidebar, SidebarRow, SidebarSection, SidebarToggle, SplitView, Toolbar,
};
use rune_kit::{
    Button, ButtonVariant, ControlSize, Dialog, IconButton, IconButtonVariant, Kit, Spinner,
    TextInput,
};

use crate::argo::{ArgoApp, Node};
use crate::kargo::{
    self, Freight, FreightRef, HistoryEntry, ProjectData, Promotion, Requested, Stage, Step,
    Warehouse,
};
use crate::{App, AppState, Confirm, Project, View, inspector, pipeline, stage};

pub fn view(s: &App, cx: &mut Cx<App>) -> El {
    let k = rune_theme_opencode::kit(cx.system_dark()).clone();
    let mut layer = Vec::new();
    let main = match &s.view {
        Some(View::Pipeline(p)) => pipeline::view(s, &k, p),
        Some(View::Stage(..)) => stage::view(s, &k),
        None => empty(
            &k,
            if s.projects.is_empty() {
                "Add a Kargo project to start."
            } else {
                "Pick a project's pipeline or one of its stages."
            },
        ),
    };
    let title = match &s.view {
        Some(View::Pipeline(p)) => format!("{p} · pipeline"),
        Some(View::Stage(p, _)) => p.clone(),
        None => "kargoslop".into(),
    };
    let toolbar = Toolbar::new()
        .leading(
            text(title)
                .font(k.ty.ui_medium)
                .color(k.c.text_base)
                .truncate(),
        )
        .trailing(toolbar_actions(s, &k));
    let split = SplitView::new(main)
        .sidebar(&s.sidebar, |s: &mut App| &mut s.sidebar, sidebar(s, &k))
        .inspector(
            &s.inspector,
            |s: &mut App| &mut s.inspector,
            inspector::view(s, &k, &mut layer),
        )
        .toolbar(toolbar)
        .titlebar_inset(cx.titlebar_inset())
        .frame_inset(8.0)
        .toolbar_in_panel()
        .sidebar_toggle(SidebarToggle::new().in_header())
        .reduced_motion(cx.is_reduced_motion())
        .build(&k);
    dialogs(s, &k, &mut layer);
    let mut root = col().w_full().h_full().child(split);
    if let Some((msg, err)) = &s.toast {
        root = root.child(toast(&k, msg, *err));
    }
    rune_kit::root(root, layer)
}

pub fn empty(k: &Kit, msg: &str) -> El {
    col().w_full().h_full().center().padding(24.0).child(
        text(msg.to_string())
            .font(k.ty.body)
            .color(k.c.text_muted)
            .wrap(),
    )
}

pub fn error(k: &Kit, msg: &str) -> El {
    text(msg.to_string())
        .font(k.ty.small)
        .color(k.c.danger.fg)
        .wrap()
        .selectable(true)
}

/// Health words from Kargo, Argo CD and pod phases, as a color.
pub fn health_color(k: &Kit, health: &str) -> Color {
    match health {
        "Healthy" | "Synced" | "Succeeded" | "Running" | "Successful" | "Completed" | "True" => {
            k.c.success.icon
        }
        "Progressing" | "Pending" | "OutOfSync" | "ContainerCreating" | "Suspended" => {
            k.c.warning.icon
        }
        "Degraded" | "Unhealthy" | "Failed" | "Errored" | "Error" | "Missing" | "Aborted"
        | "CrashLoopBackOff" | "False" | "Failure" => k.c.danger.icon,
        _ => k.c.icon_faint,
    }
}

pub fn dot(k: &Kit, health: &str) -> El {
    el().size(8.0, 8.0)
        .radius(4.0)
        .fill(health_color(k, health))
}

/// A dot and a word.
pub fn chip(k: &Kit, health: &str, label: impl Into<String>) -> El {
    let label = label.into();
    row().gap(5.0).items_center().child(dot(k, health)).child(
        text(if label.is_empty() {
            "unknown".into()
        } else {
            label
        })
        .font(k.ty.small_medium)
        .color(k.c.text_base),
    )
}

fn toast(k: &Kit, msg: &str, error: bool) -> El {
    let c = if error { &k.c.danger } else { &k.c.success };
    let card = row()
        .key("toast")
        .max_w(460.0)
        .px(14.0)
        .py(10.0)
        .gap(10.0)
        .items_center()
        .radius(8.0)
        .fill(k.c.layer_01)
        .border(1.0, c.border)
        .child(el().size(8.0, 8.0).radius(4.0).fill(c.icon))
        .child(
            text(msg.to_string())
                .font(k.ty.ui)
                .color(k.c.text_base)
                .wrap()
                .shrink(),
        )
        .pin(Some(16.0), None, None, Some(16.0))
        .on_press(|s: &mut App| s.toast = None);
    k.elevate(card, &k.elevation.overlay)
}

pub fn small_button(k: &Kit, label: &str, variant: ButtonVariant) -> Button {
    let _ = k;
    Button::new(label.to_string())
        .variant(variant)
        .control_size(ControlSize::Small)
}

fn toolbar_actions(s: &App, k: &Kit) -> El {
    let mut r = row().gap(6.0).items_center();
    let loading = s.busy
        || s.project_name()
            .and_then(|p| s.projects.get(p))
            .is_some_and(|p| p.loading && p.data.is_none());
    if loading {
        r = r.child(Spinner::new().build(k));
    }
    if let Some(p) = s.project_name()
        && let Some(base) = kargo::ui_url()
    {
        let url = match s.stage() {
            Some((_, st)) => format!("{base}/project/{p}/stage/{}", st.name),
            None => format!("{base}/project/{p}"),
        };
        r = r.child(link_button(k, "Kargo ↗", url));
    }
    let open = s.inspector.is_open();
    r.child(
        small_button(k, "Refresh", ButtonVariant::Ghost)
            .shortcut("⌘R")
            .on_press(|s: &mut App| s.refresh_all())
            .build(k),
    )
    .child(
        IconButton::new(&shell::INSPECTOR)
            .variant(IconButtonVariant::GhostMuted)
            .expanded(open)
            .label(if open { "Hide panel" } else { "Show panel" })
            .shortcut("⌘I")
            .on_press(|s: &mut App| s.inspector.toggle())
            .build(k)
            .key("inspector-toggle"),
    )
}

pub fn link_button(k: &Kit, label: &str, url: String) -> El {
    small_button(k, label, ButtonVariant::Ghost)
        .on_press_tx(move |_: &mut App, tx: &mut Tx| {
            let _ = tx.open_url(&url);
        })
        .build(k)
}

fn health_glyph(health: &str, promoting: bool) -> &'static str {
    if promoting {
        return "⟳";
    }
    match health {
        "Healthy" => "✓",
        "Progressing" => "…",
        "Unhealthy" | "Degraded" => "✗",
        _ => "",
    }
}

fn sidebar(s: &App, k: &Kit) -> El {
    let mut sb = Sidebar::new().label("Kargo projects").header(
        SidebarRow::new("Add project")
            .icon(&rune_kit::glyphs::SPARKLE)
            .height(32.0)
            .on_select(|s: &mut App, _| s.open_add())
            .build(k)
            .key("add-project"),
    );
    for (name, p) in &s.projects {
        sb = sb.section(project_section(s, k, name, p));
    }
    sb.build(k)
}

fn project_section(s: &App, k: &Kit, name: &str, p: &Project) -> SidebarSection {
    let mut rows = Vec::new();
    let pipe = View::Pipeline(name.to_string());
    let v = pipe.clone();
    rows.push(
        SidebarRow::new("Pipeline")
            .height(30.0)
            .selected(s.view.as_ref() == Some(&pipe))
            .on_select(move |s: &mut App, _| s.open(v.clone()))
            .build(k)
            .key(format!("{name}/pipeline")),
    );
    if p.loading && p.data.is_none() {
        rows.push(
            SidebarRow::new("Loading…")
                .select_only()
                .build(k)
                .key(format!("{name}/loading")),
        );
    }
    if let Some(e) = &p.error {
        rows.push(error(k, e).key(format!("{name}/err")));
    }
    if let Some(d) = &p.data {
        let mut seen = std::collections::HashSet::new();
        for w in &d.warehouses {
            for st in d.pipeline(&w.name) {
                if seen.insert(st.name.clone()) {
                    rows.push(stage_row(s, k, name, d, st));
                }
            }
        }
        for st in &d.stages {
            if seen.insert(st.name.clone()) {
                rows.push(stage_row(s, k, name, d, st));
            }
        }
    }
    let (n1, n2) = (name.to_string(), name.to_string());
    SidebarSection::new(name.to_string())
        .key(name.to_string())
        .count(p.data.as_ref().map_or(0, |d| d.stages.len()))
        .collapsed(p.collapsed)
        .on_toggle(move |s: &mut App, _| {
            if let Some(p) = s.projects.get_mut(&n1) {
                p.collapsed ^= true;
            }
        })
        .action(
            small_button(k, "Remove", ButtonVariant::GhostFaint)
                .on_press(move |s: &mut App| s.remove_project(&n2))
                .build(k),
        )
        .rows(rows)
}

fn stage_row(s: &App, k: &Kit, project: &str, d: &ProjectData, st: &Stage) -> El {
    let view = View::Stage(project.to_string(), st.name.clone());
    let promoting = d
        .promotions
        .iter()
        .any(|p| p.stage == st.name && p.running());
    let version = st
        .current()
        .iter()
        .filter_map(|f| f.artifacts.first())
        .map(|a| a.rsplit(':').next().unwrap_or(a).to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let health = if st.health.is_empty() {
        "unknown"
    } else {
        st.health.as_str()
    };
    let subtitle = if promoting {
        format!("{version} · promoting")
    } else {
        format!("{version} · {health}")
    };
    let v = view.clone();
    let mut r = SidebarRow::new(st.name.clone())
        .subtitle(subtitle)
        .height(44.0)
        .selected(s.view.as_ref() == Some(&view))
        .on_select(move |s: &mut App, _| s.open(v.clone()));
    let g = health_glyph(&st.health, promoting);
    if !g.is_empty() {
        r = r.badge(g);
    }
    r.build(k).key(format!("{project}/stage/{}", st.name))
}

/// A dialog sized to its content, with a padded body and right-aligned actions.
#[allow(clippy::too_many_arguments)]
pub fn modal(
    key: &str,
    title: &str,
    description: Option<&str>,
    body: Vec<El>,
    actions: Vec<El>,
    k: &Kit,
    layer: &mut Vec<El>,
    width: f32,
) {
    let mut footer = row().w_full().gap(8.0).pt(4.0).child(row().grow(1.0));
    for a in actions {
        footer = footer.child(a);
    }
    let mut content = col().w_full().px(16.0).pb(16.0).gap(12.0);
    if let Some(desc) = description {
        content = content.child(
            text(desc.to_string())
                .font(k.ty.ui)
                .color(k.c.text_muted)
                .wrap()
                .mt(-6.0),
        );
    }
    let content = content.children(body).child(footer);
    Dialog::new(key, title)
        .open(true)
        .fit()
        .width(width)
        .child(content)
        .on_close(close_dialogs)
        .build(k, layer);
}

fn close_dialogs(s: &mut App) {
    s.add = None;
    s.confirm = None;
    s.promote_pick = None;
}

pub fn cancel(k: &Kit) -> El {
    Button::new("Cancel")
        .variant(ButtonVariant::Ghost)
        .on_press(close_dialogs)
        .build(k)
}

fn dialogs(s: &App, k: &Kit, layer: &mut Vec<El>) {
    if let Some(a) = &s.add {
        let input = TextInput::new(&a.edit)
            .placeholder("Project name")
            .width(None)
            .on_edit(|s: &mut App| &mut s.add.as_mut().expect("dialog open").edit)
            .on_submit(|s: &mut App, _| {
                let name = s
                    .add
                    .as_ref()
                    .map(|a| a.edit.text.clone())
                    .unwrap_or_default();
                s.add_project(&name);
            })
            .build(k);
        let mut list = col().w_full().gap(2.0);
        match &a.available {
            None => list = list.child(Spinner::new().build(k)),
            Some(Err(e)) => list = list.child(error(k, e)),
            Some(Ok(names)) => {
                let typed = a.edit.text.trim().to_lowercase();
                for n in names
                    .iter()
                    .filter(|n| !s.store.projects.contains(n) && n.to_lowercase().contains(&typed))
                {
                    let name = n.clone();
                    let item = row()
                        .w_full()
                        .px(10.0)
                        .py(7.0)
                        .radius(6.0)
                        .child(text(n.clone()).font(k.ty.ui).color(k.c.text_base))
                        .on_press(move |s: &mut App| s.add_project(&name));
                    list = list.child(k.interactive(item).key(format!("proj/{n}")));
                }
            }
        }
        let add = Button::new("Add")
            .variant(ButtonVariant::Contrast)
            .on_press(|s: &mut App| {
                let name = s
                    .add
                    .as_ref()
                    .map(|a| a.edit.text.clone())
                    .unwrap_or_default();
                s.add_project(&name);
            })
            .build(k);
        let list = col()
            .w_full()
            .max_h(320.0)
            .scroll(rune::ScrollSpec {
                stick_bottom: false,
            })
            .child(list);
        modal(
            "add-project",
            "Add Kargo project",
            Some("Projects you can see with the kargo CLI."),
            vec![input, list],
            vec![cancel(k), add],
            k,
            layer,
            460.0,
        );
    }
    if s.confirm.is_none()
        && let Some((project, stage)) = &s.promote_pick
    {
        stage::promote_dialog(s, k, layer, project, stage);
    }
    let Some(c) = &s.confirm else { return };
    if let Confirm::InstallApp = c {
        let never = Button::new("Don't ask again")
            .variant(ButtonVariant::Ghost)
            .on_press(|s: &mut App| {
                s.confirm = None;
                s.store.skip_app_prompt = true;
                s.save();
                s.notify("You can add it later with `kargoslop install-app`", false);
            })
            .build(k);
        let later = Button::new("Not now")
            .variant(ButtonVariant::Ghost)
            .on_press(|s: &mut App| s.confirm = None)
            .build(k);
        let add = Button::new("Add to Applications")
            .variant(ButtonVariant::Contrast)
            .on_press(|s: &mut App| s.install_app())
            .build(k);
        let detail = text(
            "Creates ~/Applications/kargoslop.app so you can open kargoslop from Spotlight, Launchpad or the Dock. \
             It runs the kargoslop you already have installed, so `brew upgrade` keeps it current. \
             Same as running `kargoslop install-app`.",
        )
        .font(k.ty.ui)
        .color(k.c.text_muted)
        .wrap();
        modal(
            "install-app",
            "Add kargoslop to your Applications?",
            None,
            vec![detail],
            vec![never, later, add],
            k,
            layer,
            460.0,
        );
        return;
    }
    let (title, desc): (&str, String) = match c {
        Confirm::Promote {
            project,
            stage,
            freight,
        } => (
            "Promote freight?",
            format!(
                "Promote {} to {stage}.",
                freight_label(s.data(project), freight)
            ),
        ),
        Confirm::Downstream {
            project,
            from,
            freight,
        } => {
            let names: Vec<String> = s
                .data(project)
                .map(|d| {
                    d.downstream(from)
                        .iter()
                        .map(|st| st.name.clone())
                        .collect()
                })
                .unwrap_or_default();
            (
                "Promote downstream?",
                format!(
                    "Promote {} from {from} to {}.",
                    freight_label(s.data(project), freight),
                    names.join(", ")
                ),
            )
        }
        Confirm::Abort { promotion, .. } => ("Abort promotion?", format!("Abort {promotion}.")),
        Confirm::Sync(app) => (
            "Sync application?",
            format!("Ask Argo CD to sync {app} now."),
        ),
        Confirm::Restart(t) => (
            "Restart rollout?",
            format!(
                "kubectl rollout restart {} in {} ({}).",
                t.resource(),
                t.namespace,
                t.context
            ),
        ),
        Confirm::Delete(t) => (
            "Delete pod?",
            format!(
                "kubectl delete {} in {} ({}). Its controller replaces it.",
                t.resource(),
                t.namespace,
                t.context
            ),
        ),
        Confirm::InstallApp => return,
    };
    let danger = matches!(c, Confirm::Delete(_) | Confirm::Abort { .. });
    let ok = Button::new("Confirm")
        .variant(if danger {
            ButtonVariant::Danger
        } else {
            ButtonVariant::Contrast
        })
        .on_press(|s: &mut App| s.run_confirm())
        .build(k);
    modal(
        "confirm",
        title,
        Some(&desc),
        vec![],
        vec![cancel(k), ok],
        k,
        layer,
        480.0,
    );
}

pub fn freight_label(d: Option<&ProjectData>, name: &str) -> String {
    match d.and_then(|d| d.freight(name)) {
        Some(f) => format!("{} ({})", f.alias, f.artifacts.join(", ")),
        None => name.get(..12).unwrap_or(name).to_string(),
    }
}

/// Headless-only sample data (`--scene demo`), for checking the layout without clusters.
pub fn demo(s: &mut App) {
    let fr = |name: &str, alias: &str, tag: &str, created: &str, verified: &[&str]| Freight {
        name: name.into(),
        alias: alias.into(),
        warehouse: "shop".into(),
        created: created.into(),
        artifacts: vec![format!("shop:{tag}")],
        source: "https://github.com/acme/shop".into(),
        verified_in: verified.iter().map(|v| v.to_string()).collect(),
        approved_for: vec![],
    };
    let freight = vec![
        fr(
            "f3",
            "brave-otter",
            "1.4.0",
            "2026-11-10T20:00:00Z",
            &["shop-stage"],
        ),
        fr(
            "f2",
            "quiet-lynx",
            "1.3.2",
            "2026-11-09T12:00:00Z",
            &["shop-stage", "shop-prod"],
        ),
        fr(
            "f1",
            "mellow-crab",
            "1.3.1",
            "2026-11-08T09:00:00Z",
            &["shop-stage", "shop-prod"],
        ),
    ];
    let cur = |f: &Freight| HistoryEntry {
        items: vec![FreightRef {
            name: f.name.clone(),
            warehouse: "shop".into(),
            artifacts: f.artifacts.clone(),
        }],
        verification: Some(("Successful".into(), "2026-11-10T20:05:00Z".into())),
    };
    let st = |name: &str, direct: bool, up: &[&str], f: &Freight, health: &str| Stage {
        name: name.into(),
        ready: Some(true),
        message: "Freight has been verified".into(),
        health: health.into(),
        requested: vec![Requested {
            warehouse: "shop".into(),
            direct,
            upstream: up.iter().map(|u| u.to_string()).collect(),
        }],
        auto_promote: direct,
        apps: vec![("argocd".into(), name.into())],
        history: vec![cur(f)],
        current_promotion: None,
        vars: vec![("cluster".into(), "gke-staging-1".into())],
    };
    let stages = vec![
        st("shop-prod", false, &["shop-stage"], &freight[1], "Healthy"),
        st("shop-stage", true, &[], &freight[0], "Progressing"),
    ];
    let step = |a: &str, status: &str| Step {
        alias: a.into(),
        status: status.into(),
        message: String::new(),
        started: "2026-11-10T20:01:00Z".into(),
        finished: "2026-11-10T20:01:05Z".into(),
    };
    let promotions = vec![Promotion {
        name: "shop-stage.01abc.f3".into(),
        stage: "shop-stage".into(),
        freight: "f3".into(),
        phase: "Running".into(),
        message: String::new(),
        created: "2026-11-10T20:01:00Z".into(),
        started: "2026-11-10T20:01:00Z".into(),
        finished: String::new(),
        total_steps: 4,
        steps: vec![
            step("checkout", "Succeeded"),
            step("commit", "Succeeded"),
            step("argocd-update", "Running"),
        ],
    }];
    let data = ProjectData {
        warehouses: vec![Warehouse {
            name: "shop".into(),
            ready: Some(true),
            message: "Discovered artifacts from 1 subscriptions".into(),
            interval: "5m0s".into(),
            subscriptions: vec!["image shop".into()],
        }],
        stages,
        freight,
        promotions,
    };
    s.store.projects = vec!["shop".into()];
    s.projects.clear();
    s.projects.insert(
        "shop".into(),
        Project {
            data: Some(data),
            next: f64::MAX,
            ..Project::default()
        },
    );
    let node = |uid: &str,
                kind: &str,
                name: &str,
                parent: Option<&str>,
                health: &str,
                info: &[(&str, &str)]| Node {
        uid: uid.into(),
        kind: kind.into(),
        group: if kind == "Pod" {
            String::new()
        } else {
            "apps".into()
        },
        name: name.into(),
        namespace: "shop".into(),
        parents: parent.map(|p| vec![p.to_string()]).unwrap_or_default(),
        health: health.into(),
        health_message: String::new(),
        info: info
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
        images: vec!["registry/shop:1.4.0".into()],
        created: "2026-11-10T20:02:00Z".into(),
    };
    let nodes = vec![
        node(
            "d1",
            "Deployment",
            "shop-api",
            None,
            "Progressing",
            &[("Revision", "Rev:12")],
        ),
        node(
            "r1",
            "ReplicaSet",
            "shop-api-7c9",
            Some("d1"),
            "Progressing",
            &[],
        ),
        node(
            "p1",
            "Pod",
            "shop-api-7c9-abcde",
            Some("r1"),
            "Healthy",
            &[("Status Reason", "Running"), ("Containers", "2/2")],
        ),
        node(
            "p2",
            "Pod",
            "shop-api-7c9-fghij",
            Some("r1"),
            "Progressing",
            &[
                ("Status Reason", "ContainerCreating"),
                ("Containers", "0/2"),
            ],
        ),
        node(
            "d2",
            "Deployment",
            "shop-worker",
            None,
            "Healthy",
            &[("Revision", "Rev:8")],
        ),
        node(
            "r2",
            "ReplicaSet",
            "shop-worker-55f",
            Some("d2"),
            "Healthy",
            &[],
        ),
        node(
            "p3",
            "Pod",
            "shop-worker-55f-xyz12",
            Some("r2"),
            "Healthy",
            &[
                ("Status Reason", "Running"),
                ("Containers", "2/2"),
                ("Restart Count", "3"),
            ],
        ),
    ];
    for name in ["shop-stage", "shop-prod"] {
        s.apps.insert(
            name.into(),
            AppState {
                app: Some(ArgoApp {
                    name: name.into(),
                    namespace: "argocd".into(),
                    project: "shop".into(),
                    cluster: "gke-staging-1".into(),
                    dest_namespace: "shop".into(),
                    sync: "Synced".into(),
                    health: "Progressing".into(),
                    revisions: vec!["1.4.0".into()],
                    operation: Some((
                        "Succeeded".into(),
                        "successfully synced (all tasks run)".into(),
                        "2026-11-10T20:02:00Z".into(),
                    )),
                    sources: vec!["shop @ 1.4.0".into()],
                    ..ArgoApp::default()
                }),
                nodes: nodes.clone(),
                next: f64::MAX,
                ..AppState::default()
            },
        );
    }
    s.contexts = vec!["gke_acme_us-central1_gke-staging-1".into()];
    s.view = Some(View::Stage("shop".into(), "shop-stage".into()));
    s.rebuild_rows();
    s.selected = Some(crate::Selected {
        key: "shop-stage|p2".into(),
        app: "shop-stage".into(),
        node: nodes[3].clone(),
        target: s.target("shop-stage", &nodes[3]),
    });
    s.table.select("shop-stage|p2");
    s.containers = Some(vec!["api".into(), "istio-proxy".into()]);
    s.container = Some("api".into());
    let lines: Vec<String> = (0..30)
        .map(|i| match i % 7 {
            3 => format!("{{\"time\":\"2026-11-10T20:03:{:02}.512Z\",\"severity\":\"ERROR\",\"message\":\"upstream timed out\",\"path\":\"/v1/search\",\"status\":504,\"duration_ms\":30012,\"trace\":{{\"id\":\"9f2c\",\"span\":\"a1\"}}}}", i),
            5 => format!("{{\"time\":\"2026-11-10T20:03:{:02}.001Z\",\"severity\":\"WARNING\",\"message\":\"slow query\",\"ms\":812}}", i),
            6 => "plain text line: starting worker pool size=8".to_string(),
            _ => format!("{{\"time\":\"2026-11-10T20:03:{:02}.120Z\",\"severity\":\"INFO\",\"message\":\"GET /health 200\",\"remoteIp\":\"127.0.0.6\"}}", i),
        })
        .collect();
    s.out.parsed = lines.iter().map(|l| crate::format::log_line(l)).collect();
    s.out.lines = lines;
    s.expanded.insert(3);
    s.refilter();
    s.filter = TextEdit::new("");
}

pub const DESCRIBE: &str = "Name:             shop-api-7c9-fghij
Namespace:        shop
Node:             gke-staging-1-pool-a/10.59.128.75
Start Time:       Tue, 10 Nov 2026 17:45:59 -0500
Labels:           app=shop-api
                  pod-template-hash=7c9
                  version=1.4.0
Annotations:      istio.io/rev: default
                  kubectl.kubernetes.io/default-container: api
Status:           Pending
IP:               10.59.12.70
Controlled By:    ReplicaSet/shop-api-7c9
Containers:
  api:
    Image:          registry/shop:1.4.0
    Port:           8080/TCP
    State:          Waiting
      Reason:       ContainerCreating
    Ready:          False
    Restart Count:  0
    Limits:
      cpu:     2
      memory:  1Gi
  istio-proxy:
    Image:          proxyv2:1.30.5
    State:          Running
      Started:      Tue, 10 Nov 2026 17:46:01 -0500
    Ready:          True
Conditions:
  Type                        Status
  PodReadyToStartContainers   True
  Initialized                 True
  Ready                       False
Volumes:
  kube-api-access:
    Type:  Projected
QoS Class:        Burstable
Events:
  Type     Reason     Age   From     Message
  ----     ------     ----  ----     -------
  Normal   Scheduled  60s   default  Successfully assigned shop/shop-api-7c9-fghij
  Warning  Unhealthy  20s   kubelet  Readiness probe failed: connection refused
";
