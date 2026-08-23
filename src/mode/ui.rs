//! Windows onto afpay, in AFUI's two shapes.
//!
//! `ui wallet` and `ui receive` are *watch* sessions: afpay opens a window onto
//! something a person reads, nothing on the page submits, and the session ends
//! when they close it. Their value type is `()` and `Outcome::Completed` never
//! occurs.
//!
//! `ui send` is a *decide* session. It shows a resolved payment and returns the
//! person's answer as a typed value, and only one answer sends: closing the
//! window, letting a credential lapse, and pressing the refuse button are all
//! refusals. Absence of an answer is never consent — see [`approved`], which is
//! the single place that judgement is made.
//!
//! The data is not a second source of truth. `args::dispatch` builds each
//! panel's request through the very same `invocation_to_input` arm the matching
//! command uses, and this module runs it through the same handler, store,
//! providers, spend ledger, and idempotency. The pages then render afpay's *own
//! emitted event* — already redacted by `output_fmt::protocol_event` — rather
//! than reaching back into the typed structs, so a `*_secret` field cannot
//! reach a window by a route the agent-facing output does not also take.
//!
//! None of these pages is written in Rust. Each is a MiniJinja template
//! rendered against the typed document its section below builds, and a person
//! may replace any of those templates — see [`frontend`]. The documents are
//! therefore a contract rather than an implementation detail: what a panel
//! computes is afpay's, what a panel looks like is not.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use agent_first_data::OutputFormat;
use agent_first_ui::{
    Outcome, UiCompletion, UiCspNonce, UiDeliveryMode, UiDeliveryPlan, UiPagePolicy, UiPageScript,
    UiSession,
};
use axum::Router;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;

mod frontend;

use frontend::{FrontendFailure, PROVIDER_ID, PanelFrontend, PanelShape};

use crate::args::UiInit;
use crate::config;
use crate::handler::{self, App};
use crate::store;
use crate::types::{Input, Network, Output, Request, RuntimeConfig};

/// One panel shape per kind, so a person can replace exactly the panel they
/// mean with `afui frontend` without affecting the others.
const WALLET_UI_KIND: &str = "wallet_inspect";
const RECEIVE_UI_KIND: &str = "receive_inspect";
const SEND_UI_KIND: &str = "send_confirm";

/// A watch panel loads a stylesheet, images and fonts from this session and
/// nothing else. No script at all: these pages have no behaviour, and neither
/// afpay nor a frontend can give them any.
///
/// Images come from this session because the receive code is one afpay serves
/// on its own route, and fonts because a frontend may ship them — AFUI serves
/// those from the same origin under the same credential. Same origin, same
/// credential, no network.
fn watch_page_policy() -> UiPagePolicy {
    UiPagePolicy::new(UiPageScript::None)
        .allow_images()
        .allow_fonts()
}

/// The confirm panel differs from a watch panel in exactly two switches, and
/// both are what make its answer trustworthy rather than what weaken it.
///
/// The nonce admits precisely one script — afpay's decision runtime, spliced in
/// at the layout marker with this session's nonce. A frontend cannot supply a
/// script file (AFUI refuses one by name) or hide one in a template (refused by
/// content), and could not run it if it did: it does not know the nonce. Form
/// submission is where the answer goes.
fn confirm_page_policy(nonce: &UiCspNonce) -> UiPagePolicy {
    watch_page_policy()
        .with_script(UiPageScript::Nonce(nonce.clone()))
        .allow_form_submission()
}

const OUTPUT_CHANNEL_CAPACITY: usize = 4096;

/// Where the receive panel's code is served from. A route rather than markup in
/// the document: afpay owns what the code encodes, a template owns where it sits
/// and how large it is, and the page keeps its unconditional HTML escaping.
const QR_PATH: &str = "qr.svg";

/// Wallet fields the card lays out by hand; everything else a wallet carries is
/// rendered generically so a new provider field shows up rather than vanishing.
const WALLET_PLACED_KEYS: [&str; 6] = ["id", "network", "label", "address", "balance", "error"];

/// Balance fields every network reports. `BalanceInfo` flattens provider extras
/// (`fee_credit_sats` and friends) alongside them, so the rest are rendered too.
const BALANCE_PLACED_KEYS: [&str; 3] = ["confirmed", "pending", "unit"];

/// The three payable strings the receive card lays out by hand. Extras are read
/// from inside `receive_info` rather than from the event around it, so a field a
/// provider starts returning shows up as its own row instead of being buried in
/// a re-print of the whole object.
const RECEIVE_PLACED_KEYS: [&str; 3] = ["address", "invoice", "quote_id"];

/// Plan fields the confirm card lays out by hand. Everything else `pay_planned`
/// carries — the expiry, the operation, the network — is rendered generically
/// below them, so a field afpay starts recording on a plan reaches the person
/// deciding about it rather than only the agent.
const PLAN_PLACED_KEYS: [&str; 9] = [
    "code",
    "id",
    "wallet",
    "to",
    "amount_native",
    "fee_estimate_native",
    "fee_unit",
    "onchain_memo",
    "spend_debits",
];

pub async fn run(init: UiInit) {
    let UiInit {
        input,
        delivery,
        output,
        log,
        data_dir,
        startup_argv,
        startup_args,
        startup_requested,
    } = init;

    let resolved_dir = data_dir.unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let mut runtime = match RuntimeConfig::load_from_dir(&resolved_dir) {
        Ok(runtime) => runtime,
        Err(error) => fail("config_invalid", &error, None, output),
    };
    if !log.is_empty() {
        runtime.log = log.clone();
    }

    let (tx, rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    let backend = store::create_storage_backend(&runtime);
    let app = Arc::new(App::new(runtime, tx, None, backend));

    let log_filters = agent_first_data::LogFilters::new(log);
    {
        let runtime = app.config.read().await;
        if let Some(event) = config::maybe_startup_log(
            &log_filters,
            startup_requested,
            Some(startup_argv),
            Some(&*runtime),
            startup_args,
        ) {
            emit_or_exit(&event, output);
        }
    }

    match input {
        Input::Balance { .. } => wallet_panel(app, input, rx, &log_filters, output, delivery).await,
        Input::Receive { .. } => {
            receive_panel(app, input, rx, &log_filters, output, delivery).await
        }
        Input::SendPlan { .. } | Input::CashuSendPlan { .. } => {
            send_panel(app, input, rx, &log_filters, output, delivery).await
        }
        // `dispatch` routes only the three `ui` actions here, and each builds
        // one of the requests above. Naming the mismatch honestly beats
        // asserting the invariant.
        _ => fail(
            "ui_request_unsupported",
            "this request has no panel",
            Some("`afpay ui --help` lists the panels afpay opens"),
            output,
        ),
    }
}

// ═══════════════════════════════════════════
// Shared session plumbing
// ═══════════════════════════════════════════

/// Run the panel's request and drain everything it produced.
///
/// Draining before anything is drawn is the point: a store or provider failure
/// must reach the agent as an error event, not as an empty window. Note this is
/// about the *request* failing — a single unreachable wallet is not an error
/// here, it arrives inside the result and the page renders it as such.
///
/// The one output the page is a view of is handed back; every other event goes
/// to the agent unchanged.
async fn run_and_drain(
    app: Arc<App>,
    input: Input,
    mut rx: mpsc::Receiver<Output>,
    log_filters: &agent_first_data::LogFilters,
    output: OutputFormat,
    wanted: fn(&Output) -> bool,
) -> Option<Output> {
    app.requests_total.fetch_add(1, Ordering::Relaxed);
    handler::dispatch(&app, Request::from_input(input)).await;
    drop(app);

    let mut kept = None;
    let mut failed = false;
    while let Some(out) = rx.recv().await {
        if wanted(&out) {
            kept = Some(out);
            continue;
        }
        match out {
            Output::Error { .. } => {
                failed = true;
                emit_or_exit(&out, output);
            }
            Output::Log { ref event, .. } if !log_filters.enabled(event) => {}
            other => emit_or_exit(&other, output),
        }
    }
    if failed {
        std::process::exit(1);
    }
    kept
}

fn window_hint(error: &agent_first_ui::Error) -> Option<&'static str> {
    // Keyed on AFUI's classification rather than one variant, so a window that
    // could not launch gets the same way out as one that was never found.
    // afpay's own alternative replaces AFUI's: the useful thing to say is not
    // only "install a browser" but "you do not need one for this".
    if error.kind() == agent_first_ui::UiErrorKind::WindowUnavailable {
        return Some("install a Chromium-family browser, or run the same request as a command");
    }
    error.hint()
}

