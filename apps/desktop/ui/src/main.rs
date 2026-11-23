//! GaLa-AI Desktop-Oberflaeche (Leptos CSR). Haengt die App ein und waehlt die Ansicht je nach Zustand.

mod chat;
mod offers;
mod screens;
mod shell;
mod state;
mod tauri;
mod updates;

use leptos::prelude::*;
use leptos::task::spawn_local;

use screens::{
    BootScreen, ForceUpdateScreen, LoginScreen, MaintenanceScreen, SetupScreen, UnreachableScreen,
};
use shell::Shell;
use state::{Ctx, Screen, UpdateState, health_loop, start};

#[component]
fn App() -> impl IntoView {
    let ctx = Ctx::new();
    provide_context(ctx);

    tauri::on_update_progress(move |done, total| {
        if matches!(ctx.update.get_untracked(), UpdateState::Downloading(..)) {
            ctx.update.set(UpdateState::Downloading(done, total));
        }
    });
    spawn_local(start(ctx));
    spawn_local(health_loop(ctx));

    // Memos: Die Health-Abfrage alle 30 s darf nicht die ganze Oberflaeche neu aufbauen
    // (sonst verschwindet z. B. das offene LV kurz und wird neu geladen).
    let required = Memo::new(move |_| ctx.update_required());
    let maintenance = Memo::new(move |_| ctx.maintenance().is_some());
    move || {
        let screen = ctx.screen.get();
        // Pflicht-Update und Wartung sperren alles, ausser Einrichtung und Start.
        // Die Wartung sperrt nur die angemeldete Ansicht, damit Anmeldung und Server-Wechsel moeglich bleiben.
        if !matches!(screen, Screen::Boot | Screen::Setup) && required.get() {
            return view! { <ForceUpdateScreen/> }.into_any();
        }
        if screen == Screen::Main && maintenance.get() {
            return view! { <MaintenanceScreen/> }.into_any();
        }
        match screen {
            Screen::Boot => view! { <BootScreen/> }.into_any(),
            Screen::Setup => view! { <SetupScreen/> }.into_any(),
            Screen::Login => view! { <LoginScreen/> }.into_any(),
            Screen::Main => view! { <Shell/> }.into_any(),
            Screen::Unreachable => view! { <UnreachableScreen/> }.into_any(),
        }
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
