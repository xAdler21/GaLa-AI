//! Dokumenten-Chatbot: Frage, Verlauf, Antwort mit Quellen.

use api_types::client::{ChatResponse, ChatTurn};
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::state::{ChatEntry, ChatState, Ctx};
use crate::tauri;

const MSG_NOT_IMPLEMENTED: &str = "Der Dokumenten-Chatbot ist noch nicht eingerichtet.";
const MSG_LLM_UNAVAILABLE: &str = "Das Sprachmodell ist gerade nicht erreichbar.";

/// Bisherige beantwortete Runden, aelteste zuerst.
fn history(ctx: Ctx) -> Vec<ChatTurn> {
    ctx.chat
        .get_untracked()
        .into_iter()
        .filter_map(|e| match e.state.get_untracked() {
            ChatState::Done(r) => Some(ChatTurn { question: e.question.get_value(), answer: r.answer }),
            _ => None,
        })
        .collect()
}

fn ask(ctx: Ctx, question: String) {
    let hist = history(ctx);
    let entry = ChatEntry {
        id: ctx.chat.with_untracked(|c| c.last().map_or(0, |e| e.id + 1)),
        question: StoredValue::new(question.clone()),
        state: RwSignal::new(ChatState::Pending),
    };
    ctx.chat.update(|c| c.push(entry));
    spawn_local(async move {
        let state = match tauri::chat(&question, hist).await {
            Ok(r) => ChatState::Done(r),
            Err(e) => {
                ctx.handle_error(&e);
                match e.code.as_str() {
                    "not_implemented" => ChatState::NotImplemented,
                    "llm_unavailable" => ChatState::Failed(MSG_LLM_UNAVAILABLE.to_owned()),
                    _ => ChatState::Failed(e.message),
                }
            }
        };
        entry.state.set(state);
    });
}

#[component]
pub fn ChatPage() -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    let input = RwSignal::new(String::new());
    let scroller = NodeRef::<html::Div>::new();

    let pending = move || ctx.chat.get().iter().any(|e| e.state.get() == ChatState::Pending);
    let send = move || {
        let q = input.get_untracked().trim().to_owned();
        if q.is_empty() || pending() {
            return;
        }
        input.set(String::new());
        ask(ctx, q);
    };

    // Nach jeder neuen Frage oder Antwort ans Ende scrollen.
    Effect::new(move |_| {
        ctx.chat.with(|c| {
            for e in c {
                e.state.track();
            }
        });
        if let Some(el) = scroller.get() {
            el.set_scroll_top(el.scroll_height());
        }
    });

    view! {
        <div class="page chat">
            <div class="chat-head">
                <h1>"Dokumenten-Chat"</h1>
                <button class="btn secondary" disabled=move || ctx.chat.get().is_empty()
                    on:click=move |_| ctx.chat.set(Vec::new())>"Neue Unterhaltung"</button>
            </div>
            <div class="chat-scroll" node_ref=scroller>
                {move || ctx.chat.get().is_empty().then(|| view! {
                    <div class="empty">"Stellen Sie eine Frage zu Ihren Dokumenten, zum Beispiel zu alten Angeboten."</div>
                })}
                <For each=move || ctx.chat.get() key=|e| e.id let(entry)>
                    <Turn entry/>
                </For>
            </div>
            <div class="composer"><div class="inner">
                <textarea rows="2" placeholder="Frage eingeben (Enter sendet, Umschalt+Enter ergibt eine neue Zeile)"
                    prop:value=move || input.get()
                    on:input:target=move |ev| input.set(ev.target().value())
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" && !ev.shift_key() {
                            ev.prevent_default();
                            send();
                        }
                    }></textarea>
                <button class="btn" disabled=move || input.get().trim().is_empty() || pending()
                    on:click=move |_| send()>"Senden"</button>
            </div></div>
        </div>
    }
}

#[component]
fn Turn(entry: ChatEntry) -> impl IntoView {
    view! {
        <div class="turn">
            <div class="bubble q">{entry.question.get_value()}</div>
            {move || match entry.state.get() {
                ChatState::Pending => view! {
                    <div class="bubble a"><span class="spinner"></span>" Antwort wird gesucht …"</div>
                }.into_any(),
                ChatState::Done(r) => view! { <Answer resp=r/> }.into_any(),
                ChatState::NotImplemented => view! { <div class="msg warn">{MSG_NOT_IMPLEMENTED}</div> }.into_any(),
                ChatState::Failed(m) => view! { <div class="msg err">{m}</div> }.into_any(),
            }}
        </div>
    }
}

#[component]
fn Answer(resp: ChatResponse) -> impl IntoView {
    let n = resp.sources.len();
    view! {
        <div class="bubble a">{resp.answer}</div>
        {(n > 0).then(|| view! {
            <details class="sources" open>
                <summary>{format!("Quellen ({n})")}</summary>
                {resp.sources.into_iter().map(|s| {
                    let place = s.page.map(|p| format!("Seite {p}"));
                    view! {
                        <div class="source">
                            <div class="path">{s.path}{place.map(|p| format!(", {p}"))}</div>
                            <div class="snip">{s.snippet}</div>
                        </div>
                    }
                }).collect_view()}
            </details>
        })}
    }
}
