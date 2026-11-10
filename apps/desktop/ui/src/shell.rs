//! Hauptansicht nach der Anmeldung: Seitenleiste, Banner, Seiten (Chat, Angebote, Einstellungen).

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::chat::ChatPage;
use crate::state::{Ctx, Page, Screen, UpdateState};
use crate::tauri;
use crate::updates::UpdatePanel;

#[component]
pub fn Shell() -> impl IntoView {
    let ctx = expect_context::<Ctx>();

    let nav = move |page: Page, label: &'static str| {
        view! {
            <button class:active=move || ctx.page.get() == page on:click=move |_| ctx.page.set(page)>{label}</button>
        }
    };

    view! {
        <div class="shell">
            <aside class="sidebar">
                <div class="brand"><div class="logo">"GL"</div><span>"GaLa-AI"</span></div>
                <nav class="nav">
                    {nav(Page::Chat, "Chat")}
                    {nav(Page::Offers, "Angebote")}
                    {nav(Page::Settings, "Einstellungen")}
                </nav>
                <div class="side-foot">
                    {move || ctx.user.get().map(|u| view! { <div>{u.username}</div> })}
                    <div>{move || format!("Version {}", ctx.version.get())}</div>
                </div>
            </aside>
            <main class="content">
                {move || ctx.offline.get().then(|| view! {
                    <div class="banner">"Der Server ist gerade nicht erreichbar. Es wird weiter versucht."</div>
                })}
                {move || match ctx.update.get() {
                    UpdateState::Available(info) => Some(view! {
                        <div class="banner info">
                            <span>{format!("Version {} ist verfügbar.", info.version)}</span>
                            <button class="btn secondary" on:click=move |_| ctx.page.set(Page::Settings)>"Details"</button>
                        </div>
                    }),
                    _ => None,
                }}
                {move || match ctx.page.get() {
                    Page::Chat => view! { <ChatPage/> }.into_any(),
                    Page::Offers => view! { <OffersPage/> }.into_any(),
                    Page::Settings => view! { <SettingsPage/> }.into_any(),
                }}
            </main>
        </div>
    }
}

#[component]
fn OffersPage() -> impl IntoView {
    view! {
        <div class="page">
            <h1>"Angebote"</h1>
            <p class="muted">"Die Angebotserstellung kommt mit M1."</p>
        </div>
    }
}

#[component]
fn SettingsPage() -> impl IntoView {
    let ctx = expect_context::<Ctx>();

    let logout = move |_| {
        spawn_local(async move {
            let _ = tauri::logout().await;
            ctx.user.set(None);
            ctx.chat.set(Vec::new());
            ctx.notice.set(None);
            ctx.screen.set(Screen::Login);
        });
    };

    view! {
        <div class="page">
            <h1>"Einstellungen"</h1>
            <div class="panel">
                <h2>"Server und Version"</h2>
                <dl class="kv">
                    <dt>"Server"</dt><dd>{move || ctx.server.get()}</dd>
                    <dt>"Benutzer"</dt><dd>{move || ctx.user.get().map(|u| u.username).unwrap_or_default()}</dd>
                    <dt>"App-Version"</dt><dd>{move || ctx.version.get()}</dd>
                    <dt>"Server-Version"</dt>
                    <dd>{move || ctx.health.get().map(|h| h.server_version).unwrap_or_else(|| "unbekannt".to_owned())}</dd>
                </dl>
                <button class="btn secondary" on:click=move |_| ctx.screen.set(Screen::Setup)>"Server ändern"</button>
            </div>
            <div class="panel">
                <h2>"Updates"</h2>
                <UpdatePanel/>
            </div>
            <div class="panel">
                <h2>"Konto"</h2>
                <button class="btn danger" on:click=logout>"Abmelden"</button>
            </div>
        </div>
    }
}