fn fail(code: &str, message: &str, hint: Option<&str>, output: OutputFormat) -> ! {
    let event = crate::output_fmt::coded_error_event(code, message, hint);
    if crate::output_fmt::emit_process_event(event, output).is_err() {
        std::process::exit(4);
    }
    std::process::exit(1);
}

/// A frontend that will not load ends the command. It is never a quietly
/// substituted afpay page, because that is indistinguishable from the override
/// having worked.
fn fail_frontend(failure: FrontendFailure, output: OutputFormat) -> ! {
    fail(failure.code, &failure.message, failure.hint, output)
}

fn emit_or_exit(output_event: &Output, format: OutputFormat) {
    if crate::output_fmt::emit_process_output(output_event, format).is_err() {
        std::process::exit(4);
    }
}

fn emit_event_or_exit(value: Value, format: OutputFormat) {
    if crate::output_fmt::emit_process_event(value, format).is_err() {
        std::process::exit(4);
    }
}

/// The window is about to appear, and `window()` does not return until the
/// person is done with it, so readiness goes out first or not at all.
///
/// `ui_frontend_id` is present only when an override is actually serving. A
/// workspace frontend that has not been trusted is skipped in silence by
/// design, so its absence here is how an agent tells "my override is running"
/// from "my override is inert" without opening a window to look.
/// The plan for one panel, from the same offer its `--mode` flag was built
/// from — so a word a person read in `--help` is a word this honours.
///
/// Why the two offers differ is where they are declared, beside the flag.
fn delivery_plan(
    offer: agent_first_ui::cli::UiDeliveryOffer,
    explicit: Option<UiDeliveryMode>,
    output: OutputFormat,
) -> UiDeliveryPlan {
    match offer.resolve(explicit) {
        Ok(plan) => plan,
        Err(error) => fail("ui_delivery_invalid", &error.to_string(), None, output),
    }
}

/// One readiness event, with AFUI's delivery facts spliced in beside afpay's.
///
/// The facts are taken rather than restated, and the link URL arrives under a
/// name the emitter will not mask: handing that URL over is the entire point
/// of `link` delivery, and a `_secret`-suffixed field would reach a person as
/// `***` — a panel announcing it is reachable at nothing.
fn emit_ready(
    fields: serde_json::Map<String, Value>,
    facts: &agent_first_ui::UiDeliveryFacts,
    frontend: &PanelFrontend,
    format: OutputFormat,
) {
    let mut fields = fields;
    fields.insert("phase".to_string(), Value::from("ui_ready"));
    if let Some(frontend_id) = frontend.frontend_id() {
        fields.insert("ui_frontend_id".to_string(), Value::from(frontend_id));
    }
    let event = agent_first_ui::cli::ready_event_revealing_link(facts, Value::Object(fields));
    emit_event_or_exit(
        agent_first_data::json_progress(event).build().into_value(),
        format,
    );
}

/// The routes every panel serves besides its own page.
///
/// The stylesheet is read once, when the panel starts, rather than per request:
/// a frontend's bytes are fixed for the life of a window — editing it revokes
/// its trust anyway — and reading it here is what turns an unreadable
/// stylesheet into a failure before a window opens rather than into a page with
/// no styling.
fn shared_panel_routes(frontend: &PanelFrontend, stylesheet: Vec<u8>) -> Router {
    agent_first_ui::page_asset_routes(stylesheet, frontend.frontend())
}

fn network_label(network: Network) -> String {
    network.to_string()
}

// ═══════════════════════════════════════════
// `ui wallet` — every wallet and its balance
// ═══════════════════════════════════════════

async fn wallet_panel(
    app: Arc<App>,
    input: Input,
    rx: mpsc::Receiver<Output>,
    log_filters: &agent_first_data::LogFilters,
    output: OutputFormat,
    delivery: Option<UiDeliveryMode>,
) -> ! {
    // Before the first provider call, so a frontend afpay cannot load costs a
    // person an error rather than a round trip, a window, and then an error.
    let frontend = match PanelFrontend::resolve(WALLET_UI_KIND, PanelShape::Wallet) {
        Ok(frontend) => frontend,
        Err(failure) => fail_frontend(failure, output),
    };
    let app_icon = match frontend.app_icon() {
        Ok(app_icon) => app_icon,
        Err(failure) => fail_frontend(failure, output),
    };
    let subject = wallet_subject_of(&input);
    let balances = run_and_drain(app, input, rx, log_filters, output, |out| {
        matches!(out, Output::WalletBalances { .. })
    })
    .await;
    let Some(balances) = balances else {
        fail(
            "ui_no_result",
            "the balance request produced no result to show",
            Some("run the matching `afpay balance` command to see what it reports"),
            output,
        );
    };

    let result = match event_result(&balances) {
        Ok(result) => result,
        Err(error) => fail("ui_render_failed", &error, None, output),
    };
    let document = wallet_document(&result, &subject);
    let (wallet_count, unreachable_count) = (document.wallet_count, document.unreachable_count);
    let stylesheet = match frontend.stylesheet() {
        Ok(bytes) => bytes,
        Err(failure) => fail_frontend(failure, output),
    };
    let page = match frontend.render_page(&document, None) {
        Ok(page) => page,
        Err(failure) => fail_frontend(failure, output),
    };
    let router = Router::new()
        .route("/", get(move || async move { Html(page) }))
        .merge(shared_panel_routes(&frontend, stylesheet));

    // The subject is what tells two panels apart in `afui session list` — an
    // all-wallets view from one filtered to a single network or wallet.
    let session = match UiSession::<()>::new(PROVIDER_ID, WALLET_UI_KIND) {
        Ok(session) => session
            .with_subject(subject.clone())
            .with_app_icon(app_icon)
            .with_security_policy(watch_page_policy().into_security_policy()),
        Err(error) => fail("ui_session_invalid", &error.to_string(), None, output),
    };

    let active = match delivery_plan(crate::args::WATCH_PANEL_DELIVERY, delivery, output)
        .start(session, router)
        .await
    {
        Ok(active) => active,
        Err(error) => fail("ui_failed", &error.to_string(), window_hint(&error), output),
    };
    let facts = active.facts();

    emit_ready(
        json_fields(serde_json::json!({
            "message": format!("The wallet panel is ready: {}.", facts.mode.description()),
            "ui_kind": WALLET_UI_KIND,
            "subject": subject,
            "wallet_count": wallet_count,
            "unreachable_count": unreachable_count,
        })),
        &facts,
        &frontend,
        output,
    );

    // A watch panel has no submit control, so every ending means the person is
    // done looking — including a link nobody attended, which AFUI lets lapse.
    // The word for which ending it was is AFUI's; afpay only reports it.
    match active.wait().await {
        Ok(outcome) => {
            emit_event_or_exit(
                agent_first_data::json_result(serde_json::json!({
                    "code": "ui_closed",
                    "ui_kind": WALLET_UI_KIND,
                    "subject": subject,
                    "ending": outcome.ending(),
                    "wallet_count": wallet_count,
                    "unreachable_count": unreachable_count,
                }))
                .build()
                .into_value(),
                output,
            );
            std::process::exit(0);
        }
        Err(error) => fail("ui_failed", &error.to_string(), window_hint(&error), output),
    }
}

/// What this panel is a view of, phrased the way the person asked for it.
fn wallet_subject_of(input: &Input) -> String {
    let Input::Balance {
        wallet,
        network,
        check,
        ..
    } = input
    else {
        return "wallets".to_string();
    };
    let mut subject = match (wallet, network) {
        (Some(wallet), _) => wallet.clone(),
        (None, Some(network)) => format!("every {} wallet", network_label(*network)),
        (None, None) => "every wallet".to_string(),
    };
    if *check {
        subject.push_str(" (proofs verified against the mint)");
    }
    subject
}

// ═══════════════════════════════════════════
// `ui receive` — what to scan to be paid
// ═══════════════════════════════════════════

