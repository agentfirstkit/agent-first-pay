#![cfg(all(feature = "ui", feature = "sol", feature = "redb"))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A person replacing an afpay panel, driven as a real process.
//!
//! What an AFUI frontend changes is which bytes reach a browser, so nothing
//! here is checked by calling a function: every case runs a real `afpay ui`
//! against a real wallet, with a stub standing in for the browser, and asserts
//! on the page and the stylesheet the stub actually fetched.
//!
//! `AFUI_BROWSER_BINARY` names the stub. It records the `--app=<url>` it was
//! launched with, curls the page, the stylesheet and the receive code into
//! files, and exits — which is the person closing the window, so the panel ends
//! and the command returns. Setting `AFPAY_STUB_PRESS` makes it post an answer
//! first, which is the person pressing a control that afpay's runtime bound.
//!
//! `AFUI_CONFIG_DIR` moves AFUI's global directory into the test's temp tree,
//! so the trust store these tests write is theirs and not the developer's.
//!
//! The Solana JSON-RPC endpoint is a stub in this process, which is what lets
//! an approval finish: the confirm reaches the chain layer, a transaction id
//! comes back, a history record is written and the spend limit is debited. That
//! is the ledger these tests read to decide whether money moved — the point of
//! the confirm panel is not what the page said, it is what afpay recorded.

use std::fs;
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use agent_first_ui::test_support::FrontendOnDisk;
use serde_json::{Value, json};

const STUB_BROWSER: &str = r#"#!/bin/sh
set -eu
url=""
for arg in "$@"; do
  case "$arg" in
    --app=*) url="${arg#--app=}" ;;
  esac
done
printf '%s' "$url" > "$AFPAY_STUB_DIR/url"
curl -sS -o "$AFPAY_STUB_DIR/body" -w '%{http_code}' "$url" > "$AFPAY_STUB_DIR/status"
curl -sS -o "$AFPAY_STUB_DIR/style" "${url}style.css"
curl -sS -o "$AFPAY_STUB_DIR/code" -w '%{http_code}' "${url}qr.svg" > "$AFPAY_STUB_DIR/code_status"
if [ -n "${AFPAY_STUB_PRESS:-}" ]; then
  curl -sS -X POST -o "$AFPAY_STUB_DIR/decided" "${url}${AFPAY_STUB_PRESS}"
fi
"#;

/// A stylesheet nobody could mistake for afpay's own.
const CUSTOM_STYLE: &str = ":root { --mine: 1 }\nbody { background: rebeccapurple }\n";

/// One wallet panel with afpay's totals band and wallet cards gone, and the
/// wallets flattened into a definition list instead.
const CUSTOM_WALLET: &str = "{% extends \"layout.html.j2\" %}\n\
{% block panel %}\n\
<section data-my-wallets><h2>MY OWN WALLET PANEL</h2>\n\
<dl class=\"mine\">{% for group in document.groups %}{% for wallet in group.wallets %}\
<div><dt>{{ wallet.id }}</dt><dd>{{ group.network }}</dd></div>\
{% endfor %}{% endfor %}</dl>\n\
</section>\n\
{% endblock %}\n";

/// One receive panel with afpay's figure and caption gone and the code moved
/// below the payload it encodes.
const CUSTOM_RECEIVE: &str = "{% extends \"layout.html.j2\" %}\n\
{% block panel %}\n\
<section data-my-receive><h2>MY OWN RECEIVE PANEL</h2>\n\
<dl class=\"mine\">{% for field in document.payload %}\
<div><dt>{{ field.name }}</dt><dd>{{ field.value }}</dd></div>\
{% endfor %}</dl>\n\
{% if document.qr %}<img class=\"mine-code\" src=\"{{ document.qr.url }}\" \
alt=\"{{ document.qr.alt }}\">{% endif %}\n\
</section>\n\
{% endblock %}\n";

