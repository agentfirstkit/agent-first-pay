//! Where an afpay panel's files come from, and what a person may replace.
//!
//! AFUI owns delivery: which directory a `provider_id` + `ui_kind` override
//! lives in, whether it has been trusted, whether it declares afpay's
//! `ui_api_version`, which file the override supplies and which falls back to
//! afpay's own, and the rule that a frontend afpay cannot load is an error
//! naming safe mode rather than a quiet built-in page. None of that is restated
//! here — [`PanelFrontend`] is a thin thing wrapped around
//! `agent_first_ui::UiFrontend`, which is the one implementation of it.
//!
//! afpay owns what a frontend *is*: MiniJinja templates rendered against the
//! typed panel documents in [`super`], plus a stylesheet and static assets. That
//! is the whole of what an override supplies. It supplies no behaviour: AFUI
//! refuses a frontend file whose name says it is a script, and
//! [`agent_first_ui::reject_frontend_script`] refuses one hiding inside a
//! template. The only JavaScript any panel loads is afpay's own decision
//! runtime, injected under a per-session nonce at the layout's
//! `<!-- afpay:trusted-runtime -->` marker.
//!
//! What that buys is the point of the whole arrangement, and on this Provider it
//! is the difference between a window and a payment: a template can move,
//! rename, regroup and drop anything, and still cannot make a control mean
//! something other than what it declares — the declaration is the template's,
//! the binding is afpay's.

use std::path::PathBuf;

use agent_first_ui::{
    Error as UiError, UiAppIcon, UiCspNonce, UiDecisionRuntime, UiErrorKind, UiFrontend, UiPage,
    UiPageBuilder,
};
use serde::Serialize;

pub(super) const PROVIDER_ID: &str = "afpay";

/// The panel contract a frontend is written against.
///
/// One number for all three panels, because from a frontend author's point of
/// view they are one contract: the shape of the `document` a template renders
/// against, the template names afpay resolves, the
/// `<!-- afpay:trusted-runtime -->` marker, and the `data-afpay-decision`
/// declaration the runtime binds. A change to any of those is a change to all
/// of them for the person who has to fix their frontend.
pub(super) const UI_API_VERSION: &str = "1";

/// Where afpay splices its own behaviour into a page it did not necessarily
/// write.
pub(super) const TRUSTED_RUNTIME_MARKER: &str = "<!-- afpay:trusted-runtime -->";

/// How a template declares that a control answers the question.
pub(super) const DECISION_ATTRIBUTE: &str = "data-afpay-decision";

/// The entry template every override supplies under `templates/`.
///
/// One name for every `ui_kind` rather than three, because an override
/// directory is already keyed by `ui_kind`: the person editing
/// `.afui/frontends/afpay/send_confirm/templates/page.html.j2` has already said
/// which panel they mean by being in that directory.
const ENTRY_TEMPLATE: &str = "page.html.j2";

const LAYOUT_TEMPLATE: &str = "layout.html.j2";
const FIELDS_TEMPLATE: &str = "fields.html.j2";
const DECIDED_TEMPLATE: &str = "decided.html.j2";

/// Where an override's files live, and nothing more.
///
/// A template names its siblings the way a person writing one would —
/// `{% extends "layout.html.j2" %}` — while the file itself is looked up under
/// this directory. Collapsing the two, so that a template had to write the
/// directory into every reference, is a contract change nobody asked for: it
/// breaks every override already written against the documented name.
const TEMPLATE_DIR: &str = "templates";

fn template_path(name: &str) -> String {
    format!("{TEMPLATE_DIR}/{name}")
}

const BUILTIN_LAYOUT: &str = include_str!("templates/layout.html.j2");
const BUILTIN_FIELDS: &str = include_str!("templates/fields.html.j2");
const BUILTIN_WALLET: &str = include_str!("templates/wallet.html.j2");
const BUILTIN_RECEIVE: &str = include_str!("templates/receive.html.j2");
const BUILTIN_CONFIRM: &str = include_str!("templates/confirm.html.j2");
const BUILTIN_DECIDED: &str = include_str!("templates/decided.html.j2");