async fn receive_panel(
    app: Arc<App>,
    input: Input,
    rx: mpsc::Receiver<Output>,
    log_filters: &agent_first_data::LogFilters,
    output: OutputFormat,
    delivery: Option<UiDeliveryMode>,
) -> ! {
    let frontend = match PanelFrontend::resolve(RECEIVE_UI_KIND, PanelShape::Receive) {
        Ok(frontend) => frontend,
        Err(failure) => fail_frontend(failure, output),
    };
    let app_icon = match frontend.app_icon() {
        Ok(app_icon) => app_icon,
        Err(failure) => fail_frontend(failure, output),
    };
    let network = receive_network_of(&input);
    let received = run_and_drain(app, input, rx, log_filters, output, |out| {
        matches!(out, Output::ReceiveInfo { .. })
    })
    .await;
    let Some(received) = received else {
        fail(
            "ui_no_result",
            "the receive request produced no address or invoice to show",
            Some("run the matching `afpay <network> receive` command to see what it reports"),
            output,
        );
    };

    let result = match event_result(&received) {
        Ok(result) => result,
        Err(error) => fail("ui_render_failed", &error, None, output),
    };
    let subject = receive_subject_of(&result, &network);
    let (document, code) = receive_document(&result, &network, &subject);
    let scannable = document.scannable;
    let stylesheet = match frontend.stylesheet() {
        Ok(bytes) => bytes,
        Err(failure) => fail_frontend(failure, output),
    };
    let page = match frontend.render_page(&document, None) {
        Ok(page) => page,
        Err(failure) => fail_frontend(failure, output),
    };

    let mut router = Router::new()
        .route("/", get(move || async move { Html(page) }))
        .merge(shared_panel_routes(&frontend, stylesheet));
    if let Some(code) = code {
        let code = Arc::new(code);
        router = router.route(
            &format!("/{QR_PATH}"),
            get(move || {
                let code = Arc::clone(&code);
                async move { svg_response(code.to_string()) }
            }),
        );
    }

    let session = match UiSession::<()>::new(PROVIDER_ID, RECEIVE_UI_KIND) {
        Ok(session) => session
            .with_subject(subject.clone())
            .with_app_icon(app_icon)
            .with_security_policy(watch_page_policy().into_security_policy()),
        Err(error) => fail("ui_session_invalid", &error.to_string(), None, output),
    };

    let active = match delivery_plan(crate::args::WATCH_PANEL_DELIVERY, delivery, output)
        .start(session, router)
        .await
    {
        Ok(active) => active,
        Err(error) => fail("ui_failed", &error.to_string(), window_hint(&error), output),
    };
    let facts = active.facts();

    // The result the agent gets carries the address and invoice too: the panel
    // is for the person paying, and the agent still has to be able to record
    // what it asked for without reading the screen.
    emit_ready(
        json_fields(serde_json::json!({
            "message": format!("The receive panel is ready: {}.", facts.mode.description()),
            "ui_kind": RECEIVE_UI_KIND,
            "subject": subject,
            "network": network,
            "scannable": scannable,
            "receive_info": result.get("receive_info").cloned().unwrap_or(Value::Null),
        })),
        &facts,
        &frontend,
        output,
    );

    match active.wait().await {
        Ok(outcome) => {
            emit_event_or_exit(
                agent_first_data::json_result(serde_json::json!({
                    "code": "ui_closed",
                    "ui_kind": RECEIVE_UI_KIND,
                    "subject": subject,
                    "ending": outcome.ending(),
                    "network": network,
                    "scannable": scannable,
                }))
                .build()
                .into_value(),
                output,
            );
            std::process::exit(0);
        }
        Err(error) => fail("ui_failed", &error.to_string(), window_hint(&error), output),
    }
}

fn svg_response(svg: String) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("image/svg+xml"),
        )],
        svg,
    )
        .into_response()
}

fn receive_network_of(input: &Input) -> String {
    match input {
        Input::Receive {
            network: Some(network),
            ..
        } => network_label(*network),
        _ => String::new(),
    }
}

/// What this panel is a view of: the wallet that will be paid, on which network.
///
/// The wallet id comes from the result rather than the request, because the
/// request is allowed to leave it out and let afpay choose — and "the wallet
/// afpay chose" is not something a person can tell two windows apart by.
fn receive_subject_of(result: &Value, network: &str) -> String {
    let wallet = text(result, "wallet");
    match (wallet.is_empty(), network.is_empty()) {
        (true, true) => "a receive address".to_string(),
        (true, false) => format!("a {network} receive address"),
        (false, true) => wallet,
        (false, false) => format!("{wallet} on {network}"),
    }
}

// ═══════════════════════════════════════════
// `ui send` — a payment a person approves
// ═══════════════════════════════════════════

/// What the person said about one resolved payment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SendDecision {
    Approve,
    Reject,
}

/// Whether this ending is permission to move money.
///
/// `Completed(Approve)` is the only one. The refuse button, a closed window,
/// and a lapsed credential are the same answer — do not send — because a person
/// who never answered has not agreed to anything. Written as an exhaustive
/// match with no default arm on purpose: a new `Outcome` variant has to be
/// classified here deliberately rather than inheriting "approve" by falling
/// through.
fn approved(outcome: &Outcome<SendDecision>) -> bool {
    match outcome {
        Outcome::Completed(SendDecision::Approve) => true,
        Outcome::Completed(SendDecision::Reject) | Outcome::Closed | Outcome::Expired => false,
    }
}

/// How the window ended, in the agent's terms.
/// How this panel ended, in afpay's words where afpay has any.
///
/// Only the answered cases do: what a person decided is afpay's domain. The
/// other two are AFUI's ending — and now that this panel is not window-only,
/// spelling them here would have said `window_closed` about a registered
/// session nobody ever opened a window on.
fn ending_of(outcome: &Outcome<SendDecision>) -> &'static str {
    match outcome {
        Outcome::Completed(SendDecision::Approve) => "approved",
        Outcome::Completed(SendDecision::Reject) => "refused",
        other => other.ending(),
    }
}

