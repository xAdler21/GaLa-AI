//! Update-Anzeige: Pruefen, Version und Notizen, Download mit Fortschritt, Installation.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::state::{Ctx, UpdateState, check_update, install_update};

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

#[component]
pub fn UpdatePanel(#[prop(optional)] auto_check: bool) -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    if auto_check && matches!(ctx.update.get_untracked(), UpdateState::Idle | UpdateState::UpToDate) {
        spawn_local(check_update(ctx));
    }

    let busy = move || matches!(ctx.update.get(), UpdateState::Checking | UpdateState::Downloading(..));

    view! {
        <div>
            {move || match ctx.update.get() {
                UpdateState::Idle => view! { <p class="muted">"Noch nicht nach Updates gesucht."</p> }.into_any(),
                UpdateState::Checking => view! { <p><span class="spinner"></span>" Suche nach Updates …"</p> }.into_any(),
                UpdateState::UpToDate => view! { <div class="msg ok">"GaLa-AI ist auf dem neuesten Stand."</div> }.into_any(),
                UpdateState::Failed(m) => view! { <div class="msg err">{m}</div> }.into_any(),
                UpdateState::Available(info) => {
                    let date = info.date.as_deref().map(|d| d.chars().take(10).collect::<String>());
                    view! {
                        <div>
                            <p><strong>{format!("Version {} ist verfügbar", info.version)}</strong>
                               {date.map(|d| format!(" (vom {d})"))}</p>
                            {info.notes.map(|n| view! { <div class="notes">{n}</div> })}
                            <button class="btn" on:click=move |_| { spawn_local(install_update(ctx)); }>
                                "Herunterladen und installieren"
                            </button>
                        </div>
                    }.into_any()
                }
                UpdateState::Downloading(done, total) => {
                    let pct = total.filter(|t| *t > 0).map(|t| (done as f64 / t as f64 * 100.0).min(100.0));
                    let label = match total {
                        Some(t) => format!("{} von {}", mb(done), mb(t)),
                        None => mb(done),
                    };
                    view! {
                        <div>
                            <p>"Update wird geladen. Die App startet danach neu."</p>
                            <div class="progress" class:indeterminate=pct.is_none()>
                                <div style=format!("width:{}%", pct.unwrap_or(35.0))></div>
                            </div>
                            <p class="muted small">{label}</p>
                        </div>
                    }.into_any()
                }
            }}
            <div class="row" style="margin-top:.5rem">
                <button class="btn secondary" disabled=busy on:click=move |_| { spawn_local(check_update(ctx)); }>
                    "Nach Updates suchen"
                </button>
            </div>
        </div>
    }
}
