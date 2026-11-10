//! The right panel: the selected resource's logs, rollout status, description, events or
//! YAML, with a filter, plus the actions that make sense for its kind.

use rune::{El, GrowthSpec, ScrollSpec, col, row, text};
use rune_kit::{ButtonVariant, Kit, SegmentedControl, Select, Spinner, Switch, TextInput};

use crate::cli::age;
use crate::format::{self, Entry, LogLine};
use crate::view::{chip, error, small_button};
use crate::{App, Confirm, Pane};

const PANES: [(Pane, &str); 5] = [
    (Pane::Logs, "Logs"),
    (Pane::Rollout, "Rollout"),
    (Pane::Describe, "Describe"),
    (Pane::Events, "Events"),
    (Pane::Yaml, "YAML"),
];

pub fn view(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
    let Some(sel) = &s.selected else {
        let msg = if s.stage().is_some() {
            "Pick a resource to see its logs, rollout, description, events or YAML.\n\nKeys: l logs · r rollout · d describe · e events · y yaml · / filter"
        } else {
            "Open a stage and pick one of its resources."
        };
        return col()
            .w_full()
            .h_full()
            .padding(16.0)
            .center()
            .child(text(msg).font(k.ty.ui).color(k.c.text_muted).wrap());
    };
    let n = &sel.node;
    let mut head = col()
        .w_full()
        .px(12.0)
        .pt(10.0)
        .pb(8.0)
        .gap(6.0)
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted)
        .child(
            row()
                .w_full()
                .gap(8.0)
                .items_center()
                .child(chip(k, &n.health, n.kind.clone()))
                .child(
                    text(n.name.clone())
                        .font(k.ty.ui_medium)
                        .color(k.c.text_base)
                        .truncate()
                        .selectable(true)
                        .grow(1.0),
                ),
        );
    let t = match &sel.target {
        Ok(t) => t,
        Err(e) => return head.child(col().padding(12.0).child(error(k, e))),
    };
    head = head.child(
        text(format!("{} · {}", t.context, t.namespace))
            .font(k.ty.small)
            .color(k.c.text_faint)
            .truncate(),
    );
    let panes: Vec<(Pane, &str)> = PANES
        .iter()
        .copied()
        .filter(|(p, _)| match p {
            Pane::Logs => t.has_logs(),
            Pane::Rollout => t.has_rollout(),
            _ => true,
        })
        .collect();
    let labels: Vec<&str> = panes.iter().map(|p| p.1).collect();
    let current = panes.iter().position(|p| p.0 == s.pane);
    let picked: Vec<Pane> = panes.iter().map(|p| p.0).collect();
    head = head.child(
        SegmentedControl::new(&labels, current)
            .width(Some(80.0 * labels.len() as f32))
            .on_pick(move |s: &mut App, i, _| {
                if let Some(p) = picked.get(i) {
                    s.open_pane(*p);
                }
            })
            .build(k),
    );
    head = head.child(controls(s, k, t, layer));
    col()
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(head)
        .child(output(s, k))
}

