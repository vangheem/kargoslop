//! One stage: what it runs, its Argo CD app, an in-flight promotion, and tabs for the live
//! resources, promotion history and freight history.

use rune::{El, ScrollSpec, col, row, text};
use rune_kit::{
    Badge, BadgeVariant, ButtonVariant, Cell, Kit, SegmentedControl, Select, Spinner, Table,
    TableColumn,
};

use crate::argo::ArgoApp;
use crate::cli::{age, took};
use crate::kargo::{ProjectData, Promotion, Stage};
use crate::kube;
use crate::view::{
    chip, dot, empty, error, freight_label, health_color, link_button, modal, small_button,
};
use crate::{App, Confirm, StageTab};

pub fn view(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
    let Some((project, st)) = s.stage() else {
        return empty(k, "Loading stage…");
    };
    let Some(d) = s.data(project) else {
        return empty(k, "Loading stage…");
    };
    let mut c = col()
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(header(s, k, project, d, st));
    if let Some(p) = d
        .promotions
        .iter()
        .find(|p| p.stage == st.name && p.running())
    {
        c = c.child(running(k, project, d, p));
    }
    let labels = [
        format!("Resources ({})", s.rows.len()),
        format!(
            "Promotions ({})",
            d.promotions.iter().filter(|p| p.stage == st.name).count()
        ),
        "History".to_string(),
    ];
    let current = match s.tab {
        StageTab::Resources => 0,
        StageTab::Promotions => 1,
        StageTab::History => 2,
    };
    let tabs = SegmentedControl::new(&labels, Some(current))
        .width(Some(420.0))
        .on_pick(|s: &mut App, i, _| {
            s.tab = [StageTab::Resources, StageTab::Promotions, StageTab::History][i.min(2)]
        })
        .build(k);
    let mut bar = row()
        .w_full()
        .px(16.0)
        .py(8.0)
        .gap(10.0)
        .items_center()
        .child(tabs)
        .child(row().grow(1.0));
    if s.tab == StageTab::Resources {
        let mut options = vec![ALL_KINDS.to_string()];
        options.extend(s.kinds());
        let current = s.kind.clone().unwrap_or_else(|| ALL_KINDS.into());
        bar = bar.child(
            Select::new("kind", &options)
                .current(Some(&current))
                .open(s.kind_open)
                .width(170.0)
                .on_toggle(|s: &mut App| s.kind_open ^= true)
                .on_close(|s: &mut App| s.kind_open = false)
                .on_choose(|s: &mut App, v: &str, _| {
                    s.kind_open = false;
                    s.kind = (v != ALL_KINDS).then(|| v.to_string());
                    s.rebuild_rows();
                })
                .build(k, layer),
        );
        if s.kind.is_none() {
            bar = bar.child(
                SegmentedControl::new(&["Workloads", "All"], Some(s.show_all as usize))
                    .width(Some(200.0))
                    .on_pick(|s: &mut App, i, _| {
                        s.show_all = i == 1;
                        s.rebuild_rows();
                    })
                    .build(k),
            );
        }
    }
    let body = match s.tab {
        StageTab::Resources => resources(s, k, st),
        StageTab::Promotions => promotions(k, project, d, st),
        StageTab::History => history(k, d, st),
    };
    c.child(bar)
        .child(col().w_full().grow(1.0).min_h(0.0).child(body))
}

fn actions(k: &Kit, project: &str, d: &ProjectData, st: &Stage) -> El {
    let mut r = row().gap(6.0).items_center();
    let (p1, s1) = (project.to_string(), st.name.clone());
    r = r.child(
        small_button(k, "Promote…", ButtonVariant::Contrast)
            .on_press(move |s: &mut App| s.promote_pick = Some((p1.clone(), s1.clone())))
            .build(k),
    );
    let downstream = d.downstream(&st.name).len();
    if downstream > 0
        && let Some(cur) = st.current().first()
    {
        let (p, from, freight) = (project.to_string(), st.name.clone(), cur.name.clone());
        r = r.child(
            small_button(
                k,
                &format!("Promote downstream ({downstream})"),
                ButtonVariant::Outline,
            )
            .on_press(move |s: &mut App| {
                s.confirm = Some(Confirm::Downstream {
                    project: p.clone(),
                    from: from.clone(),
                    freight: freight.clone(),
                })
            })
            .build(k),
        );
    }
    for (_, app) in &st.apps {
        let a = app.clone();
        r = r.child(
            small_button(k, "Sync", ButtonVariant::Ghost)
                .on_press(move |s: &mut App| s.confirm = Some(Confirm::Sync(a.clone())))
                .build(k),
        );
        let a = app.clone();
        r = r.child(
            small_button(k, "Hard refresh", ButtonVariant::Ghost)
                .on_press(move |s: &mut App| s.refresh_app(&a, true))
                .build(k),
        );
        if let Some(url) = crate::argo::ui_url(app) {
            r = r.child(link_button(k, "Argo CD ↗", url));
        }
    }
    r
}