/// One confirm panel with afpay's terms list, debit list and detail list all
/// dropped, the whole thing reworded, and the two controls in the opposite
/// order to the one afpay ships them in.
const CUSTOM_CONFIRM: &str = "{% extends \"layout.html.j2\" %}\n\
{% block panel %}\n\
<article data-my-confirm><h2>MY OWN CONFIRM PAGE</h2>\n\
<p class=\"where\">{{ document.wallet }} pays {{ document.to }}</p>\n\
<p class=\"cost\">{{ document.amount }} plus {{ document.fee }} {{ document.unit }}</p>\n\
<p class=\"plan\">{{ document.plan_id }}</p>\n\
<nav>{% for decision in document.decisions | reverse %}\
<button type=\"button\" data-afpay-decision=\"{{ decision.id }}\">{{ decision.label }}</button>\
{% endfor %}</nav>\n\
</article>\n\
{% endblock %}\n";

// ═══════════════════════════════════════════
// A Solana endpoint that answers
// ═══════════════════════════════════════════

/// The two calls a native SOL transfer makes, plus the one a balance makes.
///
/// Not a second afpay: it is the chain, stubbed, so the confirm panel's
/// approval can run all the way to a recorded payment. Everything the tests
/// assert about money is read back out of afpay's own ledger afterwards.
async fn start_sol_rpc() -> String {
    use axum::Json;
    use axum::routing::post;

    let router = axum::Router::new().route(
        "/",
        post(|Json(request): Json<Value>| async move {
            let method = request
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let result = match method {
                // 32 zero bytes: a valid base58 blockhash to sign against.
                "getLatestBlockhash" => {
                    json!({ "value": { "blockhash": "11111111111111111111111111111111" } })
                }
                "sendTransaction" => json!("5tubStubStubStubStubStubStubStubStubStubStubStubStub"),
                "getBalance" => json!({ "value": 900_000_u64 }),
                _ => Value::Null,
            };
            Json(json!({ "jsonrpc": "2.0", "id": 1, "result": result }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{address}/")
}

// ═══════════════════════════════════════════
// The workspace a panel is opened in
// ═══════════════════════════════════════════

struct Panel {
    root: PathBuf,
    data_dir: PathBuf,
    config_dir: PathBuf,
    stub_dir: PathBuf,
    stub: PathBuf,
    wallet: String,
}

impl Panel {
    async fn new(name: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let base = std::env::temp_dir().join(format!(
            "afpay-frontend-{name}-{}-{stamp}",
            std::process::id()
        ));
        let root = base.join("workspace");
        let data_dir = base.join("afpay-home");
        let config_dir = base.join("afui-config");
        let stub_dir = base.join("stub");
        for directory in [&root, &data_dir, &config_dir, &stub_dir] {
            fs::create_dir_all(directory).expect("test directory");
        }
        let stub = stub_dir.join("stub-browser.sh");
        fs::write(&stub, STUB_BROWSER).expect("write stub browser");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("chmod stub");
        }

        let mut panel = Self {
            root,
            data_dir,
            config_dir,
            stub_dir,
            stub,
            wallet: String::new(),
        };
        let endpoint = start_sol_rpc().await;
        panel.wallet = panel.create_wallet(&endpoint);
        panel
    }

    /// One Solana wallet, pinned to the stub endpoint. Key generation is local,
    /// so this needs nothing but the process itself.
    fn create_wallet(&self, endpoint: &str) -> String {
        let created = self.run(&[
            "sol",
            "wallet",
            "create",
            "--sol-rpc-endpoint",
            endpoint,
            "--sol-cluster",
            "devnet",
            "--label",
            "panel",
        ]);
        created
            .iter()
            .find_map(|event| event["result"]["wallet"].as_str())
            .unwrap_or_else(|| panic!("no wallet in {created:?}"))
            .to_owned()
    }

    /// Any afpay command, run against this test's own data directory.
    fn run(&self, args: &[&str]) -> Vec<Value> {
        let output = Command::new(binary())
            .current_dir(&self.root)
            .args(args)
            .args(["--data-dir", &self.data_dir.to_string_lossy()])
            .env("AFUI_CONFIG_DIR", &self.config_dir)
            .env_remove("AFUI_SAFE_MODE")
            .output()
            .expect("run afpay");
        events_of(&output.stdout, &output.stderr)
    }

    /// Run one panel to completion and return what the stub fetched.
    fn open(&self, args: &[&str], env: &[(&str, &str)]) -> Drive {
        for name in ["body", "style", "url", "code", "decided"] {
            let _ = fs::remove_file(self.stub_dir.join(name));
        }
        let mut command = Command::new(binary());
        command
            .current_dir(&self.root)
            .args(args)
            .args(["--data-dir", &self.data_dir.to_string_lossy()])
            .env("AFUI_BROWSER_BINARY", &self.stub)
            .env("AFUI_CONFIG_DIR", &self.config_dir)
            .env("AFPAY_STUB_DIR", &self.stub_dir)
            .env_remove("AFUI_SAFE_MODE")
            .env_remove("AFPAY_STUB_PRESS");
        for (name, value) in env {
            command.env(name, value);
        }
        let output = command.output().expect("run afpay ui");
        Drive {
            status: output.status.code().unwrap_or(99),
            events: events_of(&output.stdout, &output.stderr),
            page: fs::read_to_string(self.stub_dir.join("body")).unwrap_or_default(),
            style: fs::read_to_string(self.stub_dir.join("style")).unwrap_or_default(),
            code: fs::read_to_string(self.stub_dir.join("code")).unwrap_or_default(),
            decided: fs::read_to_string(self.stub_dir.join("decided")).unwrap_or_default(),
            opened: self.stub_dir.join("url").exists(),
        }
    }

    fn frontend_root(&self, ui_kind: &str) -> PathBuf {
        self.root.join(".afui/frontends/afpay").join(ui_kind)
    }

    /// What `afui frontend init` writes, plus files of the person's own.
    fn install(&self, ui_kind: &str, ui_api_version: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = self.frontend_root(ui_kind);
        fs::create_dir_all(root.join("templates")).expect("frontend templates directory");
        fs::write(
            root.join("frontend.json"),
            serde_json::to_string_pretty(&json!({
                "frontend_id": "my_pay_panel",
                "ui_api_version": ui_api_version,
            }))
            .expect("frontend manifest"),
        )
        .expect("write frontend manifest");
        for (name, text) in files {
            fs::write(root.join(name), text).expect("write frontend file");
        }
        root
    }

    /// What `afui frontend enable` records, through AFUI's own code.
    ///
    /// This used to build the trust store by hand around a local copy of
    /// AFUI's fingerprint algorithm — which proves a copy of a hash function
    /// still matches, not that this frontend serves.
    fn trust(&self, ui_kind: &str) {
        FrontendOnDisk::at(self.frontend_root(ui_kind), "afpay", ui_kind)
            .trust_in(&self.config_dir)
            .expect("trust the frontend");
    }

    // ── the ledger, which is what decides whether money moved ──

    fn history(&self) -> Vec<Value> {
        let events = self.run(&["history", "list"]);
        events
            .iter()
            .find_map(|event| event["result"]["items"].as_array().cloned())
            .unwrap_or_default()
    }

    fn spent(&self) -> u64 {
        let events = self.run(&["limit", "list"]);
        events
            .iter()
            .find_map(|event| event["result"]["limits"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|limit| limit["spent"].as_u64())
            .sum()
    }
}

struct Drive {
    status: i32,
    events: Vec<Value>,
    page: String,
    style: String,
    code: String,
    decided: String,
    opened: bool,
}

impl Drive {
    fn ready(&self) -> Value {
        self.events
            .iter()
            .find(|event| event["progress"]["phase"] == "ui_ready")
            .unwrap_or_else(|| panic!("no ui_ready progress in {:?}", self.events))["progress"]
            .clone()
    }

    fn result(&self) -> Value {
        self.events
            .iter()
            .find(|event| event["kind"] == "result")
            .unwrap_or_else(|| panic!("no result in {:?}", self.events))["result"]
            .clone()
    }

    fn error(&self) -> Value {
        self.events
            .iter()
            .find(|event| event["kind"] == "error")
            .unwrap_or_else(|| panic!("no error event in {:?}", self.events))["error"]
            .clone()
    }

    fn frontend_id(&self) -> Option<String> {
        self.ready()
            .get("ui_frontend_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

fn events_of(stdout: &[u8], stderr: &[u8]) -> Vec<Value> {
    format!(
        "{}{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    )
    .lines()
    .filter(|line| !line.trim().is_empty())
    .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{line:?}: {error}")))
    .collect()
}

fn binary() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    exe.parent()
        .and_then(Path::parent)
        .expect("target debug dir")
        .join("afpay")
}

/// One panel, in the terms these tests drive it by.
struct Kind {
    ui_kind: &'static str,
    /// Markup only afpay's own page for this panel produces.
    builtin_marker: &'static str,
    /// Markup only the override produces.
    custom_marker: &'static str,
    template: &'static str,
}

const WALLET: Kind = Kind {
    ui_kind: "wallet_inspect",
    builtin_marker: "<ul class=\"wallets\">",
    custom_marker: "MY OWN WALLET PANEL",
    template: CUSTOM_WALLET,
};

const RECEIVE: Kind = Kind {
    ui_kind: "receive_inspect",
    builtin_marker: "<figcaption>Scan this with the paying device.</figcaption>",
    custom_marker: "MY OWN RECEIVE PANEL",
    template: CUSTOM_RECEIVE,
};

const CONFIRM: Kind = Kind {
    ui_kind: "send_confirm",
    builtin_marker: "<dl class=\"terms\">",
    custom_marker: "MY OWN CONFIRM PAGE",
    template: CUSTOM_CONFIRM,
};

/// The whole lifecycle of one panel, in the order a person lives it.
///
/// One test per `ui_kind` rather than six each, because each step is the
/// previous step's workspace one edit later: what makes the trust gate
/// meaningful is that the same directory served afpay's own page a moment
/// earlier.
fn drive_lifecycle(panel: &Panel, kind: &Kind, argv: &[&str]) {
    let open = || panel.open(argv, &[]);

    // 1. Nothing installed: afpay's own panel and afpay's own stylesheet, and
    //    nothing claimed otherwise.
    let builtin = open();
    assert_eq!(builtin.status, 0, "{:?}", builtin.events);
    assert!(
        builtin.page.contains(kind.builtin_marker),
        "{}",
        builtin.page
    );
    assert!(
        !builtin.page.contains(kind.custom_marker),
        "{}",
        builtin.page
    );
    assert!(builtin.style.contains("color-scheme"), "{}", builtin.style);
    assert_eq!(builtin.frontend_id(), None);

    // 2. Installed but not trusted: still afpay's. A workspace frontend is
    //    inert until someone says otherwise, and the readiness event is where
    //    an agent can see that it is not serving.
    panel.install(
        kind.ui_kind,
        "1",
        &[
            ("templates/page.html.j2", kind.template),
            ("style.css", CUSTOM_STYLE),
        ],
    );
    let untrusted = open();
    assert_eq!(untrusted.status, 0, "{:?}", untrusted.events);
    assert!(untrusted.page.contains(kind.builtin_marker));
    assert!(!untrusted.page.contains(kind.custom_marker));
    assert_eq!(untrusted.style, builtin.style);
    assert_eq!(untrusted.frontend_id(), None);

    // 3. Trusted: the person's own structure is what a browser receives, and
    //    afpay's is gone from the page entirely — structure, not colour.
    panel.trust(kind.ui_kind);
    let trusted = open();
    assert_eq!(trusted.status, 0, "{:?}", trusted.events);
    assert!(
        trusted.page.contains(kind.custom_marker),
        "{}",
        trusted.page
    );
    assert!(
        !trusted.page.contains(kind.builtin_marker),
        "{}",
        trusted.page
    );
    assert_ne!(trusted.page, untrusted.page, "the override changed nothing");
    assert_eq!(trusted.style, CUSTOM_STYLE);
    assert_ne!(trusted.style, builtin.style);
    assert_eq!(trusted.frontend_id().as_deref(), Some("my_pay_panel"));
    // The frame is still afpay's, because the override did not replace it:
    // per file, not per directory.
    assert!(
        trusted
            .page
            .contains("<link rel=\"stylesheet\" href=\"style.css\">")
    );

    // 4. Edited after being trusted: the fingerprint no longer matches, so the
    //    frontend is inert again and afpay's panel is back.
    fs::write(
        panel
            .frontend_root(kind.ui_kind)
            .join("templates/page.html.j2"),
        kind.template
            .replace(kind.custom_marker, "EDITED AFTER TRUST"),
    )
    .expect("edit the trusted frontend");
    let edited = open();
    assert_eq!(edited.status, 0, "{:?}", edited.events);
    assert!(
        !edited.page.contains("EDITED AFTER TRUST"),
        "{}",
        edited.page
    );
    assert!(edited.page.contains(kind.builtin_marker));
    assert_eq!(edited.style, builtin.style);
    assert_eq!(edited.frontend_id(), None);

    // 5. Safe mode with a trusted frontend: afpay's panel, no questions asked.
    panel.trust(kind.ui_kind);
    let safe = panel.open(argv, &[("AFUI_SAFE_MODE", "1")]);
    assert_eq!(safe.status, 0, "{:?}", safe.events);
    assert!(safe.page.contains(kind.builtin_marker), "{}", safe.page);
    assert_eq!(safe.style, builtin.style);
    assert_eq!(safe.frontend_id(), None);

    // …and the same frontend still serves when safe mode is not set, so step 5
    // proved safe mode rather than another revoked fingerprint.
    assert!(open().page.contains("EDITED AFTER TRUST"));
}

/// 6. The one behaviour a fallback would destroy.
///
/// A frontend afpay cannot use is an error naming safe mode. It is never a
/// quietly substituted afpay page, because that is indistinguishable from the
/// override having worked — and on the confirm panel it would be a person
/// approving a payment on a page they did not write.
fn drive_incompatible(panel: &Panel, kind: &Kind, argv: &[&str]) {
    panel.install(
        kind.ui_kind,
        "99",
        &[("templates/page.html.j2", kind.template)],
    );
    panel.trust(kind.ui_kind);

    let drive = panel.open(argv, &[]);
    assert_eq!(drive.status, 1, "{:?}", drive.events);
    assert!(
        drive.page.is_empty() && !drive.opened,
        "no window may open onto a panel afpay could not load"
    );
    let error = drive.error();
    assert_eq!(error["code"], "ui_frontend_incompatible");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("ui_api_version 99"),
        "{error}"
    );
    assert!(
        error["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("AFUI_SAFE_MODE=1"),
        "{error}"
    );

    // Safe mode is the documented way out, and it works on exactly this
    // workspace without touching the frontend.
    let safe = panel.open(argv, &[("AFUI_SAFE_MODE", "1")]);
    assert_eq!(safe.status, 0, "{:?}", safe.events);
    assert!(safe.page.contains(kind.builtin_marker), "{}", safe.page);
}

fn wallet_argv() -> Vec<&'static str> {
    vec!["ui", "wallet"]
}

// ═══════════════════════════════════════════
// wallet_inspect
// ═══════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_wallet_panel_is_replaceable_only_once_installed_compatible_and_trusted() {
    let panel = Panel::new("wallet").await;
    drive_lifecycle(&panel, &WALLET, &wallet_argv());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_incompatible_wallet_frontend_is_an_error_and_never_a_quiet_builtin_page() {
    let panel = Panel::new("wallet-incompatible").await;
    drive_incompatible(&panel, &WALLET, &wallet_argv());
}

// ═══════════════════════════════════════════
// receive_inspect
// ═══════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_receive_panel_is_replaceable_only_once_installed_compatible_and_trusted() {
    let panel = Panel::new("receive").await;
    let wallet = panel.wallet.clone();
    let argv = vec!["ui", "receive", "--network", "sol", "--wallet", &wallet];
    drive_lifecycle(&panel, &RECEIVE, &argv);

    // The code is afpay's whichever page points at it: an override chooses
    // where it sits, never what it encodes.
    let drive = panel.open(&argv, &[]);
    assert!(drive.page.contains("class=\"mine-code\""), "{}", drive.page);
    assert!(drive.code.starts_with("<svg"), "{}", drive.code);
    assert!(drive.code.contains("<path fill=\"#000000\""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_incompatible_receive_frontend_is_an_error_and_never_a_quiet_builtin_page() {
    let panel = Panel::new("receive-incompatible").await;
    let wallet = panel.wallet.clone();
    drive_incompatible(
        &panel,
        &RECEIVE,
        &["ui", "receive", "--network", "sol", "--wallet", &wallet],
    );
}

// ═══════════════════════════════════════════
// send_confirm — the panel that moves money
// ═══════════════════════════════════════════

fn send_argv(wallet: &str) -> Vec<&str> {
    vec![
        "ui",
        "send",
        "--network",
        "sol",
        "--wallet",
        wallet,
        "--to",
        "8nTKRhLQDcnCaS5s8Z4KZPb1i9ddfbfQDeJpw7g4QxjV",
        "--amount",
        "1000",
        "--token",
        "native",
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_confirm_panel_is_replaceable_only_once_installed_compatible_and_trusted() {
    let panel = Panel::new("confirm").await;
    let wallet = panel.wallet.clone();
    drive_lifecycle(&panel, &CONFIRM, &send_argv(&wallet));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_incompatible_confirm_frontend_is_an_error_and_never_a_quiet_builtin_page() {
    let panel = Panel::new("confirm-incompatible").await;
    let wallet = panel.wallet.clone();
    drive_incompatible(&panel, &CONFIRM, &send_argv(&wallet));
}

/// A person's own confirm page — reworded, restructured, with afpay's terms and
/// details dropped and the two controls in the opposite order — still pays the
/// plan it was shown, through afpay.
///
/// The page is the person's; the binding is afpay's. The stub presses `approve`
/// on the route afpay's runtime maps that declaration to, and the money moving
/// is read out of afpay's ledger rather than off the page.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restructured_confirm_page_still_pays_the_plan_it_showed() {
    let panel = Panel::new("confirm-approve").await;
    let wallet = panel.wallet.clone();
    panel.run(&[
        "sol",
        "limit",
        "add",
        "--window",
        "1h",
        "--max-spend",
        "1000000",
        "--token",
        "native",
    ]);
    panel.install(
        "send_confirm",
        "1",
        &[("templates/page.html.j2", CUSTOM_CONFIRM)],
    );
    panel.trust("send_confirm");

    assert_eq!(panel.spent(), 0);
    assert!(panel.history().is_empty());

    let drive = panel.open(&send_argv(&wallet), &[("AFPAY_STUB_PRESS", "approve")]);
    assert_eq!(drive.status, 0, "{:?}", drive.events);
    assert!(drive.page.contains("MY OWN CONFIRM PAGE"), "{}", drive.page);
    assert!(
        !drive.page.contains("<dl class=\"terms\">"),
        "{}",
        drive.page
    );
    assert_eq!(drive.frontend_id().as_deref(), Some("my_pay_panel"));

    // The declaration is the page's; the script that binds it is afpay's, and
    // it arrives under a nonce a frontend directory has no way to know.
    assert!(drive.page.contains("data-afpay-decision=\"approve\""));
    assert!(drive.page.contains("data-afpay-decision=\"refuse\""));
    assert!(drive.page.contains("<script nonce=\""), "{}", drive.page);
    assert!(drive.page.contains("form.action = action"));
    // Nothing in the markup names where an answer goes.
    assert!(!drive.page.contains("action=\"approve\""));

    // What afpay reports, and what afpay recorded.
    let result = drive.result();
    assert_eq!(result["decision"], "approved");
    assert_eq!(result["ending"], "approved");
    assert_eq!(result["dispatched"], true);
    let plan_id = result["plan"]["plan_id"].as_str().expect("a plan id");
    // The plan on the page is the plan that was confirmed.
    assert!(drive.page.contains(plan_id), "{}", drive.page);
    // afpay's own words on the page a press lands on, not the page's.
    assert!(
        drive.decided.contains("afpay is sending the payment now."),
        "{}",
        drive.decided
    );

    // The ledger: one payment, for the amount and fee the page showed, debited
    // from the budget the plan named.
    let history = panel.history();
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(history[0]["direction"], "send");
    assert_eq!(history[0]["amount"]["value"], 1000);
    assert_eq!(panel.spent(), 6000);
}

/// A confirm page that declares no control is a broken override, and is
/// reported as one rather than opened as a question nobody can answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_confirm_page_with_no_declared_control_is_a_broken_override_not_a_silent_window() {
    let panel = Panel::new("confirm-empty").await;
    let wallet = panel.wallet.clone();
    panel.install(
        "send_confirm",
        "1",
        &[(
            "templates/page.html.j2",
            "{% extends \"layout.html.j2\" %}{% block panel %}\
             <p>Trust me, it is fine.</p>{% endblock %}",
        )],
    );
    panel.trust("send_confirm");

    let drive = panel.open(&send_argv(&wallet), &[]);
    assert_eq!(drive.status, 1, "{:?}", drive.events);
    assert!(
        !drive.opened,
        "no window may open onto an unanswerable question"
    );
    let error = drive.error();
    assert_eq!(error["code"], "ui_frontend_incomplete");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("data-afpay-decision"),
        "{error}"
    );
    // Nothing moved: the plan afpay recorded stays unconfirmed and expires.
    assert!(panel.history().is_empty());
}

/// Closing the window is a person walking away, not a person agreeing — and
/// the way to know is the ledger, not the page.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_the_confirm_window_moves_no_money_however_the_page_was_written() {
    let panel = Panel::new("confirm-closed").await;
    let wallet = panel.wallet.clone();
    panel.run(&[
        "sol",
        "limit",
        "add",
        "--window",
        "1h",
        "--max-spend",
        "1000000",
        "--token",
        "native",
    ]);
    // A page that says the payment is already sent, and offers a control
    // labelled as though refusing were the dangerous answer. It changes what a
    // person reads; it cannot change what afpay does.
    panel.install(
        "send_confirm",
        "1",
        &[(
            "templates/page.html.j2",
            "{% extends \"layout.html.j2\" %}{% block panel %}\
             <p>PAYMENT ALREADY SENT — this window is a receipt.</p>\
             <button data-afpay-decision=\"approve\">Close</button>\
             <button data-afpay-decision=\"refuse\">Cancel the payment</button>\
             {% endblock %}",
        )],
    );
    panel.trust("send_confirm");

    let drive = panel.open(&send_argv(&wallet), &[]);
    assert_eq!(drive.status, 0, "{:?}", drive.events);
    assert!(
        drive.page.contains("PAYMENT ALREADY SENT"),
        "{}",
        drive.page
    );

    let result = drive.result();
    assert_eq!(result["decision"], "refused");
    // AFUI's word for an unanswered ending. This panel is no longer
    // window-only, so afpay spelling `window_closed` here would have been a
    // claim about a delivery it might not have used.
    assert_eq!(result["ending"], "closed");
    assert_eq!(result["dispatched"], false);

    // The ledger, which is the only thing that decides whether money moved.
    assert!(panel.history().is_empty(), "{:?}", panel.history());
    assert_eq!(panel.spent(), 0);
}

/// An override may not ship behaviour, by file name or by content — and least
/// of all on the panel that moves money.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_confirm_frontend_that_tries_to_ship_javascript_is_refused_rather_than_served() {
    let panel = Panel::new("confirm-script").await;
    let wallet = panel.wallet.clone();
    panel.install(
        "send_confirm",
        "1",
        &[(
            "templates/page.html.j2",
            "{% extends \"layout.html.j2\" %}{% block panel %}\
             <script>fetch('approve',{method:'POST'})</script>{% endblock %}",
        )],
    );
    panel.trust("send_confirm");

    let drive = panel.open(&send_argv(&wallet), &[]);
    assert_eq!(drive.status, 1, "{:?}", drive.events);
    assert!(!drive.opened);
    assert_eq!(drive.error()["code"], "ui_frontend_unsafe");
    assert!(panel.history().is_empty());
}

/// `ui_kind` is the override key, so replacing one panel replaces one panel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_frontend_replaces_exactly_the_panel_it_was_installed_for() {
    let panel = Panel::new("per-kind").await;
    panel.install(
        "send_confirm",
        "1",
        &[("templates/page.html.j2", CUSTOM_CONFIRM)],
    );
    panel.trust("send_confirm");

    let drive = panel.open(&wallet_argv(), &[]);
    assert_eq!(drive.status, 0, "{:?}", drive.events);
    assert!(drive.page.contains(WALLET.builtin_marker), "{}", drive.page);
    assert_eq!(drive.frontend_id(), None);
}

// ═══════════════════════════════════════════
// delivery
// ═══════════════════════════════════════════

/// A panel that only shows something may travel; the one that authorizes a
/// payment may not.
///
/// AFUI's link URL is a bearer capability. For a view of balances that is a
/// trade worth making — it is how a person reads this from their phone. For a
/// send it would be the authority to move money, held by whoever has the URL,
/// so `afpay ui send` does not offer that word at all. The refusal belongs in
/// the parser: a person who asks for it hears so before a payment has been
/// planned, not after one is sitting there unconfirmed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_watch_panels_offer_the_link_delivery() {
    let panel = Panel::new("delivery-offer").await;
    let wallet = panel.wallet.clone();

    let mut send = send_argv(&wallet);
    send.extend(["--mode", "link"]);
    let refused = panel.run(&send);
    let error = refused
        .iter()
        .find(|event| event["kind"] == "error")
        .unwrap_or_else(|| panic!("no error in {refused:?}"))["error"]
        .clone();
    assert_eq!(error["code"], "cli_invalid_argument_value", "{error}");
    // Naming what *is* on offer, so the next thing the person types is a word
    // this panel does.
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("expected one of window, session"),
        "{error}"
    );

    // And the word is genuinely absent from that panel's discovery surface
    // rather than accepted there and refused later. Discovery takes no other
    // arguments, so this is the binary on its own.
    let help = Command::new(binary())
        .args(["ui", "send", "--help"])
        .output()
        .expect("run afpay ui send --help");
    let help = events_of(&help.stdout, &help.stderr);
    let usage = help
        .iter()
        .find_map(|event| event["result"]["help"]["shapes"][0]["usage"].as_str())
        .unwrap_or_else(|| panic!("no usage in {help:?}"))
        .to_owned();
    assert!(usage.contains("[--mode <window|session>]"), "{usage}");
}

/// A link delivery whose URL is masked is a delivery nobody can use.
///
/// AFDATA redacts `_secret`-suffixed fields, which is right everywhere except
/// the one event whose job is to hand this URL over. Handing it over is the
/// entire point of `link`, so it is revealed deliberately — and nothing else
/// in the event is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_link_delivery_publishes_a_url_a_person_can_actually_open() {
    let panel = Panel::new("delivery-link").await;
    let mut argv = wallet_argv();
    argv.extend(["--mode", "link"]);

    // Nothing opens a window here, so the panel would block: the stub browser
    // is not involved in `link`. Read the ready event and end the process.
    let mut child = Command::new(binary())
        .current_dir(&panel.root)
        .args(&argv)
        .args(["--data-dir", &panel.data_dir.to_string_lossy()])
        // Progress goes to stderr under the default split, and this test is
        // about one progress event.
        .args(["--output-to", "stdout"])
        .env("AFUI_CONFIG_DIR", &panel.config_dir)
        .env_remove("AFUI_SAFE_MODE")
        .stdout(Stdio::piped())
        .spawn()
        .expect("run afpay ui wallet --mode link");
    let mut line = String::new();
    BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut line)
        .expect("read the ready event");
    let _ = child.kill();
    let _ = child.wait();

    let ready: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
    let url = ready["progress"][agent_first_ui::cli::LINK_URL_FIELD]
        .as_str()
        .unwrap_or_else(|| panic!("no link URL in {ready}"));
    assert!(url.starts_with("http://"), "{url}");
    assert!(
        !url.contains('*'),
        "the URL a person must open was masked: {url}"
    );
}

/// Every panel says how it was delivered, in AFUI's words, in the one event an
/// agent reads before the person is done.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panel_reports_the_delivery_it_resolved_to() {
    let panel = Panel::new("delivery-facts").await;

    // No flag: the environment decides, which is the whole reason afpay does
    // not give `--mode` a default of its own.
    let drive = panel.open(&wallet_argv(), &[]);
    assert_eq!(drive.status, 0, "{:?}", drive.events);
    let ready = drive.ready();
    assert_eq!(ready["mode"], "window", "{ready}");
    assert!(
        ready["session_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "{ready}"
    );
    // A window is not a bearer URL, so there is nothing to reveal here.
    assert!(ready.get("link_url_secret").is_none(), "{ready}");
    assert!(drive.opened, "a window delivery opens a window");

    let closed = drive.result();
    assert_eq!(closed["code"], "ui_closed", "{closed}");
    assert_eq!(closed["ending"], "closed", "{closed}");
}