/// The stylesheet is a route rather than an inline `<style>` so the page needs
/// no style exemption in its policy, and so a frontend can replace presentation
/// without touching structure — or structure without touching presentation.
const BUILTIN_STYLE: &str = include_str!("style.css");
const BUILTIN_APP_ICON: &str = include_str!("app-icon.svg");

/// Where the confirm page shows what is happening while an answer travels.
const DECISION_STATUS_ATTRIBUTE: &str = "data-afpay-decision-status";

/// The confirm panel's runtime: which declaration means what, and afpay's own
/// words for what is happening while it travels.
///
/// The rules around it — exact declaration match, an unrecognised declaration
/// bound to nothing, one answer per page, every control disabled the moment
/// one is pressed, a form navigation rather than a fetch — are AFUI's, so they
/// hold identically for every decide page in this kit rather than being
/// re-derived here. What stays afpay's is the vocabulary: `approve` and
/// `refuse`, the routes they post to, and what a person reads while the
/// payment is on its way.
fn decision_runtime() -> Result<UiDecisionRuntime, FrontendFailure> {
    UiDecisionRuntime::new(DECISION_ATTRIBUTE)
        .and_then(|runtime| runtime.with_status_attribute(DECISION_STATUS_ATTRIBUTE))
        .and_then(|runtime| runtime.with_decision("refuse", "refuse"))
        .and_then(|runtime| runtime.with_pending_text("refuse", "Refusing…"))
        .and_then(|runtime| runtime.with_decision("approve", "approve"))
        .and_then(|runtime| runtime.with_pending_text("approve", "Sending payment…"))
        .map_err(FrontendFailure::from)
}

/// A panel that failed before it could be shown, in afpay's error vocabulary.
#[derive(Debug)]
pub(super) struct FrontendFailure {
    pub(super) code: &'static str,
    pub(super) message: String,
    pub(super) hint: Option<&'static str>,
}

/// Every AFUI failure, in afpay's vocabulary.
///
/// AFUI classifies its own errors and knows its own recovery actions; afpay
/// prefixes the one and passes the other through. What was here before was a
/// match over AFUI's variants that fell into `unreadable` for anything it had
/// not thought about.
impl From<UiError> for FrontendFailure {
    fn from(error: UiError) -> Self {
        Self {
            code: ui_error_code(&error),
            message: error.to_string(),
            hint: error.hint(),
        }
    }
}

/// `ui_<afpay's word for what AFUI classified>`, interned so the code stays
/// `&'static str`.
///
/// Exhaustive over AFUI's closed classification, with **no wildcard arm**:
/// this used to default to `ui_frontend_unreadable`, so a closed session
/// runtime or a machine with no browser was reported as a frontend that could
/// not be read. A payment panel is the worst place to misname why something
/// stopped.
fn ui_error_code(error: &UiError) -> &'static str {
    match error.kind() {
        UiErrorKind::FrontendIncompatible => "ui_frontend_incompatible",
        UiErrorKind::FrontendUnsafe => "ui_frontend_unsafe",
        UiErrorKind::FrontendUnreadable => "ui_frontend_unreadable",
        UiErrorKind::PageRender => "ui_frontend_template",
        UiErrorKind::PageIncomplete => "ui_frontend_incomplete",
        UiErrorKind::WindowUnavailable => "ui_window_unavailable",
        UiErrorKind::WindowWaitFailed => "ui_window_wait_failed",
        UiErrorKind::DeliveryModeInvalid | UiErrorKind::DeliveryModeNotOffered => {
            "ui_delivery_unavailable"
        }
        UiErrorKind::LinkAddressUnavailable | UiErrorKind::UpstreamNotProxyable => {
            "ui_delivery_unreachable"
        }
        UiErrorKind::RuntimeClosed => "ui_runtime_closed",
        UiErrorKind::RuntimeBusy => "ui_runtime_busy",
        UiErrorKind::RuntimeMisconfigured
        | UiErrorKind::RuntimeMessageTooLarge
        | UiErrorKind::RuntimeBlob
        | UiErrorKind::RuntimePayload => "ui_runtime_failed",
        UiErrorKind::ConfigUnreadable => "ui_config_unreadable",
        UiErrorKind::InvalidArgument | UiErrorKind::Io => "ui_failed",
    }
}