fn controls(s: &App, k: &Kit, t: &crate::kube::Target, layer: &mut Vec<El>) -> El {
    let mut r = row().w_full().gap(8.0).items_center();
    if s.pane == Pane::Logs
        && let Some(list) = &s.containers
        && !list.is_empty()
    {
        let mut options: Vec<String> = list.clone();
        options.push("all containers".into());
        let current = s
            .container
            .clone()
            .unwrap_or_else(|| "all containers".into());
        r = r.child(
            Select::new("container", &options)
                .current(Some(&current))
                .open(s.container_open)
                .width(180.0)
                .on_toggle(|s: &mut App| s.container_open ^= true)
                .on_close(|s: &mut App| s.container_open = false)
                .on_choose(|s: &mut App, v: &str, _| {
                    s.container_open = false;
                    s.container = (v != "all containers").then(|| v.to_string());
                    s.open_pane(Pane::Logs);
                })
                .build(k, layer),
        );
        r = r.child(
            Switch::new(s.previous)
                .label("Previous")
                .on_toggle(|s: &mut App| {
                    s.previous ^= true;
                    s.open_pane(Pane::Logs);
                })
                .build(k),
        );
    }
    let filter = TextInput::new(&s.filter)
        .placeholder("Filter  /")
        .width(None)
        .on_edit(|s: &mut App| &mut s.filter)
        .build(k);
    r = r.child(col().grow(1.0).min_w(80.0).child(filter));
    if t.has_rollout() {
        let tt = t.clone();
        r = r.child(
            small_button(k, "Restart", ButtonVariant::Outline)
                .on_press(move |s: &mut App| s.confirm = Some(Confirm::Restart(tt.clone())))
                .build(k),
        );
    }
    if t.kind == "Pod" {
        let tt = t.clone();
        r = r.child(
            small_button(k, "Delete", ButtonVariant::Ghost)
                .on_press(move |s: &mut App| s.confirm = Some(Confirm::Delete(tt.clone())))
                .build(k),
        );
    }
    r.child(
        small_button(k, "Reload", ButtonVariant::Ghost)
            .on_press(|s: &mut App| s.open_pane(s.pane))
            .build(k),
    )
}

fn output(s: &App, k: &Kit) -> El {
    let o = &s.out;
    let mut status = row().w_full().px(12.0).py(4.0).gap(8.0).items_center();
    if o.loading {
        status = status.child(Spinner::new().build(k));
    }
    let live = o.stream.is_some() && o.done.is_none();
    let what = match (s.pane, live) {
        (Pane::Logs, true) => "following".to_string(),
        (Pane::Rollout, true) => "watching rollout".to_string(),
        _ => o.done.clone().unwrap_or_default(),
    };
    let count = match (s.pane, &o.events) {
        (Pane::Events, Some(ev)) => format!("{} events", ev.len()),
        (Pane::Describe, _) if !o.describe.is_empty() => String::new(),
        _ if s.filter.text.trim().is_empty() => format!("{} lines", o.lines.len()),
        _ => format!("{} of {} lines", o.shown.len(), o.lines.len()),
    };
    status = status
        .child(text(count).font(k.ty.small).color(k.c.text_faint))
        .child(text(what).font(k.ty.small).color(if live {
            k.c.success.fg
        } else {
            k.c.text_faint
        }))
        .child(row().grow(1.0));
    if s.pane == Pane::Logs {
        if !s.raw_logs && o.parsed.iter().any(|l| l.json.is_some()) {
            status = status.child(
                small_button(
                    k,
                    if s.expand_all {
                        "Collapse all"
                    } else {
                        "Expand all"
                    },
                    ButtonVariant::GhostFaint,
                )
                .on_press(|s: &mut App| {
                    s.expand_all ^= true;
                    s.expanded.clear();
                })
                .build(k),
            );
        }
        status = status.child(
            Switch::new(s.raw_logs)
                .label("Raw")
                .on_toggle(|s: &mut App| s.raw_logs ^= true)
                .build(k),
        );
    }
    let body = match s.pane {
        Pane::Events if o.events.is_some() => events(s, k),
        Pane::Describe if !o.describe.is_empty() => describe(s, k),
        Pane::Logs if !s.raw_logs => lines(s, k, Style::Logs),
        Pane::Yaml => lines(s, k, Style::Yaml),
        _ => lines(s, k, Style::Plain),
    };
    col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .child(status)
        .child(body)
}

#[derive(Clone, Copy, PartialEq)]
enum Style {
    Plain,
    Logs,
    Yaml,
}

fn scroller(s: &App, inner: El) -> El {
    col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(ScrollSpec {
            stick_bottom: matches!(s.pane, Pane::Logs | Pane::Rollout),
        })
        .child(inner)
}