async fn send_panel(
    app: Arc<App>,
    input: Input,
    rx: mpsc::Receiver<Output>,
    log_filters: &agent_first_data::LogFilters,
    output: OutputFormat,
    delivery: Option<UiDeliveryMode>,
) -> ! {
    // Resolved before the payment is: a frontend afpay cannot load never costs
    // a provider round trip, never records a plan, and never opens a window.
    let frontend = match PanelFrontend::resolve(SEND_UI_KIND, PanelShape::Confirm) {
        Ok(frontend) => Arc::new(frontend),
        Err(failure) => fail_frontend(failure, output),
    };
    let app_icon = match frontend.app_icon() {
        Ok(app_icon) => app_icon,
        Err(failure) => fail_frontend(failure, output),
    };

    // The window shows a plan, not a request. `input` is the plan half of the
    // §9 boundary — afpay resolves the payment, records it, and emits
    // `pay_planned`; this page is a view of that event, already redacted. A
    // payment that could not be resolved produces no plan and no window: the
    // person is never asked to accept terms afpay could not state.
    let (mut app, mut rx) = (app, rx);
    let planned = match plan_payment(&mut app, &mut rx, input, log_filters, output).await {
        Some(planned) => planned,
        None => std::process::exit(1),
    };
    // The plan id afpay will submit comes from the stored plan's own event and
    // is carried in Rust from here to the confirm. The document below shows it,
    // but nothing read back off the page decides which plan is paid.
    let plan_id = text(&planned, "plan_id");
    let subject = send_subject_of(&planned);
    let document = confirm_document(&planned, &subject);

    let stylesheet = match frontend.stylesheet() {
        Ok(bytes) => bytes,
        Err(failure) => fail_frontend(failure, output),
    };
    // The nonce is minted once per session and is the only thing that can put a
    // script on this page. The page's own markup may be a person's; the script
    // that turns a declared control into an answer is afpay's, and a frontend
    // cannot forge the attribute that admits it.
    let nonce = match UiCspNonce::generate() {
        Ok(nonce) => nonce,
        Err(error) => fail("ui_session_invalid", &error.to_string(), None, output),
    };
    // A page that declares no control fails here — after the plan is recorded,
    // because whether a page can be answered is only knowable once it exists.
    // Nothing has moved: an unconfirmed plan expires on its own, exactly as a
    // refusal leaves one.
    let page = match frontend.render_page(&document, Some(&nonce)) {
        Ok(page) => page,
        Err(failure) => fail_frontend(failure, output),
    };

    let session = match UiSession::<SendDecision>::new(PROVIDER_ID, SEND_UI_KIND) {
        Ok(session) => session
            .with_subject(subject.clone())
            .with_app_icon(app_icon)
            .with_security_policy(confirm_page_policy(&nonce).into_security_policy()),
        Err(error) => fail("ui_session_invalid", &error.to_string(), None, output),
    };
    // Two routes rather than one with a body to parse: the path *is* the
    // answer, so nothing has to read a form field to find out which control was
    // pressed — and a control's declaration is bound to one of these two paths
    // by afpay's runtime, never by the page.
    let router = Router::new()
        .route("/", get(move || async move { Html(page) }))
        .route(
            "/approve",
            post(decider(
                session.completion(),
                SendDecision::Approve,
                Arc::clone(&frontend),
            )),
        )
        .route(
            "/refuse",
            post(decider(
                session.completion(),
                SendDecision::Reject,
                Arc::clone(&frontend),
            )),
        )
        .merge(shared_panel_routes(&frontend, stylesheet));

    let active = match delivery_plan(crate::args::DECISION_PANEL_DELIVERY, delivery, output)
        .start(session, router)
        .await
    {
        Ok(active) => active,
        Err(error) => fail("ui_failed", &error.to_string(), window_hint(&error), output),
    };
    let facts = active.facts();

    emit_ready(
        json_fields(serde_json::json!({
            "message": format!(
                "A payment is waiting for a person to approve it ({}). Ending the panel without \
                 answering refuses it.",
                facts.mode.description()
            ),
            "ui_kind": SEND_UI_KIND,
            "subject": subject,
            "plan": planned.clone(),
        })),
        &facts,
        &frontend,
        output,
    );

    let outcome = match active.wait().await {
        Ok(outcome) => outcome,
        Err(error) => fail("ui_failed", &error.to_string(), window_hint(&error), output),
    };
    let approved = approved(&outcome);
    emit_event_or_exit(
        agent_first_data::json_result(serde_json::json!({
            "code": "ui_send_decided",
            "ui_kind": SEND_UI_KIND,
            "subject": subject,
            "decision": if approved { "approved" } else { "refused" },
            "ending": ending_of(&outcome),
            "plan": planned,
            "dispatched": approved,
        }))
        .build()
        .into_value(),
        output,
    );
    if !approved {
        // The plan is left unconfirmed and expires on its own. Nothing was
        // contacted, so there is nothing to undo.
        std::process::exit(0);
    }

    // What gets paid is the plan that was shown, submitted by id. There is no
    // second resolution here and no request to rebuild from flags: a person
    // cannot approve one payment and have afpay make another.
    let confirm = Input::PayConfirm {
        id: request_identifier(output),
        plan_id,
        expect: None,
        idempotency_key: None,
    };
    let sent = run_and_drain(app, confirm, rx, log_filters, output, |_| false).await;
    debug_assert!(sent.is_none());
    std::process::exit(0);
}

/// Run the plan half and hand back afpay's own `pay_planned` event.
///
/// Returns `None` when nothing was resolved — the error has already gone to
/// the agent, and there is no window to open.
async fn plan_payment(
    app: &mut Arc<App>,
    rx: &mut mpsc::Receiver<Output>,
    input: Input,
    log_filters: &agent_first_data::LogFilters,
    output: OutputFormat,
) -> Option<Value> {
    app.requests_total.fetch_add(1, Ordering::Relaxed);
    handler::dispatch(app, Request::from_input(input)).await;

    let mut planned = None;
    let mut failed = false;
    while let Ok(event) = rx.try_recv() {
        match event {
            Output::PayPlanned { .. } => {
                planned = match event_result(&event) {
                    Ok(value) => Some(value),
                    Err(error) => fail("ui_render_failed", &error, None, output),
                };
            }
            Output::Error { .. } => {
                failed = true;
                emit_or_exit(&event, output);
            }
            Output::Log { ref event, .. } if !log_filters.enabled(event) => {}
            other => emit_or_exit(&other, output),
        }
    }
    if failed || planned.is_none() {
        if !failed {
            emit_or_exit(
                &Output::Error {
                    id: None,
                    error_code: "plan_not_resolved".to_string(),
                    error:
                        "afpay resolved no plan for this payment, so there is nothing to approve"
                            .to_string(),
                    hint: Some(
                        "run the same request as a command to see why it could not be resolved"
                            .to_string(),
                    ),
                    retryable: false,
                    retry_after_ms: None,
                    trace: crate::types::Trace::from_duration(0),
                },
                output,
            );
        }
        return None;
    }
    planned
}

fn request_identifier(output: OutputFormat) -> String {
    match store::wallet::generate_request_identifier() {
        Ok(id) => id,
        Err(error) => fail("internal_error", &error.to_string(), None, output),
    }
}

fn send_subject_of(planned: &Value) -> String {
    let wallet = text(planned, "wallet");
    let to = text(planned, "to");
    if to.is_empty() {
        format!("{wallet} → a Cashu bearer token")
    } else {
        format!("{wallet} → {to}")
    }
}

/// One handler that records one answer.
///
/// The first answer wins; a second press of the other button is told the
/// session already returned, rather than silently doing nothing.
fn decider(
    completion: UiCompletion<SendDecision>,
    decision: SendDecision,
    frontend: Arc<PanelFrontend>,
) -> impl Fn() -> std::pin::Pin<Box<dyn Future<Output = Response> + Send>> + Clone + Send + 'static
{
    move || {
        let completion = completion.clone();
        let frontend = Arc::clone(&frontend);
        Box::pin(async move {
            let recorded = completion.complete(decision).await;
            decided_page(&frontend, decision, recorded)
        })
    }
}

/// What afpay tells the person after they answer — in afpay's words, from what
/// afpay recorded.
///
/// This is not a courtesy. A confirm panel's page may have been written by
/// somebody other than afpay, and this is the sentence that says what actually
/// happened, rendered from the answer the session took rather than from
/// whatever the control that was pressed claimed to be.
fn decided_page(frontend: &PanelFrontend, decision: SendDecision, recorded: bool) -> Response {
    let document = decided_document(decision, recorded);
    match frontend.render_decided(&document) {
        Ok(page) => Html(page).into_response(),
        Err(failure) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html(format!(
                "<!doctype html><html lang=\"en\"><body><h1>{}</h1><p>{}</p></body></html>",
                escape(document.heading),
                escape(&failure.message)
            )),
        )
            .into_response(),
    }
}

fn decided_document(decision: SendDecision, recorded: bool) -> DecidedDocument {
    let (heading, message) = match (decision, recorded) {
        (SendDecision::Approve, true) => ("Approved", "afpay is sending the payment now."),
        (SendDecision::Reject, true) => ("Refused", "Nothing was sent."),
        (_, false) => (
            "Already answered",
            "This window returned an answer already; this press changed nothing.",
        ),
    };
    DecidedDocument {
        ui_kind: SEND_UI_KIND,
        title: "Payment",
        heading,
        subject: message.to_owned(),
        footer: "",
        message: message.to_owned(),
    }
}

// ═══════════════════════════════════════════
// The documents a panel template renders against
// ═══════════════════════════════════════════
//
// These types are `ui_api_version` — the whole of what a frontend author may
// rely on. Everything a panel worked out is here already counted, grouped,
// ordered and formatted as text, so an override reorders, regroups or drops
// sections without recomputing anything, and cannot arrive at a different
// answer than the one `afpay balance` or `afpay receive` reports. Nothing here
// is markup: the template decides what a `<dl>` or a `<figure>` is, and afpay
// decides what is true.
//
// Every value is a string, a boolean or a count. Amounts stay exact digits with
// no thousands separators — a payment figure has to read back to afpay — and a
// value the panel has no answer for is the em dash afpay wrote, not an empty
// cell a template has to interpret.

/// One name/value row a panel prints without laying it out by hand.
#[derive(Serialize)]
struct FieldDocument {
    name: String,
    value: String,
}

