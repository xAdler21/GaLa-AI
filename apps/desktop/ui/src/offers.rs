//! Angebote: LV-Liste, Import, Einheitspreise eingeben (speichert sofort), Abgabe exportieren.

use api_types::client::{ExportFormat, LvDetail, LvSummary, PriceEntry};
use leptos::prelude::*;
use leptos::task::spawn_local;
use lv_model::number::{NumberLocale, parse_decimal};
use lv_model::{Decimal, Lv, Node, Oz, PositionsArt};
use wasm_bindgen::JsCast;

use crate::state::Ctx;
use crate::tauri::{self, SavedExport};

const ACCEPT: &str = ".x83,.x84,.x81,.x86,.d83,.d84,.d81,.d86,.p83";

// --- Zahlen --------------------------------------------------------------------------------

/// Deutsche Schreibweise mit Tausenderpunkt, mindestens `min_dp` Nachkommastellen.
pub fn fmt_de(d: Decimal, min_dp: u32) -> String {
    let d = d.normalize();
    let dp = d.scale().max(min_dp) as usize;
    let s = format!("{d:.dp$}");
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let (sign, digits) = int.strip_prefix('-').map_or(("", int), |r| ("-", r));
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push('.');
        }
        grouped.push(c);
    }
    if frac.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped},{frac}")
    }
}

fn money(d: Decimal) -> String {
    fmt_de(d.round_dp(2), 2)
}

/// Eingabe → EP. Leer heißt „kein Preis“; `Err` bei unlesbarer oder negativer Zahl.
fn parse_ep(raw: &str) -> Result<Option<Decimal>, ()> {
    let t = raw.trim().trim_end_matches('€').trim();
    if t.is_empty() {
        return Ok(None);
    }
    match parse_decimal(t, NumberLocale::German) {
        Some(d) if d >= Decimal::ZERO => Ok(Some(d.round_dp(3))),
        _ => Err(()),
    }
}

// --- Seite -----------------------------------------------------------------------------------

#[component]
pub fn OffersPage() -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    move || match ctx.offer.get() {
        None => view! { <LvList/> }.into_any(),
        Some(id) => view! { <LvEditor id/> }.into_any(),
    }
}