fn header(s: &App, k: &Kit, project: &str, d: &ProjectData, st: &Stage) -> El {
    let title = row()
        .w_full()
        .gap(10.0)
        .items_center()
        .child(
            text(st.name.clone())
                .font(k.ty.body_medium)
                .color(k.c.text_base)
                .truncate()
                .grow(1.0),
        )
        .child(actions(k, project, d, st));
    let mut meta = row().gap(10.0).items_center().flex_wrap().child(chip(
        k,
        &st.health,
        format!(
            "Kargo {}",
            if st.health.is_empty() {
                "unknown"
            } else {
                &st.health
            }
        ),
    ));
    if let Some(ready) = st.ready {
        meta = meta.child(
            text(st.message.clone())
                .font(k.ty.small)
                .color(if ready { k.c.text_muted } else { k.c.danger.fg })
                .truncate(),
        );
    }
    if st.auto_promote {
        meta = meta.child(
            Badge::new("auto-promote")
                .variant(BadgeVariant::Subtle)
                .build(k),
        );
    }
    let upstream: Vec<String> = st
        .requested
        .iter()
        .map(|r| {
            if r.direct {
                format!("from warehouse {}", r.warehouse)
            } else {
                format!("from {}", r.upstream.join(", "))
            }
        })
        .collect();
    meta = meta.child(
        text(upstream.join(" · "))
            .font(k.ty.small)
            .color(k.c.text_faint),
    );
    let mut running_now = col().gap(2.0);
    for f in st.current() {
        let alias = d
            .freight(&f.name)
            .map_or(String::new(), |x| x.alias.clone());
        running_now = running_now.child(
            row()
                .gap(8.0)
                .items_center()
                .child(
                    text("Running")
                        .font(k.ty.small_medium)
                        .color(k.c.text_muted),
                )
                .child(text(alias).font(k.ty.ui_medium).color(k.c.text_base))
                .child(
                    text(f.artifacts.join(", "))
                        .font(k.ty.mono)
                        .color(k.c.text_base)
                        .selectable(true),
                ),
        );
    }
    let mut h = col()
        .w_full()
        .px(16.0)
        .py(12.0)
        .gap(8.0)
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted)
        .child(title)
        .child(meta)
        .child(running_now);
    for (_, name) in &st.apps {
        let state = s.apps.get(name);
        match state.and_then(|a| a.app.as_ref()) {
            Some(a) => h = h.child(app_line(s, k, a)),
            None => {
                h = h.child(
                    row()
                        .gap(8.0)
                        .items_center()
                        .child(Spinner::new().build(k))
                        .child(
                            text(format!("Argo CD {name}"))
                                .font(k.ty.small)
                                .color(k.c.text_muted),
                        ),
                )
            }
        }
        if let Some(e) = state.and_then(|a| a.error.as_ref()) {
            h = h.child(error(k, e));
        }
    }
    h
}

fn app_line(s: &App, k: &Kit, a: &ArgoApp) -> El {
    let cluster = if a.cluster.is_empty() {
        s.clusters
            .get(&a.server)
            .cloned()
            .unwrap_or_else(|| a.server.clone())
    } else {
        a.cluster.clone()
    };
    let mut r = row()
        .gap(10.0)
        .items_center()
        .flex_wrap()
        .child(
            text(format!("Argo CD {}", a.name))
                .font(k.ty.small_medium)
                .color(k.c.text_base),
        )
        .child(chip(k, &a.sync, a.sync.clone()))
        .child(chip(k, &a.health, a.health.clone()))
        .child(
            text(format!("{cluster} / {}", a.dest_namespace))
                .font(k.ty.small)
                .color(k.c.text_muted),
        )
        .child(
            text(a.sources.join(" · "))
                .font(k.ty.mono)
                .color(k.c.text_faint)
                .truncate(),
        );
    if let Some((phase, msg, at)) = &a.operation
        && phase != "Succeeded"
    {
        r = r.child(
            text(format!("last sync {phase} {}: {msg}", age(at)))
                .font(k.ty.small)
                .color(health_color(k, phase))
                .truncate(),
        );
    }
    let mut c = col().gap(4.0).child(r);
    for (kind, msg) in &a.conditions {
        c = c.child(
            text(format!("{kind}: {msg}"))
                .font(k.ty.small)
                .color(k.c.warning.fg)
                .wrap(),
        );
    }
    c
}