/// One wallet's balance, in the unit its provider reports.
#[derive(Serialize)]
struct BalanceDocument {
    unit: String,
    confirmed: String,
    pending: String,
    /// Provider extras that rode in on `BalanceInfo`'s flattened map —
    /// phoenixd's `fee_credit_sats`, and whatever the next backend reports.
    extras: Vec<FieldDocument>,
}

/// One wallet card.
#[derive(Serialize)]
struct WalletCardDocument {
    id: String,
    label: String,
    address: String,
    /// True when the provider did not answer. The card stays on the page:
    /// dropping it would understate what the person holds, and failing the
    /// whole panel would tell them nothing at all.
    failed: bool,
    error: Option<String>,
    balance: Option<BalanceDocument>,
    details: Vec<FieldDocument>,
}

/// Wallets under the network they belong to, in the order the providers
/// reported them.
#[derive(Serialize)]
struct NetworkGroupDocument {
    network: String,
    wallets: Vec<WalletCardDocument>,
}

/// One per-network total, straight from `NetworkBalanceSummary`.
///
/// One entry per (network, unit): a network whose wallets report different
/// units gets one each rather than a sum that means nothing.
#[derive(Serialize)]
struct TotalDocument {
    network: String,
    unit: String,
    confirmed: String,
    pending: String,
    wallet_count: String,
    errors: String,
    /// True when some wallet in this total did not answer, so the figure beside
    /// it is a floor rather than the answer.
    degraded: bool,
}

/// `wallet_inspect`.
#[derive(Serialize)]
struct WalletDocument {
    ui_kind: &'static str,
    title: &'static str,
    heading: &'static str,
    subject: String,
    footer: String,
    wallet_count: usize,
    unreachable_count: usize,
    totals: Vec<TotalDocument>,
    groups: Vec<NetworkGroupDocument>,
}

/// The code to scan, as an image afpay serves.
///
/// `url` is a route on this session rather than markup in the document, so the
/// page keeps its unconditional escaping and a template still chooses where the
/// code sits and how large it is. What it encodes is not a template's to
/// change.
#[derive(Serialize)]
struct QrDocument {
    kind: &'static str,
    url: &'static str,
    alt: String,
}

/// `receive_inspect`.
#[derive(Serialize)]
struct ReceiveDocument {
    ui_kind: &'static str,
    title: &'static str,
    heading: &'static str,
    subject: String,
    footer: &'static str,
    network: String,
    wallet: String,
    scannable: bool,
    qr: Option<QrDocument>,
    /// Why there is no code, when there is none. A receive panel exists to be
    /// pointed at a phone, so "nothing to scan" is a sentence rather than an
    /// empty frame.
    warning: Option<String>,
    /// The payable strings in text, because a person copying one into another
    /// machine is as ordinary as scanning it, and a code cannot be selected.
    payload: Vec<FieldDocument>,
    details: Vec<FieldDocument>,
}

/// One spend budget this payment would debit.
#[derive(Serialize)]
struct DebitDocument {
    amount: String,
    token: String,
}

/// One answer a person can give, and what to call it.
///
/// `id` is the whole of the semantics: afpay's runtime binds a control
/// declaring `data-afpay-decision="approve"` to the route that approves, and
/// binds nothing to a declaration it does not recognise. A template may put
/// these anywhere, label them anything, and wrap them in anything — the mapping
/// is not the template's to write.
#[derive(Serialize)]
struct DecisionDocument {
    id: &'static str,
    label: &'static str,
}

/// `send_confirm`: everything a person needs in order to refuse.
///
/// The destination exactly as afpay will use it, the amount and fee it
/// resolved, and the spend budgets this payment would debit. There is no "fee
/// unknown" state to render — a payment afpay could not price never reaches a
/// window. `plan_id` names the plan an approval submits, so what is shown is
/// what is confirmed.
#[derive(Serialize)]
struct ConfirmDocument {
    ui_kind: &'static str,
    title: &'static str,
    heading: &'static str,
    subject: String,
    footer: &'static str,
    plan_id: String,
    /// `send` or `cashu_send` — which confirm route this plan belongs to.
    operation: String,
    network: String,
    wallet: String,
    /// Named rather than blank when there is no destination: a Cashu token mint
    /// pays a bearer, and an empty row would read as "to nobody".
    to: String,
    amount: String,
    fee: String,
    unit: String,
    /// What this payment will write on-chain, in words rather than a blank.
    /// It gets a row of its own instead of falling into `details` because it is
    /// the part of the approval that is public and cannot be taken back, and a
    /// person scanning for that should not have to find it among plan ids and
    /// expiry stamps.
    onchain_memo: String,
    debits: Vec<DebitDocument>,
    details: Vec<FieldDocument>,
    decisions: Vec<DecisionDocument>,
}

/// What the confirm panel shows once an answer has been recorded.
#[derive(Serialize)]
struct DecidedDocument {
    ui_kind: &'static str,
    title: &'static str,
    heading: &'static str,
    subject: String,
    footer: &'static str,
    message: String,
}

fn wallet_document(result: &Value, subject: &str) -> WalletDocument {
    let wallets = array(result, "wallets");
    let summaries = array(result, "summary");
    let wallet_count = wallets.len();
    let unreachable_count = wallets.iter().filter(|item| is_failed(item)).count();

    let totals = summaries
        .iter()
        .map(|summary| {
            let errors = number(summary, "errors");
            TotalDocument {
                network: text(summary, "network"),
                unit: text(summary, "unit"),
                confirmed: scalar(summary.get("confirmed")),
                pending: scalar(summary.get("pending")),
                wallet_count: scalar(summary.get("wallet_count")),
                errors: scalar(summary.get("errors")),
                degraded: errors != Some(0),
            }
        })
        .collect();

    let mut groups: Vec<NetworkGroupDocument> = Vec::new();
    for wallet in &wallets {
        let network = text(wallet, "network");
        let card = wallet_card_document(wallet);
        match groups.iter_mut().find(|group| group.network == network) {
            Some(group) => group.wallets.push(card),
            None => groups.push(NetworkGroupDocument {
                network,
                wallets: vec![card],
            }),
        }
    }

    WalletDocument {
        ui_kind: WALLET_UI_KIND,
        title: "Wallets",
        heading: "Wallets",
        subject: subject.to_owned(),
        footer: format!(
            "{wallet_count} {} · {unreachable_count} unreachable",
            if wallet_count == 1 {
                "wallet"
            } else {
                "wallets"
            }
        ),
        wallet_count,
        unreachable_count,
        totals,
        groups,
    }
}

fn wallet_card_document(wallet: &Value) -> WalletCardDocument {
    WalletCardDocument {
        id: text(wallet, "id"),
        label: text(wallet, "label"),
        address: text(wallet, "address"),
        failed: is_failed(wallet),
        error: wallet
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_owned),
        balance: match wallet.get("balance") {
            Some(balance @ Value::Object(_)) => Some(BalanceDocument {
                unit: text(balance, "unit"),
                confirmed: scalar(balance.get("confirmed")),
                pending: scalar(balance.get("pending")),
                extras: fields_of(balance, &BALANCE_PLACED_KEYS),
            }),
            _ => None,
        },
        details: fields_of(wallet, &WALLET_PLACED_KEYS),
    }
}

fn is_failed(wallet: &Value) -> bool {
    wallet.get("error").is_some() || !matches!(wallet.get("balance"), Some(Value::Object(_)))
}

