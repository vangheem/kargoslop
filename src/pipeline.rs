//! A project's pipeline: per warehouse, its stages in order and a freight × stage matrix
//! showing where each piece of freight runs, where it was verified, and where it can go.

use rune::{El, ScrollSpec, col, row, text};
use rune_kit::{Badge, BadgeVariant, ButtonVariant, Kit, Spinner};

use crate::kargo::{Freight, ProjectData, Stage, Warehouse};
use crate::view::{chip, dot, empty, error, small_button};
use crate::{App, Confirm, View};

const FREIGHT_COL: f32 = 280.0;
const STAGE_COL: f32 = 150.0;
const MAX_FREIGHT: usize = 30;

pub fn view(s: &App, k: &Kit, project: &str) -> El {
    let Some(p) = s.projects.get(project) else {
        return empty(k, "Unknown project.");
    };
    let Some(d) = &p.data else {
        return match &p.error {
            Some(e) => col().padding(16.0).child(error(k, e)),
            None => col()
                .w_full()
                .h_full()
                .center()
                .child(Spinner::new().build(k)),
        };
    };
    let mut c = col().w_full().gap(28.0).padding(16.0);
    if let Some(e) = &p.error {
        c = c.child(error(k, e));
    }
    for w in &d.warehouses {
        c = c.child(warehouse(s, k, project, d, w).key(format!("wh/{}", w.name)));
    }
    col()
        .w_full()
        .h_full()
        .min_h(0.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(c)
}

fn warehouse(s: &App, k: &Kit, project: &str, d: &ProjectData, w: &Warehouse) -> El {
    let stages = d.pipeline(&w.name);
    let (p, name) = (project.to_string(), w.name.clone());
    let refresh = small_button(k, "Discover now", ButtonVariant::Ghost)
        .on_press(move |s: &mut App| {
            let (p, name) = (p.clone(), name.clone());
            s.act("Warehouse refresh requested", move || {
                crate::kargo::refresh_warehouse(&p, &name)
            });
        })
        .build(k);
    let mut sub = vec![w.subscriptions.join(", ")];
    if !w.interval.is_empty() {
        sub.push(format!("every {}", w.interval));
    }
    sub.push(w.message.clone());
    let header = row()
        .w_full()
        .gap(10.0)
        .items_center()
        .child(dot(
            k,
            if w.ready == Some(true) {
                "Healthy"
            } else {
                "Unknown"
            },
        ))
        .child(
            text(format!("Warehouse {}", w.name))
                .font(k.ty.body_medium)
                .color(k.c.text_base),
        )
        .child(
            text(
                sub.into_iter()
                    .filter(|x| !x.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · "),
            )
            .font(k.ty.small)
            .color(k.c.text_muted)
            .truncate()
            .grow(1.0),
        )
        .child(refresh);
    let mut cards = row().w_full().gap(8.0).flex_wrap().items_center();
    for (i, st) in stages.iter().enumerate() {
        if i > 0 {
            cards = cards.child(text("→").font(k.ty.ui).color(k.c.text_faint));
        }
        cards = cards.child(stage_card(s, k, project, d, st));
    }
    col()
        .w_full()
        .gap(12.0)
        .child(header)
        .child(cards)
        .child(matrix(k, project, d, w, &stages))
}

fn stage_card(s: &App, k: &Kit, project: &str, d: &ProjectData, st: &Stage) -> El {
    let promoting = d
        .promotions
        .iter()
        .find(|p| p.stage == st.name && p.running());
    let current = st.current();
    let version = current
        .iter()
        .map(|f| {
            let alias = d.freight(&f.name).map_or("", |x| x.alias.as_str());
            format!("{alias} {}", f.artifacts.join(", "))
        })
        .collect::<Vec<_>>()
        .join(" + ");
    let mut c = col()
        .w(240.0)
        .gap(4.0)
        .padding(10.0)
        .radius(8.0)
        .fill(k.c.layer_01)
        .border(1.0, k.c.border_muted)
        .child(
            row()
                .gap(6.0)
                .items_center()
                .child(chip(k, &st.health, st.short().to_string()))
                .child(row().grow(1.0))
                .child(if st.auto_promote {
                    Badge::new("auto").variant(BadgeVariant::Subtle).build(k)
                } else {
                    row()
                }),
        )
        .child(
            text(version)
                .font(k.ty.mono)
                .color(k.c.text_muted)
                .truncate(),
        );
    if let Some(p) = promoting {
        c = c.child(
            text(format!(
                "promoting · step {}/{}",
                p.steps.len(),
                p.total_steps.max(p.steps.len())
            ))
            .font(k.ty.small_medium)
            .color(k.c.warning.fg),
        );
    } else if st.ready == Some(false) && !st.message.is_empty() {
        c = c.child(
            text(st.message.clone())
                .font(k.ty.small)
                .color(k.c.danger.fg)
                .max_lines(2),
        );
    }
    let view = View::Stage(project.to_string(), st.name.clone());
    let selected = s.view.as_ref() == Some(&view);
    if selected {
        c = c.border(1.0, k.c.border_focus);
    }
    k.interactive(c.on_press(move |s: &mut App| s.open(view.clone())))
        .key(format!("card/{}", st.name))
}

fn matrix(k: &Kit, project: &str, d: &ProjectData, w: &Warehouse, stages: &[&Stage]) -> El {
    let head_cell = |label: String, width: f32| {
        text(label)
            .font(k.ty.small_medium)
            .color(k.c.text_muted)
            .truncate()
            .w(width)
            .px(8.0)
    };
    let mut head = row()
        .w_full()
        .py(6.0)
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted)
        .child(head_cell("Freight".into(), FREIGHT_COL));
    for st in stages {
        head = head.child(head_cell(st.short().to_string(), STAGE_COL));
    }
    let mut table = col().w_full().child(head);
    for f in d
        .freight
        .iter()
        .filter(|f| f.warehouse == w.name)
        .take(MAX_FREIGHT)
    {
        let mut r = row()
            .w_full()
            .py(5.0)
            .items_center()
            .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted)
            .child(
                col()
                    .w(FREIGHT_COL)
                    .px(8.0)
                    .gap(1.0)
                    .child(
                        row()
                            .gap(6.0)
                            .child(
                                text(f.alias.clone())
                                    .font(k.ty.ui_medium)
                                    .color(k.c.text_base)
                                    .truncate(),
                            )
                            .child(
                                text(crate::cli::age(&f.created))
                                    .font(k.ty.small)
                                    .color(k.c.text_faint),
                            ),
                    )
                    .child(
                        text(f.artifacts.join(", "))
                            .font(k.ty.mono)
                            .color(k.c.text_muted)
                            .truncate(),
                    ),
            );
        for st in stages {
            r = r.child(cell(k, project, st, f));
        }
        table = table.child(r.key(format!("fr/{}", f.name)));
    }
    table
}

fn cell(k: &Kit, project: &str, st: &Stage, f: &Freight) -> El {
    let c = row().w(STAGE_COL).px(8.0).items_center();
    if st.is_current(&f.name) {
        return c.child(
            Badge::new("● running")
                .variant(BadgeVariant::Accent)
                .build(k),
        );
    }
    let verified = f.verified_in.contains(&st.name);
    if st.accepts(f) {
        let (p, stage, freight) = (project.to_string(), st.name.clone(), f.name.clone());
        let label = if verified { "✓ promote" } else { "promote" };
        return c.child(
            small_button(k, label, ButtonVariant::Outline)
                .on_press(move |s: &mut App| {
                    s.confirm = Some(Confirm::Promote {
                        project: p.clone(),
                        stage: stage.clone(),
                        freight: freight.clone(),
                    })
                })
                .build(k),
        );
    }
    if verified {
        return c.child(text("✓ verified").font(k.ty.small).color(k.c.success.fg));
    }
    c.child(text("·").font(k.ty.small).color(k.c.text_faint))
}
