//! Vollbild-Ansichten: Einrichtung, Anmeldung, Wartungssperre, Pflicht-Update, Server nicht erreichbar.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::state::{Ctx, Page, Screen, start};
use crate::tauri;
use crate::updates::UpdatePanel;

#[component]
fn Brand() -> impl IntoView {
    view! { <div class="brand"><div class="logo">"GL"</div><span>"GaLa-AI"</span></div> }
}

#[component]
pub fn BootScreen() -> impl IntoView {
    view! { <div class="center"><div class="spinner"></div></div> }
}

#[component]
pub fn SetupScreen() -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    let initial = ctx.server.get_untracked();
    let url = RwSignal::new(if initial.is_empty() { "https://gala-coordinator".to_owned() } else { initial });
    let status = RwSignal::new(None::<(bool, String)>);
    let busy = RwSignal::new(false);
    let can_cancel = !ctx.server.get_untracked().is_empty();

    let test = move |_| {
        busy.set(true);
        status.set(None);
        spawn_local(async move {
            match tauri::test_connection(&url.get_untracked()).await {
                Ok(h) => status.set(Some((true, format!("Verbindung steht. Server-Version {}.", h.server_version)))),
                Err(e) => status.set(Some((false, e.message))),
            }
            busy.set(false);
        });
    };
    let save = move |_| {
        busy.set(true);
        spawn_local(async move {
            match tauri::save_server_url(&url.get_untracked()).await {
                Ok(_) => start(ctx).await,
                Err(e) => status.set(Some((false, e.message))),
            }
            busy.set(false);
        });
    };
    let cancel = move |_| {
        ctx.screen.set(if ctx.user.get_untracked().is_some() { Screen::Main } else { Screen::Login });
    };

    view! {
        <div class="center"><div class="card">
            <Brand/>
            <h1>"Server einrichten"</h1>
            <p class="muted">"Adresse des GaLa-AI-Servers im Firmennetz."</p>
            <div class="field">
                <label for="server">"Server-Adresse"</label>
                <input id="server" type="text" prop:value=move || url.get()
                    on:input:target=move |ev| url.set(ev.target().value())/>
            </div>
            {move || status.get().map(|(ok, text)| view! { <div class="msg" class:ok=ok class:err=!ok>{text}</div> })}
            <div class="row">
                <button class="btn secondary" disabled=move || busy.get() on:click=test>"Verbindung testen"</button>
                <button class="btn" disabled=move || busy.get() on:click=save>"Speichern und weiter"</button>
                {can_cancel.then(|| view! { <button class="btn secondary" on:click=cancel>"Abbrechen"</button> })}
            </div>
        </div></div>
    }
}

#[component]
pub fn LoginScreen() -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    let username = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        busy.set(true);
        error.set(None);
        ctx.notice.set(None);
        spawn_local(async move {
            match tauri::login(&username.get_untracked(), &password.get_untracked()).await {
                Ok(user) => {
                    password.set(String::new());
                    ctx.user.set(Some(user));
                    ctx.page.set(Page::Chat);
                    ctx.screen.set(Screen::Main);
                }
                Err(e) => {
                    ctx.handle_error(&e);
                    error.set(Some(e.message));
                }
            }
            busy.set(false);
        });
    };

    view! {
        <div class="center"><form class="card" on:submit=submit>
            <Brand/>
            <h1>"Anmelden"</h1>
            <p class="muted">{move || ctx.server.get()}</p>
            {move || ctx.notice.get().map(|n| view! { <div class="msg warn">{n}</div> })}
            {move || error.get().map(|e| view! { <div class="msg err">{e}</div> })}
            <div class="field">
                <label for="user">"Benutzername"</label>
                <input id="user" type="text" autocomplete="username" prop:value=move || username.get()
                    on:input:target=move |ev| username.set(ev.target().value())/>
            </div>
            <div class="field">
                <label for="pw">"Passwort"</label>
                <input id="pw" type="password" autocomplete="current-password" prop:value=move || password.get()
                    on:input:target=move |ev| password.set(ev.target().value())/>
            </div>
            <div class="row">
                <button class="btn" type="submit"
                    disabled=move || busy.get() || username.get().trim().is_empty() || password.get().is_empty()>
                    "Anmelden"
                </button>
                <button class="btn secondary" type="button" on:click=move |_| ctx.screen.set(Screen::Setup)>
                    "Server ändern"
                </button>
            </div>
        </form></div>
    }
}

#[component]
pub fn MaintenanceScreen() -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    let message = move || {
        ctx.maintenance()
            .and_then(|m| m.message)
            .unwrap_or_else(|| "Der Server wird gerade gewartet.".to_owned())
    };
    view! {
        <div class="center"><div class="card lock">
            <div class="icon">"🔒"</div>
            <h1>"Wartungsarbeiten"</h1>
            <p>{message}</p>
            <p class="muted small">"Die App entsperrt sich automatisch, sobald die Wartung beendet ist."</p>
            <div class="spinner"></div>
        </div></div>
    }
}

#[component]
pub fn ForceUpdateScreen() -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    let min = move || ctx.health.get().and_then(|h| h.min_client_version).unwrap_or_default();
    view! {
        <div class="center"><div class="card wide">
            <Brand/>
            <h1>"Update erforderlich"</h1>
            <p>
                {move || format!("Der Server benötigt mindestens Version {}. Installiert ist {}.", min(), ctx.version.get())}
            </p>
            <UpdatePanel auto_check=true/>
        </div></div>
    }
}

#[component]
pub fn UnreachableScreen() -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    view! {
        <div class="center"><div class="card">
            <Brand/>
            <h1>"Server nicht erreichbar"</h1>
            <div class="msg err">{move || ctx.fatal.get()}</div>
            <div class="row">
                <button class="btn" on:click=move |_| { spawn_local(start(ctx)); }>"Erneut versuchen"</button>
                <button class="btn secondary" on:click=move |_| ctx.screen.set(Screen::Setup)>"Server ändern"</button>
            </div>
        </div></div>
    }
}