/// Build the receive document, and the SVG afpay will serve beside it.
///
/// When a provider returns something that cannot be encoded — a bare quote id
/// with no address or invoice yet — there is no code and the document says why
/// in place of one.
fn receive_document(
    result: &Value,
    network: &str,
    subject: &str,
) -> (ReceiveDocument, Option<String>) {
    let receive_info = result.get("receive_info").cloned().unwrap_or(Value::Null);
    let invoice = text(&receive_info, "invoice");
    let address = text(&receive_info, "address");
    let quote_id = text(&receive_info, "quote_id");
    let payload = super::qr::wallet_deposit_qr_payload(
        (!invoice.is_empty()).then_some(invoice.as_str()),
        (!address.is_empty()).then_some(address.as_str()),
    );

    let (qr, code, warning) = match &payload {
        Some((kind, payload)) => match super::qr::render_qr_svg_element(payload) {
            Ok(svg) => (
                Some(QrDocument {
                    kind,
                    url: QR_PATH,
                    alt: format!(
                        "A QR code encoding this wallet's {}",
                        kind.replace('_', " ")
                    ),
                }),
                Some(svg),
                None,
            ),
            Err(error) => (
                None,
                None,
                Some(format!(
                    "This payload could not be drawn as a code ({error}). Copy it below instead."
                )),
            ),
        },
        None => (
            None,
            None,
            Some(
                "This wallet returned no address or invoice yet, so there is nothing to scan."
                    .to_owned(),
            ),
        ),
    };

    let mut fields = Vec::new();
    for (name, value) in [
        ("invoice", &invoice),
        ("address", &address),
        ("quote_id", &quote_id),
    ] {
        if !value.is_empty() {
            fields.push(FieldDocument {
                name: name.to_owned(),
                value: value.clone(),
            });
        }
    }

    let document = ReceiveDocument {
        ui_kind: RECEIVE_UI_KIND,
        title: "Receive",
        heading: "Receive",
        subject: subject.to_owned(),
        footer: "Payment status is not monitored here.",
        network: network.to_owned(),
        wallet: text(result, "wallet"),
        scannable: qr.is_some(),
        qr,
        warning,
        payload: fields,
        details: fields_of(&receive_info, &RECEIVE_PLACED_KEYS),
    };
    (document, code)
}

fn confirm_document(planned: &Value, subject: &str) -> ConfirmDocument {
    let to = text(planned, "to");
    ConfirmDocument {
        ui_kind: SEND_UI_KIND,
        title: "Approve payment",
        heading: "Approve this payment?",
        subject: subject.to_owned(),
        footer: "Closing this window without answering refuses the payment.",
        plan_id: text(planned, "plan_id"),
        operation: text(planned, "operation"),
        network: text(planned, "network"),
        wallet: text(planned, "wallet"),
        to: if to.is_empty() {
            "a Cashu bearer token".to_owned()
        } else {
            to
        },
        amount: scalar(planned.get("amount_native")),
        fee: scalar(planned.get("fee_estimate_native")),
        unit: text(planned, "fee_unit"),
        onchain_memo: match text(planned, "onchain_memo") {
            memo if memo.is_empty() => "none — nothing is written on-chain".to_owned(),
            memo => memo,
        },
        debits: array(planned, "spend_debits")
            .iter()
            .map(|debit| {
                let token = text(debit, "token");
                DebitDocument {
                    amount: scalar(debit.get("amount_native")),
                    token: if token.is_empty() {
                        "native".to_owned()
                    } else {
                        token
                    },
                }
            })
            .collect(),
        details: fields_of(planned, &PLAN_PLACED_KEYS),
        decisions: vec![
            DecisionDocument {
                id: "refuse",
                label: "Do not send",
            },
            DecisionDocument {
                id: "approve",
                label: "Approve and send",
            },
        ],
    }
}

// ═══════════════════════════════════════════
// Value rendering
// ═══════════════════════════════════════════

/// The `result` payload of afpay's own protocol event for this output.
fn event_result(output: &Output) -> Result<Value, String> {
    let event = crate::output_fmt::protocol_event(output)?;
    Ok(event.get("result").cloned().unwrap_or(Value::Null))
}

fn json_fields(value: Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(fields) => fields,
        _ => serde_json::Map::new(),
    }
}

/// Every field of `value` the card did not already place, in the order the
/// event carries them.
fn fields_of(value: &Value, placed: &[&str]) -> Vec<FieldDocument> {
    let Some(fields) = value.as_object() else {
        return Vec::new();
    };
    fields
        .iter()
        .filter(|(name, _)| !placed.contains(&name.as_str()))
        .map(|(name, field)| FieldDocument {
            name: name.clone(),
            value: scalar(Some(field)),
        })
        .collect()
}