/// Long outputs (logs, YAML) as virtual rows, one per line.
fn lines(s: &App, k: &Kit, style: Style) -> El {
    let kc = k.clone();
    let rows = col()
        .key(format!("out-{}", s.out_gen))
        .w_full()
        .px(12.0)
        .growth(GrowthSpec::default())
        .virtual_rows(
            s.out.shown.len(),
            18.0,
            |s: &App, i| format!("l{}", s.out.first_id + s.out.shown[i] as u64).into(),
            move |s: &App, i| {
                let idx = s.out.shown[i];
                let line = &s.out.lines[idx];
                let k = &kc;
                if line.starts_with("! ") {
                    return text(line.clone())
                        .font(k.ty.mono)
                        .color(k.c.danger.fg)
                        .wrap()
                        .selectable(true);
                }
                match style {
                    Style::Logs if idx < s.out.parsed.len() => {
                        log_row(s, k, &s.out.parsed[idx], s.out.first_id + idx as u64)
                    }
                    Style::Yaml => row()
                        .w_full()
                        .gap(10.0)
                        .child(
                            text(format!("{:>4}", idx + 1))
                                .font(k.ty.mono)
                                .color(k.c.text_faint),
                        )
                        .child(
                            text(line.clone())
                                .font(k.ty.mono)
                                .color(k.c.text_base)
                                .spans(format::code_spans(k, "yaml", line))
                                .wrap()
                                .selectable(true)
                                .grow(1.0)
                                .min_w(0.0),
                        ),
                    _ => text(line.clone())
                        .font(k.ty.mono)
                        .color(k.c.text_base)
                        .spans(format::kv_spans(k, line))
                        .wrap()
                        .selectable(true),
                }
            },
        )
        .virtual_rows_version((s.out.version, s.expand_all, s.expanded.len()));
    scroller(s, rows)
}

fn log_row(s: &App, k: &Kit, l: &LogLine, id: u64) -> El {
    let open = l.json.is_some() && (s.expand_all ^ s.expanded.contains(&id));
    let toggle = move |s: &mut App| {
        if !s.expanded.remove(&id) {
            s.expanded.insert(id);
        }
    };
    let mut head = row().w_full().gap(8.0);
    head = head.child(if l.json.is_some() {
        k.interactive(
            row()
                .w(14.0)
                .cursor(rune::Cursor::Pointer)
                .role("button")
                .label(if open { "Collapse" } else { "Expand" })
                .child(
                    text(if open { "▾" } else { "▸" })
                        .font(k.ty.mono)
                        .color(k.c.text_muted),
                )
                .on_press(toggle),
        )
    } else {
        row().w(14.0)
    });
    if !l.time.is_empty() {
        head = head.child(text(l.time.clone()).font(k.ty.mono).color(k.c.text_faint));
    }
    if !l.level.is_empty() {
        let lv: String = l.level.to_uppercase().chars().take(5).collect();
        head = head.child(
            text(lv)
                .font(k.ty.mono)
                .color(format::level_color(k, &l.level))
                .w(44.0),
        );
    }
    if !l.prefix.is_empty() {
        // `pod/web-7c9-abcde/api` → `abcde/api`.
        let mut parts = l.prefix.rsplit('/');
        let (c, pod) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let short = pod.rsplit('-').next().unwrap_or(pod);
        head = head.child(
            text(format!("{short}/{c}"))
                .font(k.ty.small)
                .color(k.c.text_faint),
        );
    }
    let msg = match (&l.json, l.msg.is_empty()) {
        (Some(v), true) => v.to_string(),
        _ => l.msg.clone(),
    };
    let ink = match l.level.to_lowercase().as_str() {
        "error" | "err" | "fatal" | "panic" | "critical" | "warn" | "warning" => {
            format::level_color(k, &l.level)
        }
        _ => k.c.text_base,
    };
    head = head.child(
        text(msg)
            .font(k.ty.mono)
            .color(ink)
            .wrap()
            .selectable(true)
            .grow(1.0)
            .min_w(0.0),
    );
    if l.extra > 0 {
        head = head.child(
            k.interactive(
                row()
                    .cursor(rune::Cursor::Pointer)
                    .child(
                        text(format!("+{}", l.extra))
                            .font(k.ty.small)
                            .color(k.c.text_faint),
                    )
                    .on_press(toggle),
            ),
        );
    }
    let mut c = col().w_full().py(1.0).child(head);
    if open && let Some(v) = &l.json {
        let pretty = serde_json::to_string_pretty(v).unwrap_or_default();
        c = c.child(
            col().w_full().pl(22.0).py(4.0).child(
                col()
                    .w_full()
                    .padding(8.0)
                    .radius(6.0)
                    .fill(k.c.layer_01)
                    .child(
                        text(pretty.clone())
                            .font(k.ty.mono)
                            .color(k.c.text_base)
                            .spans(format::code_spans(k, "json", &pretty))
                            .wrap()
                            .selectable(true),
                    ),
            ),
        );
    }
    c
}