/// Which built-in page a `ui_kind` starts from when no override supplies one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PanelShape {
    /// Every wallet and its balance: `wallet_inspect`.
    Wallet,
    /// What to scan to be paid: `receive_inspect`.
    Receive,
    /// One resolved payment and a person's answer: `send_confirm`.
    Confirm,
}

impl PanelShape {
    fn builtin_entry(self) -> &'static str {
        match self {
            Self::Wallet => BUILTIN_WALLET,
            Self::Receive => BUILTIN_RECEIVE,
            Self::Confirm => BUILTIN_CONFIRM,
        }
    }

    /// Whether a page of this shape must declare the controls the runtime
    /// binds. Only a panel that returns an answer has an answer to lose.
    fn needs_decision_controls(self) -> bool {
        matches!(self, Self::Confirm)
    }
}

/// One panel's file source: a person's frontend, or afpay's own.
pub(super) struct PanelFrontend {
    frontend: UiFrontend,
    shape: PanelShape,
}

impl PanelFrontend {
    /// Resolve the frontend for this panel before anything is served.
    ///
    /// Called before the window opens — and, on `send_confirm`, before the
    /// payment is even resolved — so a frontend afpay cannot load ends the
    /// command with an error rather than opening a window onto a page nobody
    /// asked for.
    pub(super) fn resolve(
        ui_kind: &'static str,
        shape: PanelShape,
    ) -> Result<Self, FrontendFailure> {
        let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let frontend = UiFrontend::resolve(&workspace_root, PROVIDER_ID, ui_kind, UI_API_VERSION)?;
        Ok(Self { frontend, shape })
    }

    /// afpay's own panel, for a caller that is not reading any frontend.
    #[cfg(test)]
    pub(super) fn builtin(ui_kind: &'static str, shape: PanelShape) -> Self {
        Self {
            frontend: UiFrontend::builtin(PROVIDER_ID, ui_kind, UI_API_VERSION),
            shape,
        }
    }

    /// The override serving this panel, or `None` for afpay's own page.
    ///
    /// This goes in the readiness event. A workspace frontend that has not been
    /// trusted is deliberately silent — AFUI skips it before parsing anything
    /// inside it — so this is how an agent tells "my override is running" from
    /// "my override is inert" without opening a window to look.
    pub(super) fn frontend_id(&self) -> Option<&str> {
        self.frontend.frontend_id()
    }

    /// The stylesheet this panel serves: the frontend's, or afpay's.
    pub(super) fn stylesheet(&self) -> Result<Vec<u8>, FrontendFailure> {
        Ok(self
            .frontend
            .file("style.css")?
            .unwrap_or_else(|| BUILTIN_STYLE.as_bytes().to_vec()))
    }

    /// The application icon from this panel's resolved frontend overlay.
    pub(super) fn app_icon(&self) -> Result<UiAppIcon, FrontendFailure> {
        self.frontend
            .app_icon(BUILTIN_APP_ICON)
            .map_err(FrontendFailure::from)
    }

    /// The resolved overlay behind this panel, for AFUI's own page routes.
    pub(super) fn frontend(&self) -> &UiFrontend {
        &self.frontend
    }