fn array(value: &Value, key: &str) -> Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn number(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

/// Render one field. Amounts stay as exact digits — a payment figure has to be
/// readable back to afpay, so no thousands separators. Nested values are shown
/// as compact JSON rather than dropped.
fn scalar(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "—".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

/// The last-resort escape, for the one page afpay writes in Rust: the reply
/// shown when even the decided template will not render. Everything else on
/// every panel is escaped by MiniJinja, unconditionally.
fn escape(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for character in raw.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::types::{
        BalanceInfo, NetworkBalanceSummary, ReceiveInfo, Trace, WalletBalanceItem, WalletSummary,
    };
    use std::collections::BTreeMap;

    fn wallet(id: &str, network: Network, label: Option<&str>) -> WalletSummary {
        WalletSummary {
            id: id.to_string(),
            network,
            label: label.map(str::to_string),
            address: format!("addr_for_{id}"),
            backend: None,
            mint_url: None,
            rpc_endpoints: None,
            chain_id: None,
            created_at_epoch_s: 1_700_000_000,
        }
    }

    fn balances(wallets: Vec<WalletBalanceItem>) -> Output {
        let summary = NetworkBalanceSummary::from_wallets(&wallets);
        Output::WalletBalances {
            id: "req_1".to_string(),
            wallets,
            summary,
            trace: Trace::from_duration(1),
        }
    }

    /// afpay's own wallet page, rendered the way a window would receive it.
    fn page_of(output: &Output) -> String {
        let result = event_result(output).expect("the panel must render");
        let document = wallet_document(&result, "every wallet");
        PanelFrontend::builtin(WALLET_UI_KIND, PanelShape::Wallet)
            .render_page(&document, None)
            .expect("the built-in wallet page must render")
    }

    #[test]
    fn a_wallet_that_failed_renders_beside_the_ones_that_answered() {
        let page = page_of(&balances(vec![
            WalletBalanceItem {
                wallet: wallet("w_good", Network::Sol, None),
                balance: Some(BalanceInfo::new(4_200, 0, "lamports")),
                error: None,
            },
            WalletBalanceItem {
                wallet: wallet("w_broken", Network::Sol, None),
                balance: None,
                error: Some("rpc endpoint refused the connection".to_string()),
            },
        ]));

        // Both wallets are on the page: the failure is data, not an error that
        // replaces the view.
        assert!(page.contains("w_good"));
        assert!(page.contains("w_broken"));
        assert!(page.contains("rpc endpoint refused the connection"));
        assert!(page.contains("class=\"wallet failed\""));
        assert!(page.contains("4200"));
        assert!(page.contains("2 wallets · 1 unreachable"));
    }

    #[test]
    fn the_only_wallet_failing_still_produces_a_page_rather_than_an_error() {
        let page = page_of(&balances(vec![WalletBalanceItem {
            wallet: wallet("w_alone", Network::Btc, None),
            balance: None,
            error: Some("esplora timed out".to_string()),
        }]));
        assert!(page.contains("w_alone"));
        assert!(page.contains("esplora timed out"));
        assert!(page.contains("1 wallet · 1 unreachable"));
    }

    #[test]
    fn provider_extras_are_shown_rather_than_silently_dropped() {
        let page = page_of(&balances(vec![WalletBalanceItem {
            wallet: wallet("w_ln", Network::Ln, None),
            balance: Some(
                BalanceInfo::new(1_000, 25, "sats").with_additional("fee_credit_sats", 777),
            ),
            error: None,
        }]));
        assert!(page.contains("fee_credit_sats"));
        assert!(page.contains("777"));
        // The wallet's own unplaced fields survive too.
        assert!(page.contains("created_at_epoch_s"));
    }

    /// The panel must not become a second, unredacted view of wallet data.
    ///
    /// `BalanceInfo` flattens a provider-controlled map into the balance object,
    /// so a backend reporting `seed_secret` would put it straight on the page if
    /// the page rendered the typed struct. It renders afpay's redacted event
    /// instead, so the value never survives the trip.
    #[test]
    fn a_secret_bearing_field_cannot_reach_the_page() {
        let mut additional = BTreeMap::new();
        additional.insert("seed_secret".to_string(), 8_675_309_u64);
        let page = page_of(&balances(vec![WalletBalanceItem {
            wallet: wallet("w_leaky", Network::Cashu, None),
            balance: Some(BalanceInfo {
                confirmed: 1,
                pending: 0,
                unit: "sats".to_string(),
                additional,
            }),
            error: None,
        }]));
        assert!(
            !page.contains("8675309"),
            "a *_secret value reached the page: {page}"
        );
        assert!(page.contains("seed_secret"));
        assert!(page.contains("***"));
    }

    #[test]
    fn wallet_labels_are_escaped_so_they_cannot_inject_markup() {
        let page = page_of(&balances(vec![WalletBalanceItem {
            wallet: wallet("w_xss", Network::Evm, Some("<script>alert(1)</script>")),
            balance: Some(BalanceInfo::new(0, 0, "gwei")),
            error: None,
        }]));
        assert!(!page.contains("<script>alert(1)</script>"));
        assert!(page.contains("&lt;script&gt;alert(1)"));
    }

    #[test]
    fn a_watch_panel_loads_no_script_and_declares_a_policy_that_forbids_one() {
        let page = page_of(&balances(vec![WalletBalanceItem {
            wallet: wallet("w_plain", Network::Sol, None),
            balance: Some(BalanceInfo::new(1, 0, "lamports")),
            error: None,
        }]));
        assert!(!page.contains("<script"));
        // The trusted-runtime marker resolves to nothing at all on a panel with
        // no decision to make.
        assert!(!page.contains(frontend::TRUSTED_RUNTIME_MARKER));
        assert!(page.contains("<link rel=\"stylesheet\" href=\"style.css\">"));
        let watch = watch_page_policy().header_value();
        let watch = watch.to_str().unwrap();
        assert!(watch.contains("default-src 'none'"), "{watch}");
        assert!(watch.contains("style-src 'self'"), "{watch}");
        // A watch panel needs no form target at all; the confirm panel needs
        // exactly its own, plus the one nonce that admits afpay's runtime.
        assert!(watch.contains("form-action 'none'"), "{watch}");
        let nonce = UiCspNonce::generate().unwrap();
        let confirm = confirm_page_policy(&nonce).header_value();
        let confirm = confirm.to_str().unwrap();
        assert!(confirm.contains("form-action 'self'"), "{confirm}");
        assert!(
            confirm.contains(&format!("script-src 'nonce-{}'", nonce.as_str())),
            "{confirm}"
        );
    }

    #[test]
    fn every_panel_follows_system_theme_and_keeps_decisions_keyboard_visible() {
        for (kind, shape) in [
            (WALLET_UI_KIND, PanelShape::Wallet),
            (RECEIVE_UI_KIND, PanelShape::Receive),
            (SEND_UI_KIND, PanelShape::Confirm),
        ] {
            // Both themes and the focus ring come from the stylesheet AFUI
            // serves every session. A panel carrying a second copy of the
            // palette is one whose appearance can drift from every other
            // interface on the machine with nothing to notice.
            let style = PanelFrontend::builtin(kind, shape).stylesheet().unwrap();
            let style = String::from_utf8_lossy(&style);
            assert!(
                !style.contains("color-scheme: light dark") && !style.contains("--afui-focus:"),
                "{kind}: {style}"
            );
        }
        // The one place a panel can forget it: the frame every panel extends.
        let page = page_of(&balances(vec![WalletBalanceItem {
            wallet: wallet("w_good", Network::Sol, None),
            balance: Some(BalanceInfo::new(4_200, 0, "lamports")),
            error: None,
        }]));
        assert!(
            page.contains("<link rel=\"stylesheet\" href=\"__afui/base.css\">"),
            "{page}"
        );
        let baseline = agent_first_ui::page_base_style_source();
        assert!(baseline.contains("color-scheme: light dark"));
        assert!(baseline.contains("@media (prefers-color-scheme: dark)"));
        assert!(baseline.contains(":focus-visible"));
    }

    #[test]
    fn an_empty_result_says_so_instead_of_rendering_an_empty_shell() {
        let page = page_of(&balances(Vec::new()));
        assert!(page.contains("No wallets match this view."));
        assert!(page.contains("0 wallets · 0 unreachable"));
    }

    #[test]
    fn the_subject_names_the_filter_the_person_asked_for() {
        assert_eq!(
            wallet_subject_of(&Input::Balance {
                id: "r".to_string(),
                wallet: None,
                network: None,
                check: false,
            }),
            "every wallet"
        );
        assert_eq!(
            wallet_subject_of(&Input::Balance {
                id: "r".to_string(),
                wallet: Some("w_1".to_string()),
                network: None,
                check: false,
            }),
            "w_1"
        );
        assert!(
            wallet_subject_of(&Input::Balance {
                id: "r".to_string(),
                wallet: None,
                network: Some(Network::Cashu),
                check: true,
            })
            .contains("proofs verified")
        );
    }

    // ── ui receive ──────────────────────────

    fn receive_output(address: Option<&str>, invoice: Option<&str>, quote: Option<&str>) -> Output {
        Output::ReceiveInfo {
            id: "req_2".to_string(),
            wallet: "w_recv".to_string(),
            receive_info: ReceiveInfo {
                address: address.map(str::to_string),
                invoice: invoice.map(str::to_string),
                quote_id: quote.map(str::to_string),
            },
            trace: Trace::from_duration(1),
        }
    }

    /// The page a window would receive, plus the code afpay would serve at
    /// `qr.svg` beside it.
    fn receive_page_of(output: &Output, network: &str) -> (String, Option<String>, bool) {
        let result = event_result(output).expect("the panel must render");
        let subject = receive_subject_of(&result, network);
        let (document, code) = receive_document(&result, network, &subject);
        let scannable = document.scannable;
        let page = PanelFrontend::builtin(RECEIVE_UI_KIND, PanelShape::Receive)
            .render_page(&document, None)
            .expect("the built-in receive page must render");
        (page, code, scannable)
    }

    /// A receive panel with no code is the wrong product: the whole point is
    /// that someone points a phone at it.
    #[test]
    fn an_address_is_drawn_as_a_code_and_also_shown_as_text() {
        let (page, code, scannable) = receive_page_of(
            &receive_output(Some("bc1qexampleaddress0000"), None, None),
            "btc",
        );
        assert!(scannable);
        // The page points at the code; afpay owns the bytes behind it.
        assert!(page.contains("<img src=\"qr.svg\""));
        assert!(page.contains("data-kind=\"receive_address\""));
        let code = code.expect("a scannable payload must produce a code");
        assert!(code.starts_with("<svg"));
        assert!(code.contains("<path fill=\"#000000\""));
        // Copyable as well as scannable.
        assert!(page.contains("bc1qexampleaddress0000"));
        assert!(page.contains("w_recv on btc"));
        assert!(!page.contains("<script"));
    }

    /// The same choice the REPL's `.svg` writer makes: an invoice wins, and it
    /// is encoded with the scheme a wallet app expects.
    #[test]
    fn an_invoice_wins_over_an_address_and_keeps_its_scheme() {
        let (page, code, scannable) = receive_page_of(
            &receive_output(
                Some("bc1qexampleaddress0000"),
                Some("lnbc1exampleinvoice"),
                None,
            ),
            "ln",
        );
        assert!(scannable);
        assert!(code.is_some());
        assert!(page.contains("data-kind=\"lightning_invoice\""));
        assert!(page.contains("lnbc1exampleinvoice"));
    }

    /// A provider that has a quote but no payable string yet must not render a
    /// blank frame someone would try to scan.
    #[test]
    fn nothing_payable_says_so_rather_than_drawing_an_empty_code() {
        let (page, code, scannable) =
            receive_page_of(&receive_output(None, None, Some("q_123")), "cashu");
        assert!(!scannable);
        assert!(code.is_none());
        assert!(!page.contains("<img"));
        assert!(page.contains("nothing to scan"));
        assert!(page.contains("q_123"));
    }

    /// The card shows each payable string once, and shows it as itself — not
    /// as a row plus a re-print of the object it came out of.
    #[test]
    fn the_receive_card_does_not_print_the_same_payload_twice() {
        let (page, _, _) = receive_page_of(
            &receive_output(Some("bc1qexampleaddress0000"), None, None),
            "btc",
        );
        assert_eq!(page.matches("bc1qexampleaddress0000").count(), 1);
        assert!(!page.contains("receive_info"));
    }

    #[test]
    fn a_receive_payload_is_escaped_so_it_cannot_inject_markup() {
        let (page, _, _) = receive_page_of(
            &receive_output(Some("<script>alert(1)</script>"), None, None),
            "sol",
        );
        assert!(!page.contains("<script>alert(1)</script>"));
        assert!(page.contains("&lt;script&gt;alert(1)"));
    }

    // ── ui send ─────────────────────────────

    /// afpay's own `pay_planned` payload, which is what the page renders.
    fn planned(to: &str, fee: u64) -> Value {
        serde_json::json!({
            "code": "pay_planned",
            "id": "req_3",
            "plan_id": "plan_deadbeef",
            "operation": "send",
            "network": "sol",
            "wallet": "w_pay",
            "to": to,
            "amount_native": 5_000,
            "fee_estimate_native": fee,
            "fee_unit": "lamports",
            "spend_debits": [{ "amount_native": 5_000, "token": "native" }],
            "expires_at_epoch_ms": 1_800_000_000_000u64,
        })
    }

    fn confirm_page_of(planned: &Value, subject: &str) -> String {
        let nonce = UiCspNonce::generate().unwrap();
        PanelFrontend::builtin(SEND_UI_KIND, PanelShape::Confirm)
            .render_page(&confirm_document(planned, subject), Some(&nonce))
            .expect("the built-in confirm page must render")
    }

    /// A person approving a payment must be able to see what it will write
    /// on-chain. `PayPlanned` did not carry the memo at all for a while, so the
    /// confirm card had a memo row whose condition was permanently false — the
    /// page looked complete and silently showed nothing. On-chain memos are
    /// public and cannot be taken back, so "not shown" is not an acceptable
    /// default for them.
    #[test]
    fn the_confirm_page_shows_what_the_payment_will_write_on_chain() {
        let mut with_memo = planned("addr", 5_000);
        with_memo["onchain_memo"] = Value::String("invoice 4471".to_owned());
        let page = confirm_page_of(&with_memo, "sol");
        assert!(page.contains("on-chain memo"), "{page}");
        assert!(page.contains("invoice 4471"), "{page}");

        // And when there is none, say so rather than leaving a blank a person
        // has to interpret.
        let page = confirm_page_of(&planned("addr", 5_000), "sol");
        assert!(page.contains("on-chain memo"), "{page}");
        assert!(page.contains("nothing is written on-chain"), "{page}");
    }

    /// The one rule this panel exists to keep. Closing the window is a person
    /// walking away, not a person agreeing; so is a credential that lapsed.
    #[test]
    fn only_pressing_approve_is_permission_to_move_money() {
        assert!(approved(&Outcome::Completed(SendDecision::Approve)));
        assert!(!approved(&Outcome::Completed(SendDecision::Reject)));
        assert!(!approved(&Outcome::Closed));
        assert!(!approved(&Outcome::Expired));
    }

    #[test]
    fn every_ending_is_named_for_the_agent_that_asked() {
        assert_eq!(
            ending_of(&Outcome::Completed(SendDecision::Approve)),
            "approved"
        );
        assert_eq!(
            ending_of(&Outcome::Completed(SendDecision::Reject)),
            "refused"
        );
        // The unanswered endings are AFUI's word, not afpay's — a panel that
        // is now also a link or a registered session cannot call every ending
        // a closed window.
        assert_eq!(
            ending_of(&Outcome::Closed),
            Outcome::<SendDecision>::Closed.ending()
        );
        assert_eq!(
            ending_of(&Outcome::Expired),
            Outcome::<SendDecision>::Expired.ending()
        );
    }

    #[test]
    fn a_planned_payment_shows_its_destination_amount_fee_and_budget() {
        let planned = planned("solana:Recipient111?amount=5000&token=native", 890);
        let subject = send_subject_of(&planned);
        let page = confirm_page_of(&planned, &subject);
        assert!(page.contains("solana:Recipient111?amount=5000&amp;token=native"));
        assert!(page.contains("class=\"payment-lead\""));
        assert!(page.contains("5000"));
        assert!(page.contains("890"));
        assert!(page.contains("lamports"));
        // The budget this payment would consume, before it consumes it.
        assert!(page.contains("class=\"debits\""));
        // The plan an approval submits is on the page, so a person can match
        // what they were shown against what afpay records.
        assert!(page.contains("plan_deadbeef"));
        assert!(page.contains("Closing this window without answering refuses the payment."));
    }

    /// The declaration, the binding, and the fact that they are not in the same
    /// file. A template says what a control is *for*; only this runtime says
    /// what it *does*, and it arrives under a nonce nothing in a frontend
    /// directory could forge.
    #[test]
    fn the_page_declares_the_controls_and_afpay_binds_them_under_its_own_nonce() {
        let page = confirm_page_of(&planned("lnbc1example", 3), "w_pay → lnbc1example");
        assert!(page.contains("data-afpay-decision=\"refuse\""));
        assert!(page.contains("data-afpay-decision=\"approve\""));
        assert!(page.contains("data-afpay-decision-status"));
        assert!(page.contains("<script nonce=\""));
        assert!(page.contains("form.action = action"));
        assert!(page.contains("Sending payment…"));
        // The routes are the runtime's, not the page's: nothing in the markup
        // names where an answer goes.
        assert!(!page.contains("action=\"approve\""));
        assert!(!page.contains("<form"));
    }

    /// A Cashu token mint has no destination. The page must name what it is
    /// rather than leave the row blank, which would read as "to nobody".
    #[test]
    fn a_token_mint_names_the_bearer_token_as_its_destination() {
        let mut planned = planned("", 3);
        if let Value::Object(fields) = &mut planned {
            fields.remove("to");
            fields.insert("operation".to_string(), serde_json::json!("cashu_send"));
        }
        let subject = send_subject_of(&planned);
        assert_eq!(subject, "w_pay → a Cashu bearer token");
        let page = confirm_page_of(&planned, &subject);
        assert!(page.contains("a Cashu bearer token"));
    }

    #[test]
    fn a_plan_reaches_the_page_only_after_the_same_redaction_the_agent_gets() {
        let plan = crate::output_fmt::redacted_value(&serde_json::json!({
            "plan_id": "plan_deadbeef",
            "wallet": "w_pay",
            "to": "lnbc1example",
            "amount_native": 1,
            "fee_estimate_native": 2,
            "fee_unit": "sats",
            "relay_password_secret": "hunter2",
        }));
        let page = confirm_page_of(&plan, "w_pay → lnbc1example");
        assert!(!page.contains("hunter2"), "a *_secret reached the page");
        assert!(page.contains("relay_password_secret"));
        assert!(page.contains("***"));
    }

    #[test]
    fn a_destination_is_escaped_so_it_cannot_inject_markup() {
        let planned = planned("<script>alert(1)</script>", 1);
        let page = confirm_page_of(&planned, "w_pay → x");
        assert!(!page.contains("<script>alert(1)</script>"));
        assert!(page.contains("&lt;script&gt;alert(1)"));
    }

    /// The window returns one answer. A second press has to say so rather than
    /// implying it changed something.
    #[tokio::test]
    async fn the_first_answer_wins_and_the_second_is_told_so() {
        let session = UiSession::<SendDecision>::new(PROVIDER_ID, SEND_UI_KIND).unwrap();
        let completion = session.completion();
        assert!(completion.complete(SendDecision::Reject).await);
        assert!(!completion.complete(SendDecision::Approve).await);
        let frontend = PanelFrontend::builtin(SEND_UI_KIND, PanelShape::Confirm);
        assert!(
            frontend
                .render_decided(&decided_document(SendDecision::Reject, true))
                .unwrap()
                .contains("Nothing was sent.")
        );
        assert!(
            frontend
                .render_decided(&decided_document(SendDecision::Approve, false))
                .unwrap()
                .contains("already")
        );
    }

    /// The contract number a frontend declares is afpay's, and it covers all
    /// three panels at once.
    #[test]
    fn every_panel_is_overridable_under_one_contract_number() {
        assert_eq!(PROVIDER_ID, "afpay");
        assert_eq!(frontend::UI_API_VERSION, "1");
        for (ui_kind, shape) in [
            (WALLET_UI_KIND, PanelShape::Wallet),
            (RECEIVE_UI_KIND, PanelShape::Receive),
            (SEND_UI_KIND, PanelShape::Confirm),
        ] {
            let frontend = PanelFrontend::builtin(ui_kind, shape);
            assert!(frontend.frontend_id().is_none());
            assert!(!frontend.stylesheet().unwrap().is_empty());
        }
    }
}