fn running(k: &Kit, project: &str, d: &ProjectData, p: &Promotion) -> El {
    let (pr, name) = (project.to_string(), p.name.clone());
    let current = p
        .steps
        .last()
        .map(|s| s.alias.as_str())
        .unwrap_or("starting");
    row()
        .w_full()
        .px(16.0)
        .py(10.0)
        .gap(10.0)
        .items_center()
        .fill(k.c.warning.bg)
        .child(Spinner::new().build(k))
        .child(
            text(format!(
                "Promoting {} · step {}/{} ({current}) · {}",
                freight_label(Some(d), &p.freight),
                p.steps.len(),
                p.total_steps.max(p.steps.len()),
                took(&p.started, "")
            ))
            .font(k.ty.ui)
            .color(k.c.warning.fg)
            .truncate()
            .grow(1.0),
        )
        .child(
            small_button(k, "Abort", ButtonVariant::Danger)
                .on_press(move |s: &mut App| {
                    s.confirm = Some(Confirm::Abort {
                        project: pr.clone(),
                        promotion: name.clone(),
                    })
                })
                .build(k),
        )
}

fn resources(s: &App, k: &Kit, st: &Stage) -> El {
    if s.rows.is_empty() {
        let loading = st
            .apps
            .iter()
            .any(|(_, a)| s.apps.get(a).is_none_or(|a| a.app.is_none()));
        return if loading {
            col()
                .w_full()
                .h_full()
                .center()
                .child(Spinner::new().build(k))
        } else if st.apps.is_empty() {
            empty(
                k,
                "This stage doesn't report an Argo CD application, so there are no resources to show.",
            )
        } else {
            empty(k, "No resources.")
        };
    }
    let kc = k.clone();
    Table::new(
        |s: &App| &s.table,
        |s: &mut App| &mut s.table,
        |s: &App| s.rows.len(),
        |s: &App, i| s.rows[i].key.as_str().into(),
    )
    .column(TableColumn::new("name", "Name").flex(3.0).min(240.0))
    .column(TableColumn::new("kind", "Kind").width(120.0))
    .column(TableColumn::new("status", "Status").width(160.0))
    .column(TableColumn::new("cpu", "CPU").width(110.0))
    .column(TableColumn::new("mem", "Memory").width(130.0))
    .column(TableColumn::new("info", "Info").flex(1.0).min(120.0))
    .column(TableColumn::new("image", "Image").flex(1.2).min(120.0))
    .column(TableColumn::new("age", "Age").width(70.0))
    .cell(move |s: &App, i, col| {
        let r = &s.rows[i];
        let n = &r.node;
        match col {
            "name" => {
                let indent = if r.depth == 0 {
                    String::new()
                } else {
                    format!("{}└ ", "   ".repeat(r.depth - 1))
                };
                Cell::text(format!("{indent}{}", n.name))
            }
            "kind" => Cell::text(n.kind.clone()).muted(),
            "cpu" | "mem" => {
                let Some((used, req, measured)) = usage(s, r, col == "cpu") else {
                    return Cell::text(String::new());
                };
                let f = if col == "cpu" {
                    kube::fmt_cpu
                } else {
                    kube::fmt_mem
                };
                let used = if measured { f(used) } else { "–".into() };
                let text = if req > 0 {
                    format!("{used} / {}", f(req))
                } else {
                    used
                };
                Cell::text(text).mono()
            }
            "status" => {
                let reason = n.info("Status Reason").unwrap_or(&n.health);
                let health = if n.health.is_empty() {
                    reason
                } else {
                    &n.health
                };
                Cell::el(
                    row().gap(6.0).items_center().child(dot(&kc, health)).child(
                        text(reason.to_string())
                            .font(kc.ty.small)
                            .color(kc.c.text_base)
                            .truncate(),
                    ),
                )
            }
            "info" => {
                let info: Vec<String> = n
                    .info
                    .iter()
                    .filter(|(k, _)| {
                        matches!(
                            k.as_str(),
                            "Containers" | "Restart Count" | "Revision" | "Node"
                        )
                    })
                    .map(|(k, v)| match k.as_str() {
                        "Restart Count" => format!("{v} restarts"),
                        "Revision" => v.replace("Rev:", "rev "),
                        "Node" => v.split('.').next().unwrap_or(v).to_string(),
                        _ => v.clone(),
                    })
                    .collect();
                let mut info = info.join(" · ");
                if !n.health_message.is_empty() {
                    info = if info.is_empty() {
                        n.health_message.clone()
                    } else {
                        format!("{info} · {}", n.health_message)
                    };
                }
                Cell::text(info).muted()
            }
            "image" => Cell::text(
                n.images
                    .iter()
                    .filter(|i| !i.contains("proxyv2"))
                    .map(|i| i.rsplit('/').next().unwrap_or(i).to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
            .mono()
            .muted(),
            _ => Cell::text(age(&n.created)).muted(),
        }
    })
    .on_select(|s: &mut App, id: &str, _| s.select(Some(id)))
    .on_open(|s: &mut App, id: &str, _| {
        s.select(Some(id));
        s.inspector.set_open(true);
    })
    .name("Resources")
    .build(s, k)
    .grow(1.0)
    .min_h(0.0)
}

fn promotions(k: &Kit, project: &str, d: &ProjectData, st: &Stage) -> El {
    let mut list = col().w_full().gap(8.0).padding(16.0);
    let mut any = false;
    for p in d.promotions.iter().filter(|p| p.stage == st.name).take(50) {
        any = true;
        list = list.child(promotion(k, project, d, p).key(format!("promo/{}", p.name)));
    }
    if !any {
        list = list.child(
            text("No promotions yet.")
                .font(k.ty.ui)
                .color(k.c.text_muted),
        );
    }
    col()
        .w_full()
        .h_full()
        .min_h(0.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(list)
}

fn promotion(k: &Kit, project: &str, d: &ProjectData, p: &Promotion) -> El {
    let phase = if p.phase.is_empty() {
        "Pending"
    } else {
        &p.phase
    };
    let mut head = row()
        .w_full()
        .gap(10.0)
        .items_center()
        .child(chip(k, phase, phase.to_string()))
        .child(
            text(freight_label(Some(d), &p.freight))
                .font(k.ty.ui_medium)
                .color(k.c.text_base)
                .truncate()
                .grow(1.0),
        )
        .child(
            text(format!(
                "{} ago · took {}",
                age(&p.created),
                took(&p.started, &p.finished)
            ))
            .font(k.ty.small)
            .color(k.c.text_faint),
        );
    if p.running() {
        let (pr, name) = (project.to_string(), p.name.clone());
        head = head.child(
            small_button(k, "Abort", ButtonVariant::Danger)
                .on_press(move |s: &mut App| {
                    s.confirm = Some(Confirm::Abort {
                        project: pr.clone(),
                        promotion: name.clone(),
                    })
                })
                .build(k),
        );
    }
    let mut steps = row().gap(4.0).flex_wrap().items_center();
    for (i, step) in p.steps.iter().enumerate() {
        if i > 0 {
            steps = steps.child(text("›").font(k.ty.small).color(k.c.text_faint));
        }
        steps = steps.child(
            row()
                .gap(4.0)
                .items_center()
                .child(dot(k, &step.status))
                .child(
                    text(step.alias.clone())
                        .font(k.ty.small)
                        .color(k.c.text_muted),
                ),
        );
    }
    for _ in p.steps.len()..p.total_steps {
        steps = steps.child(text("› ○").font(k.ty.small).color(k.c.text_faint));
    }
    let mut c = col()
        .w_full()
        .gap(6.0)
        .padding(10.0)
        .radius(8.0)
        .fill(k.c.layer_01)
        .child(head)
        .child(steps)
        .child(
            text(p.name.clone())
                .font(k.ty.mono)
                .color(k.c.text_faint)
                .selectable(true),
        );
    let failed = p
        .steps
        .iter()
        .filter(|s| !s.message.is_empty())
        .map(|s| format!("{}: {}", s.alias, s.message));
    let msg: Vec<String> = std::iter::once(p.message.clone())
        .filter(|m| !m.is_empty())
        .chain(failed)
        .collect();
    for m in msg {
        c = c.child(
            text(m)
                .font(k.ty.small)
                .color(k.c.danger.fg)
                .wrap()
                .selectable(true),
        );
    }
    c
}

fn history(k: &Kit, d: &ProjectData, st: &Stage) -> El {
    let mut list = col().w_full().gap(4.0).padding(16.0);
    for (i, h) in st.history.iter().enumerate() {
        let names: Vec<String> = h
            .items
            .iter()
            .map(|f| freight_label(Some(d), &f.name))
            .collect();
        let (phase, at) = h.verification.clone().unwrap_or_default();
        let mut r = row()
            .w_full()
            .gap(10.0)
            .px(10.0)
            .py(8.0)
            .radius(6.0)
            .items_center()
            .child(
                text(names.join(" + "))
                    .font(k.ty.ui)
                    .color(k.c.text_base)
                    .truncate()
                    .grow(1.0),
            );
        if !phase.is_empty() {
            r = r.child(chip(
                k,
                &phase,
                format!("verification {}", phase.to_lowercase()),
            ));
            r = r.child(
                text(format!("{} ago", age(&at)))
                    .font(k.ty.small)
                    .color(k.c.text_faint),
            );
        }
        if i == 0 {
            r = r
                .fill(k.c.layer_01)
                .child(Badge::new("current").variant(BadgeVariant::Accent).build(k));
        }
        list = list.child(r.key(format!("h{i}")));
    }
    col()
        .w_full()
        .h_full()
        .min_h(0.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(list)
}

pub fn promote_dialog(s: &App, k: &Kit, layer: &mut Vec<rune::El>, project: &str, stage: &str) {
    let Some(d) = s.data(project) else { return };
    let Some(st) = d.stage(stage) else { return };
    let mut list = col().w_full().gap(2.0);
    for f in d
        .freight
        .iter()
        .filter(|f| st.takes_from(&f.warehouse))
        .take(40)
    {
        let ok = st.accepts(f);
        let current = st.is_current(&f.name);
        let mut notes = Vec::new();
        if !f.verified_in.is_empty() {
            notes.push(format!("verified in {}", f.verified_in.join(", ")));
        }
        if !ok {
            notes.push("not yet verified upstream".into());
        }
        let ink = if ok { k.c.text_base } else { k.c.text_faint };
        let mut item = row()
            .w_full()
            .gap(10.0)
            .px(10.0)
            .py(8.0)
            .radius(6.0)
            .items_center()
            .child(
                col()
                    .grow(1.0)
                    .min_w(0.0)
                    .gap(2.0)
                    .child(
                        row()
                            .gap(8.0)
                            .child(text(f.alias.clone()).font(k.ty.ui_medium).color(ink))
                            .child(
                                text(f.artifacts.join(", "))
                                    .font(k.ty.mono)
                                    .color(ink)
                                    .truncate(),
                            ),
                    )
                    .child(
                        text(format!("{} ago · {}", age(&f.created), notes.join(" · ")))
                            .font(k.ty.small)
                            .color(k.c.text_faint)
                            .truncate(),
                    ),
            );
        if current {
            item = item.child(Badge::new("running").variant(BadgeVariant::Accent).build(k));
        }
        if ok && !current {
            let (p, stg, fr) = (project.to_string(), stage.to_string(), f.name.clone());
            item = k.interactive(item.on_press(move |s: &mut App| {
                s.confirm = Some(Confirm::Promote {
                    project: p.clone(),
                    stage: stg.clone(),
                    freight: fr.clone(),
                })
            }));
        }
        list = list.child(item.key(format!("pick/{}", f.name)));
    }
    let body = col()
        .w_full()
        .max_h(480.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(list);
    let title = format!("Promote to {stage}");
    modal(
        "promote",
        &title,
        Some("Pick the freight to promote. Newest first."),
        vec![body],
        vec![crate::view::cancel(k)],
        k,
        layer,
        640.0,
    );
}

const ALL_KINDS: &str = "All kinds";

/// Usage and requests summed over the pods at or under a row: (used, requested, any measured).
fn usage(s: &App, r: &crate::ResRow, cpu: bool) -> Option<(u64, u64, bool)> {
    if r.pods.is_empty() {
        return None;
    }
    let live = s.apps.get(&r.app).map(|a| &a.usage);
    let (mut used, mut req, mut measured) = (0, 0, false);
    for (name, rc, rm) in &r.pods {
        req += if cpu { rc } else { rm };
        if let Some((uc, um)) = live.and_then(|u| u.get(name)) {
            used += if cpu { uc } else { um };
            measured = true;
        }
    }
    Some((used, req, measured))
}