#[component]
fn LvList() -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    let items = RwSignal::new(None::<Result<Vec<LvSummary>, String>>);
    let busy = RwSignal::new(None::<String>);
    let error = RwSignal::new(None::<String>);
    let confirm = RwSignal::new(None::<String>);

    let reload = move || {
        spawn_local(async move {
            let r = tauri::lv_list().await.map_err(|e| {
                ctx.handle_error(&e);
                e.message
            });
            items.try_set(Some(r));
        });
    };
    reload();

    let on_file = move |ev: leptos::ev::Event| {
        let Some(input) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
        else {
            return;
        };
        let Some(file) = input.files().and_then(|f| f.get(0)) else {
            return;
        };
        input.set_value("");
        let name = file.name();
        busy.set(Some(format!("{name} wird eingelesen …")));
        error.set(None);
        spawn_local(async move {
            let result = async {
                let buf = wasm_bindgen_futures::JsFuture::from(file.array_buffer())
                    .await
                    .map_err(|_| "Datei konnte nicht gelesen werden.".to_owned())?;
                let bytes = js_sys::Uint8Array::new(&buf).to_vec();
                tauri::lv_import(&name, &bytes).await.map_err(|e| {
                    ctx.handle_error(&e);
                    e.message
                })
            }
            .await;
            busy.try_set(None);
            match result {
                Ok(detail) => ctx.offer.set(Some(detail.summary.id.to_string())),
                Err(m) => {
                    error.try_set(Some(m));
                }
            }
        });
    };

    let delete = move |id: String| {
        confirm.set(None);
        spawn_local(async move {
            match tauri::lv_delete(&id).await {
                Ok(()) => reload(),
                Err(e) => {
                    ctx.handle_error(&e);
                    error.try_set(Some(e.message));
                }
            }
        });
    };

    view! {
        <div class="page">
            <div class="page-head">
                <h1>"Angebote"</h1>
                <label class="btn" class:disabled=move || busy.get().is_some()>
                    "LV importieren …"
                    <input type="file" accept=ACCEPT class="hidden" on:change=on_file
                        disabled=move || busy.get().is_some()/>
                </label>
            </div>
            <p class="muted">"GAEB-Ausschreibung (X83 oder D83) importieren, Einheitspreise eintragen und die Abgabe als X84/D84 oder Excel erzeugen."</p>
            {move || busy.get().map(|m| view! { <div class="msg ok"><span class="spinner"></span>" "{m}</div> })}
            {move || error.get().map(|m| view! { <div class="msg err">{m}</div> })}
            {move || match items.get() {
                None => view! { <p class="muted"><span class="spinner"></span>" Lade …"</p> }.into_any(),
                Some(Err(m)) => view! { <div class="msg err">{m}</div> }.into_any(),
                Some(Ok(list)) if list.is_empty() => view! {
                    <div class="empty small-top">"Noch keine LVs. Oben rechts eine GAEB-Datei importieren."</div>
                }.into_any(),
                Some(Ok(list)) => view! {
                    <table class="list">
                        <thead><tr>
                            <th>"Bezeichnung"</th><th>"Format"</th><th>"Datei"</th>
                            <th class="num">"Bepreist"</th><th class="num">"Summe netto"</th>
                            <th>"Geändert"</th><th></th>
                        </tr></thead>
                        <tbody>
                            {list.into_iter().map(|s| {
                                let id = s.id.to_string();
                                let (open_id, del_id, ask_id, is_ask) = (id.clone(), id.clone(), id.clone(), id.clone());
                                let done = s.positions > 0 && s.priced == s.positions;
                                view! {
                                    <tr class="click" on:click=move |_| ctx.offer.set(Some(open_id.clone()))>
                                        <td><strong>{s.name}</strong></td>
                                        <td>{s.format}</td>
                                        <td class="muted">{s.file_name}</td>
                                        <td class="num" class:ok=done>{format!("{} / {}", s.priced, s.positions)}</td>
                                        <td class="num">{format!("{} €", money(s.total))}</td>
                                        <td class="muted">{format!("{}{}", s.updated_at.format("%d.%m.%Y"),
                                            s.created_by.map(|u| format!(", {u}")).unwrap_or_default())}</td>
                                        <td class="actions" on:click=|ev| ev.stop_propagation()>
                                            {move || if confirm.get().as_deref() == Some(is_ask.as_str()) {
                                                let del_id = del_id.clone();
                                                view! {
                                                    <button class="btn danger" on:click=move |_| delete(del_id.clone())>"Wirklich löschen"</button>
                                                    <button class="btn secondary" on:click=move |_| confirm.set(None)>"Abbrechen"</button>
                                                }.into_any()
                                            } else {
                                                let ask_id = ask_id.clone();
                                                view! {
                                                    <button class="btn secondary" on:click=move |_| confirm.set(Some(ask_id.clone()))>"Löschen"</button>
                                                }.into_any()
                                            }}
                                        </td>
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                }.into_any(),
            }}
        </div>
    }
}

// --- Editor ----------------------------------------------------------------------------------

#[derive(Clone, PartialEq)]
enum SaveState {
    Idle,
    Pending,
    Error(String),
}

#[derive(Clone)]
struct PosRow {
    idx: usize,
    oz: Oz,
    kurztext: String,
    langtext: String,
    menge: Option<Decimal>,
    einheit: String,
    art: PositionsArt,
    /// Text im Eingabefeld.
    ep_text: RwSignal<String>,
    /// Zuletzt gespeicherter EP (zum Erkennen echter Änderungen).
    saved: RwSignal<Option<Decimal>>,
    gp: RwSignal<Option<Decimal>>,
    state: RwSignal<SaveState>,
    open: RwSignal<bool>,
}

#[derive(Clone)]
enum Row {
    Title {
        depth: usize,
        oz: String,
        label: String,
    },
    Pos(PosRow),
}

fn rows_of(lv: &Lv) -> Vec<Row> {
    fn walk(nodes: &[Node], depth: usize, out: &mut Vec<Row>, idx: &mut usize) {
        for n in nodes {
            match n {
                Node::Titel(t) => {
                    out.push(Row::Title {
                        depth,
                        oz: t
                            .oz
                            .value
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                        label: t.bezeichnung.value.clone().unwrap_or_default(),
                    });
                    walk(&t.children, depth + 1, out, idx);
                }
                Node::Position(p) => {
                    let Some(oz) = p.oz.value.clone() else {
                        continue;
                    };
                    let ep = p.ep.value;
                    out.push(Row::Pos(PosRow {
                        idx: *idx,
                        oz,
                        kurztext: p.kurztext.value.clone().unwrap_or_default(),
                        langtext: p.langtext.value.clone().unwrap_or_default(),
                        menge: p.menge.value,
                        einheit: p
                            .einheit
                            .value
                            .map(|u| u.symbol().to_owned())
                            .or_else(|| p.einheit.raw.clone())
                            .unwrap_or_default(),
                        art: p.art,
                        ep_text: RwSignal::new(ep.map(|d| fmt_de(d, 2)).unwrap_or_default()),
                        saved: RwSignal::new(ep),
                        gp: RwSignal::new(p.gp.value.or_else(|| p.computed_gp())),
                        state: RwSignal::new(SaveState::Idle),
                        open: RwSignal::new(false),
                    }));
                    *idx += 1;
                }
                Node::Vorbemerkung(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&lv.nodes, 0, &mut out, &mut 0);
    out
}

fn art_label(art: PositionsArt) -> Option<&'static str> {
    match art {
        PositionsArt::Bedarf => Some("Bedarf"),
        PositionsArt::Alternativ => Some("Wahl"),
        PositionsArt::Stundenlohn => Some("Stundenlohn"),
        PositionsArt::Zulage => Some("Zulage"),
        PositionsArt::Normal => None,
    }
}

fn focus_ep(idx: usize) {
    let el = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| {
            d.query_selector(&format!("input.ep[data-idx=\"{idx}\"]"))
                .ok()
                .flatten()
        })
        .and_then(|e| e.dyn_into::<web_sys::HtmlInputElement>().ok());
    if let Some(el) = el {
        let _ = el.focus();
        el.select();
    }
}

#[derive(Clone, PartialEq)]
enum ExportState {
    Idle,
    Running(ExportFormat),
    Done(SavedExport),
    Failed(String),
}

#[component]
fn LvEditor(id: String) -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    let summary = RwSignal::new(None::<LvSummary>);
    let rows = RwSignal::new(Vec::<Row>::new());
    let exports = RwSignal::new(Vec::<ExportFormat>::new());
    let warnings = RwSignal::new(Vec::<String>::new());
    let load_error = RwSignal::new(None::<String>);
    let save_error = RwSignal::new(None::<String>);
    let only_open = RwSignal::new(false);
    let export_state = RwSignal::new(ExportState::Idle);
    // Warteschlange: immer nur eine Speicher-Anfrage unterwegs, sonst kollidieren die Revisionen.
    let queue = ArcStoredValue::new(Vec::<PriceEntry>::new());
    let in_flight = RwSignal::new(false);
    let id = StoredValue::new(id);

    let load = move || {
        let id = id.get_value();
        spawn_local(async move {
            match tauri::lv_get(&id).await {
                Ok(d) => {
                    rows.try_set(rows_of(&d.lv));
                    exports.try_set(d.exports);
                    warnings.try_set(d.warnings);
                    summary.try_set(Some(d.summary));
                    load_error.try_set(None);
                    save_error.try_set(None);
                }
                Err(e) => {
                    ctx.handle_error(&e);
                    if e.code == "not_found" {
                        ctx.offer.set(None);
                    } else {
                        load_error.try_set(Some(e.message));
                    }
                }
            }
        });
    };
    load();

    let apply = move |d: &LvDetail| {
        let by_oz: std::collections::HashMap<&Oz, _> =
            d.lv.positions()
                .filter_map(|p| Some((p.oz.value.as_ref()?, p)))
                .collect();
        rows.with_untracked(|rs| {
            for r in rs {
                if let Row::Pos(p) = r
                    && let Some(src) = by_oz.get(&p.oz)
                {
                    p.gp.try_set(src.gp.value.or_else(|| src.computed_gp()));
                    p.saved.try_set(src.ep.value);
                }
            }
        });
        summary.try_set(Some(d.summary.clone()));
    };

    fn flush(
        ctx: Ctx,
        id: StoredValue<String>,
        queue: ArcStoredValue<Vec<PriceEntry>>,
        in_flight: RwSignal<bool>,
        summary: RwSignal<Option<LvSummary>>,
        rows: RwSignal<Vec<Row>>,
        save_error: RwSignal<Option<String>>,
        apply: impl Fn(&LvDetail) + Copy + 'static,
    ) {
        if in_flight.get_untracked() {
            return;
        }
        let batch = std::mem::take(&mut *queue.write_value());
        if batch.is_empty() {
            return;
        }
        let Some(revision) = summary.with_untracked(|s| s.as_ref().map(|s| s.revision)) else {
            return;
        };
        in_flight.set(true);
        let Some(id_val) = id.try_get_value() else {
            return;
        };
        spawn_local(async move {
            let result = tauri::lv_set_prices(&id_val, revision, &batch).await;
            let set_state = |state: SaveState| {
                rows.with_untracked(|rs| {
                    for r in rs {
                        if let Row::Pos(p) = r
                            && batch.iter().any(|b| b.oz == p.oz)
                        {
                            p.state.try_set(state.clone());
                        }
                    }
                });
            };
            match result {
                Ok(d) => {
                    apply(&d);
                    set_state(SaveState::Idle);
                    save_error.try_set(None);
                }
                Err(e) => {
                    ctx.handle_error(&e);
                    let msg = if e.code == "conflict" {
                        "Jemand anderes hat das LV inzwischen gespeichert. Bitte neu laden; die letzte Eingabe ist nicht gespeichert.".to_owned()
                    } else {
                        format!("Nicht gespeichert: {}", e.message)
                    };
                    set_state(SaveState::Error(msg.clone()));
                    save_error.try_set(Some(msg));
                    // Bei Fehlern nicht endlos weiterschicken.
                    queue.write_value().clear();
                }
            }
            // `try_set` gibt den Wert nur zurück, wenn die Seite schon geschlossen ist.
            if in_flight.try_set(false).is_none() {
                flush(ctx, id, queue, in_flight, summary, rows, save_error, apply);
            }
        });
    }

    let enqueue = {
        let queue = queue.clone();
        move |p: &PosRow| {
            let Ok(ep) = parse_ep(&p.ep_text.get_untracked()) else {
                p.state.set(SaveState::Error("Keine gültige Zahl".into()));
                return;
            };
            if ep == p.saved.get_untracked() {
                if matches!(p.state.get_untracked(), SaveState::Error(_)) {
                    p.state.set(SaveState::Idle);
                }
                return;
            }
            // Sofort anzeigen; der Server rechnet GP verbindlich nach.
            p.gp.set(ep.map(|ep| p.menge.map_or(ep, |m| (m * ep).round_dp(2))));
            p.ep_text.set(ep.map(|d| fmt_de(d, 2)).unwrap_or_default());
            p.state.set(SaveState::Pending);
            {
                let mut q = queue.write_value();
                q.retain(|e| e.oz != p.oz);
                q.push(PriceEntry {
                    oz: p.oz.clone(),
                    ep,
                });
            }
            flush(
                ctx,
                id,
                queue.clone(),
                in_flight,
                summary,
                rows,
                save_error,
                apply,
            );
        }
    };

    let run_export = move |format: ExportFormat| {
        export_state.set(ExportState::Running(format));
        let id = id.get_value();
        spawn_local(async move {
            let s = match tauri::lv_export(&id, format.as_str()).await {
                Ok(saved) => ExportState::Done(saved),
                Err(e) => {
                    ctx.handle_error(&e);
                    ExportState::Failed(e.message)
                }
            };
            export_state.try_set(s);
        });
    };

    let render_row = move |row: Row| match row {
        Row::Title { depth, oz, label } => view! {
            <tr class=format!("title d{}", depth.min(3))>
                <td>{oz}</td><td colspan="5">{label}</td>
            </tr>
        }
        .into_any(),
        Row::Pos(p) => {
            let enqueue = enqueue.clone();
            let (p1, p2, p3, p4) = (p.clone(), p.clone(), p.clone(), p.clone());
            let counts = p.art.counts_towards_total();
            let has_long = !p.langtext.is_empty();
            let idx = p.idx;
            view! {
                <tr class="pos"
                    class:priced=move || p1.saved.get().is_some()
                    style:display=move || if only_open.get() && p2.saved.get().is_some() { "none" } else { "" }>
                    <td class="oz">{p.oz.to_string()}</td>
                    <td class="text" class:click=has_long on:click=move |_| if has_long { p3.open.update(|o| *o = !*o) }>
                        {art_label(p.art).map(|l| view! { <span class="badge">{l}</span>" " })}
                        {p.kurztext.clone()}
                        {move || (p4.open.get()).then(|| view! { <div class="long">{p4.langtext.clone()}</div> })}
                    </td>
                    <td class="num">{p.menge.map(|m| fmt_de(m, 0)).unwrap_or_default()}</td>
                    <td>{p.einheit.clone()}</td>
                    <td class="num">
                        <input class="ep" data-idx=idx inputmode="decimal" placeholder="—"
                            class:err=move || matches!(p.state.get(), SaveState::Error(_))
                            title=move || match p.state.get() { SaveState::Error(m) => m, _ => String::new() }
                            prop:value=move || p.ep_text.get()
                            on:input:target=move |ev| p.ep_text.set(ev.target().value())
                            on:change={
                                let (enqueue, p) = (enqueue.clone(), p.clone());
                                move |_| enqueue(&p)
                            }
                            on:keydown=move |ev| {
                                if ev.key() == "Enter" {
                                    ev.prevent_default();
                                    focus_ep(idx + 1);
                                }
                            }/>
                    </td>
                    <td class="num gp" class:muted=!counts>
                        {move || match (p.state.get(), p.gp.get()) {
                            (SaveState::Pending, _) => view! { <span class="spinner sm"></span> }.into_any(),
                            (_, Some(gp)) if counts => money(gp).into_any(),
                            (_, Some(gp)) => format!("({})", money(gp)).into_any(),
                            _ => "".into_any(),
                        }}
                    </td>
                </tr>
            }
            .into_any()
        }
    };

    view! {
        <div class="page offer">
            <div class="page-head">
                <div>
                    <button class="link" on:click=move |_| ctx.offer.set(None)>"← Alle Angebote"</button>
                    <h1>{move || summary.get().map(|s| s.name).unwrap_or_else(|| "LV".into())}</h1>
                    <div class="muted small">{move || summary.get().map(|s| format!("{} · {}", s.format, s.file_name))}</div>
                </div>
                <div class="totals">
                    <div class="sum">{move || summary.get().map(|s| format!("{} €", money(s.total)))}</div>
                    <div class="muted small">{move || summary.get().map(|s| format!("netto · {} von {} Positionen bepreist", s.priced, s.positions))}</div>
                </div>
            </div>
            {move || load_error.get().map(|m| view! {
                <div class="msg err">{m}" "<button class="btn secondary" on:click=move |_| load()>"Erneut laden"</button></div>
            })}
            {move || save_error.get().map(|m| view! {
                <div class="msg err">{m}" "<button class="btn secondary" on:click=move |_| load()>"Neu laden"</button></div>
            })}
            {move || (!warnings.get().is_empty()).then(|| view! {
                <details class="msg warn"><summary>{format!("Hinweise zum Import ({})", warnings.get().len())}</summary>
                    <ul>{warnings.get().into_iter().map(|w| view! { <li>{w}</li> }).collect_view()}</ul>
                </details>
            })}
            <div class="row toolbar">
                {move || exports.get().into_iter().map(|f| {
                    let label = match f {
                        ExportFormat::X84 => "Abgabe X84 erzeugen",
                        ExportFormat::D84 => "Abgabe D84 erzeugen",
                        ExportFormat::Xlsx => "Excel erzeugen",
                    };
                    view! {
                        <button class="btn" class:secondary=f == ExportFormat::Xlsx
                            disabled=move || in_flight.get() || matches!(export_state.get(), ExportState::Running(_))
                            on:click=move |_| run_export(f)>{label}</button>
                    }
                }).collect_view()}
                <label class="check"><input type="checkbox" prop:checked=move || only_open.get()
                    on:change:target=move |ev| only_open.set(ev.target().checked())/>" nur ohne Preis"</label>
                {move || in_flight.get().then(|| view! { <span class="muted small"><span class="spinner sm"></span>" speichert …"</span> })}
            </div>
            {move || match export_state.get() {
                ExportState::Idle => None,
                ExportState::Running(_) => Some(view! { <div class="msg ok"><span class="spinner sm"></span>" Datei wird erzeugt …"</div> }.into_any()),
                ExportState::Failed(m) => Some(view! { <div class="msg err">{m}</div> }.into_any()),
                ExportState::Done(saved) => {
                    let path = saved.path.clone();
                    let missing = saved.missing_prices.len();
                    Some(view! {
                        <div class="msg ok">
                            "Gespeichert: "<strong>{saved.path.clone()}</strong>" "
                            <button class="btn secondary" on:click=move |_| {
                                let path = path.clone();
                                spawn_local(async move { let _ = tauri::show_in_folder(&path).await; });
                            }>"Im Ordner zeigen"</button>
                            {(missing > 0).then(|| view! {
                                <div class="warn-line">{format!("{missing} Positionen ohne Preis: {}{}",
                                    saved.missing_prices.iter().take(8).cloned().collect::<Vec<_>>().join(", "),
                                    if missing > 8 { " …" } else { "" })}</div>
                            })}
                        </div>
                    }.into_any())
                }
            }}
            <table class="lv">
                <thead><tr>
                    <th>"OZ"</th><th>"Kurztext"</th><th class="num">"Menge"</th><th>"Einheit"</th>
                    <th class="num">"EP €"</th><th class="num">"GP €"</th>
                </tr></thead>
                <tbody>
                    {move || rows.get().into_iter().map(render_row.clone()).collect_view()}
                </tbody>
            </table>
            {move || summary.get().is_none().then(|| view! { <p class="muted"><span class="spinner"></span>" Lade …"</p> })}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn german_numbers() {
        assert_eq!(fmt_de(Decimal::new(123456789, 2), 2), "1.234.567,89");
        assert_eq!(fmt_de(Decimal::new(125, 1), 0), "12,5");
        assert_eq!(fmt_de(Decimal::new(12345, 3), 2), "12,345");
        assert_eq!(fmt_de(Decimal::new(-1000, 0), 2), "-1.000,00");
        assert_eq!(parse_ep("1.234,5"), Ok(Some(Decimal::new(12345, 1))));
        assert_eq!(parse_ep(" 12,30 € "), Ok(Some(Decimal::new(1230, 2))));
        assert_eq!(parse_ep(""), Ok(None));
        assert_eq!(parse_ep("abc"), Err(()));
        assert_eq!(parse_ep("-3"), Err(()));
    }
}