    /// Render this panel's page from a typed document.
    ///
    /// `nonce` is the session's, and the only thing that can put a script on
    /// the page. A panel with no decision to make passes `None` and the marker
    /// resolves to nothing at all.
    pub(super) fn render_page<T: Serialize>(
        &self,
        document: &T,
        nonce: Option<&UiCspNonce>,
    ) -> Result<String, FrontendFailure> {
        let decisions = self
            .shape
            .needs_decision_controls()
            .then(decision_runtime)
            .transpose()?;
        let runtime = match &decisions {
            Some(decisions) => {
                let nonce = nonce.ok_or_else(|| FrontendFailure {
                    code: "internal",
                    message: "a confirm panel was rendered without a session nonce".to_owned(),
                    hint: None,
                })?;
                Some(format!(
                    "<script nonce=\"{}\">\n{}</script>",
                    nonce.as_str(),
                    decisions.source()
                ))
            }
            None => None,
        };
        let mut builder = self.page_builder(ENTRY_TEMPLATE, self.shape.builtin_entry())?;
        if let Some(decisions) = &decisions {
            builder = builder.requires_decisions(decisions);
        }
        builder
            .runtime_marker(TRUSTED_RUNTIME_MARKER)
            .runtime(runtime)
            .render(document)
            .map_err(FrontendFailure::from)
    }

    /// Render the page afpay shows once an answer has been recorded.
    ///
    /// Rendered through the same pipeline, so an override's layout and
    /// stylesheet still apply — but never a decision control, because this page
    /// has no question left on it.
    pub(super) fn render_decided<T: Serialize>(
        &self,
        document: &T,
    ) -> Result<String, FrontendFailure> {
        self.page_builder(DECIDED_TEMPLATE, BUILTIN_DECIDED)?
            .runtime_marker(TRUSTED_RUNTIME_MARKER)
            .runtime(None)
            .render(document)
            .map_err(FrontendFailure::from)
    }

    /// Every template both pages share, plus whatever partials the frontend
    /// added of its own.
    ///
    /// `UiPage` owns the assembly from here: the MiniJinja policy, the render,
    /// the guard against a rendered value smuggling markup, the runtime-marker
    /// count, and the declaration contract — in the order that keeps afpay's
    /// own trusted runtime from tripping the guard it splices past. What used
    /// to be here was that whole sequence written again, including a
    /// `replace` over every occurrence of the marker: a template that wrote it
    /// twice injected the decision runtime twice.
    fn page_builder<'a>(
        &'a self,
        entry: &'a str,
        fallback: &'a str,
    ) -> Result<UiPageBuilder<'a>, FrontendFailure> {
        Ok(UiPage::builder(&self.frontend)
            .entry_at(entry, template_path(entry))
            .fallback(fallback)
            .template_at(
                LAYOUT_TEMPLATE,
                template_path(LAYOUT_TEMPLATE),
                BUILTIN_LAYOUT,
            )
            .template_at(
                FIELDS_TEMPLATE,
                template_path(FIELDS_TEMPLATE),
                BUILTIN_FIELDS,
            )
            .template_at(
                DECIDED_TEMPLATE,
                template_path(DECIDED_TEMPLATE),
                BUILTIN_DECIDED,
            )
            .template_at(
                ENTRY_TEMPLATE,
                template_path(ENTRY_TEMPLATE),
                self.shape.builtin_entry(),
            )
            .override_templates_under(TEMPLATE_DIR, ".j2")?)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// The rules a decide page is held to — exact declaration match, an
    /// unrecognised declaration bound to nothing, the marker exactly once —
    /// are AFUI's now, and tested there. What is still afpay's is that its
    /// runtime and its own page agree on a vocabulary: a runtime binding a
    /// word no page declares is a control that never appears, and a page
    /// declaring a word no runtime binds is one that does nothing.
    ///
    /// `mode::ui`'s own tests render the built-in confirm page and assert both
    /// declarations reach it; this is the other side of that pair.
    #[test]
    fn the_runtime_binds_the_safe_answer_first_and_gives_each_one_afpays_words() {
        let runtime = decision_runtime().expect("the decision runtime builds");
        let declarations: Vec<_> = runtime.declarations().collect();
        assert!(declarations.contains(&"approve"), "{declarations:?}");
        assert!(declarations.contains(&"refuse"), "{declarations:?}");
        assert_eq!(runtime.attribute(), DECISION_ATTRIBUTE);

        // Domain words, supplied by afpay: AFUI carries no default here.
        let source = runtime.source();
        assert!(source.contains("Sending payment"), "{source}");
        assert!(source.contains("Refusing"), "{source}");
    }
}