fn matches(filter: &str, parts: &[&str]) -> bool {
    filter.is_empty() || parts.iter().any(|p| p.to_lowercase().contains(filter))
}

fn events(s: &App, k: &Kit) -> El {
    let f = s.filter.text.trim().to_lowercase();
    let list = s.out.events.as_deref().unwrap_or_default();
    let mut c = col().w_full().gap(8.0).padding(12.0);
    if list.is_empty() {
        c = c.child(
            text("No events. Kubernetes keeps them for about an hour.")
                .font(k.ty.ui)
                .color(k.c.text_muted)
                .wrap(),
        );
    }
    for (i, e) in list.iter().enumerate() {
        if !matches(&f, &[&e.reason, &e.message, &e.kind, &e.source, &e.field]) {
            continue;
        }
        let warn = e.kind == "Warning";
        let tone = if warn { &k.c.warning } else { &k.c.info };
        let mut head = row()
            .w_full()
            .gap(8.0)
            .items_center()
            .child(
                text(e.kind.clone())
                    .font(k.ty.small_medium)
                    .color(tone.fg)
                    .px(6.0)
                    .py(1.0)
                    .radius(4.0)
                    .fill(tone.bg),
            )
            .child(
                text(e.reason.clone())
                    .font(k.ty.ui_medium)
                    .color(k.c.text_base),
            );
        if e.count > 1 {
            head = head.child(
                text(format!("×{}", e.count))
                    .font(k.ty.small_medium)
                    .color(k.c.text_muted),
            );
        }
        head = head.child(row().grow(1.0)).child(
            text(format!("{} ago", age(&e.last)))
                .font(k.ty.small)
                .color(k.c.text_faint),
        );
        let mut meta = vec![e.source.clone()];
        if !e.field.is_empty() {
            meta.push(e.field.clone());
        }
        if e.count > 1 && !e.first.is_empty() {
            meta.push(format!("first {} ago", age(&e.first)));
        }
        let card = col()
            .w_full()
            .gap(4.0)
            .padding(10.0)
            .radius(8.0)
            .fill(k.c.layer_01)
            .border(
                1.0,
                if warn {
                    k.c.warning.border
                } else {
                    k.c.border_muted
                },
            )
            .child(head)
            .child(
                text(e.message.clone())
                    .font(k.ty.ui)
                    .color(k.c.text_base)
                    .wrap()
                    .selectable(true),
            )
            .child(
                text(
                    meta.into_iter()
                        .filter(|m| !m.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · "),
                )
                .font(k.ty.small)
                .color(k.c.text_faint)
                .truncate(),
            );
        c = c.child(card.key(format!("ev{i}")));
    }
    scroller(s, c)
}

/// Sections that start closed: long and rarely what you're after.
fn starts_closed(key: &str) -> bool {
    matches!(
        key,
        "Annotations"
            | "Volumes"
            | "Tolerations"
            | "Init Containers"
            | "Node-Selectors"
            | "Pod Template"
    )
}

fn describe(s: &App, k: &Kit) -> El {
    let f = s.filter.text.trim().to_lowercase();
    let mut fields = col()
        .w_full()
        .gap(6.0)
        .padding(10.0)
        .radius(8.0)
        .fill(k.c.layer_01);
    let mut sections = col().w_full().gap(8.0);
    let mut any_field = false;
    for e in &s.out.describe {
        let all: Vec<&str> = [e.key.as_str(), e.value.as_str()]
            .into_iter()
            .chain(e.lines.iter().map(String::as_str))
            .collect();
        if !matches(&f, &all) {
            continue;
        }
        if e.is_field() {
            any_field = true;
            fields = fields.child(field(k, e).key(format!("f/{}", e.key)));
            continue;
        }
        let open = starts_closed(&e.key) == s.toggled.contains(&e.key) || !f.is_empty();
        let key = e.key.clone();
        let mut title = row()
            .w_full()
            .gap(8.0)
            .items_center()
            .cursor(rune::Cursor::Pointer)
            .child(
                text(if open { "▾" } else { "▸" })
                    .font(k.ty.mono)
                    .color(k.c.text_muted),
            )
            .child(
                text(e.key.clone())
                    .font(k.ty.ui_medium)
                    .color(k.c.text_base),
            );
        if !e.value.is_empty() {
            title = title.child(
                text(e.value.clone())
                    .font(k.ty.small)
                    .color(k.c.text_muted)
                    .truncate(),
            );
        }
        let title = k.interactive(title.on_press(move |s: &mut App| {
            if !s.toggled.remove(&key) {
                s.toggled.insert(key.clone());
            }
        }));
        let mut card = col()
            .w_full()
            .gap(2.0)
            .padding(10.0)
            .radius(8.0)
            .fill(k.c.layer_01)
            .child(title);
        if open {
            let mut body = col().w_full().gap(1.0).pt(6.0);
            for (i, line) in e.lines.iter().enumerate() {
                if !f.is_empty() && !line.to_lowercase().contains(&f) && !is_subhead(line) {
                    continue;
                }
                let el = if is_subhead(line) {
                    text(line.trim_end_matches(':').to_string())
                        .font(k.ty.ui_medium)
                        .color(k.c.text_accent)
                        .pt(if i == 0 { 0.0 } else { 8.0 })
                } else {
                    text(line.clone())
                        .font(k.ty.mono)
                        .color(k.c.text_base)
                        .spans(format::kv_spans(k, line))
                        .wrap()
                        .selectable(true)
                };
                body = body.child(el.key(format!("{i}")));
            }
            card = card.child(body);
        }
        sections = sections.child(card.key(format!("s/{}", e.key)));
    }
    let mut c = col().w_full().gap(8.0).padding(12.0);
    if any_field {
        c = c.child(fields);
    }
    scroller(s, c.child(sections))
}

/// A container or volume name heading its block inside a section (`  api:`).
fn is_subhead(line: &str) -> bool {
    !line.starts_with(' ') && line.ends_with(':') && !line.trim_end_matches(':').contains(' ')
}

fn field(k: &Kit, e: &Entry) -> El {
    let label = text(e.key.clone())
        .font(k.ty.small_medium)
        .color(k.c.text_muted)
        .w(150.0)
        .pt(2.0);
    let values: Vec<&str> = std::iter::once(e.value.as_str())
        .chain(e.lines.iter().map(String::as_str))
        .filter(|v| !v.is_empty())
        .collect();
    let value = if values.len() > 1 || values.first().is_some_and(|v| v.contains('=')) {
        let mut pills = row().gap(4.0).flex_wrap();
        for v in values {
            pills = pills.child(
                text(v.to_string())
                    .font(k.ty.small)
                    .color(k.c.text_base)
                    .px(6.0)
                    .py(1.0)
                    .radius(4.0)
                    .fill(k.c.surface_raised)
                    .border(1.0, k.c.border_muted)
                    .selectable(true),
            );
        }
        pills
    } else {
        let v = values.first().copied().unwrap_or("");
        row().child(
            text(v.to_string())
                .font(k.ty.ui)
                .color(k.c.text_base)
                .spans(format::kv_spans(k, v))
                .wrap()
                .selectable(true),
        )
    };
    row()
        .w_full()
        .gap(12.0)
        .child(label)
        .child(value.grow(1.0).min_w(0.0))
}
