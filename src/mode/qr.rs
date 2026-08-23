//! The one place afpay turns a payment destination into a QR code.
//!
//! Two callers encode the same bytes: the interactive session writing an `.svg`
//! file, and `afpay ui receive` drawing the code into its page. Keeping the
//! choice here is not tidiness — a second copy of it would let a code someone
//! scanned and a file someone saved disagree about what is being paid.

/// Render one payload as a standalone SVG document.
#[cfg(feature = "interactive")]
pub(super) fn render_qr_svg(data: &str) -> Result<String, String> {
    render(data)
}

/// Render one payload as an SVG element to embed in a page.
///
/// A standalone file opens with an XML prolog; inside an HTML document that
/// prolog is not a declaration but stray character data, so it is dropped. The
/// element itself needs no `img-src` allowance and no script — it is markup the
/// page already contains.
#[cfg(feature = "ui")]
pub(super) fn render_qr_svg_element(data: &str) -> Result<String, String> {
    let document = render(data)?;
    Ok(match document.split_once("?>") {
        Some((prolog, element)) if prolog.starts_with("<?xml") => element.to_string(),
        _ => document,
    })
}

fn render(data: &str) -> Result<String, String> {
    use qrcode::QrCode;
    use qrcode::render::svg;

    let code = QrCode::new(data.as_bytes()).map_err(|e| format!("QR encode error: {e}"))?;
    let rendered = code
        .render::<svg::Color<'_>>()
        .min_dimensions(320, 320)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .quiet_zone(true)
        .build();
    Ok(rendered)
}

fn add_lightning_prefix(invoice: &str) -> String {
    if invoice.starts_with("lightning:") {
        invoice.to_string()
    } else if invoice.starts_with("lnbc")
        || invoice.starts_with("lntb")
        || invoice.starts_with("lnbcrt")
    {
        format!("lightning:{invoice}")
    } else {
        invoice.to_string()
    }
}

/// What a payer's device should scan, and what kind of thing it is.
///
/// An invoice wins over a bare address: a wallet that produced both is telling
/// us the invoice is the one carrying the amount.
pub(super) fn wallet_deposit_qr_payload(
    invoice: Option<&str>,
    address: Option<&str>,
) -> Option<(&'static str, String)> {
    if let Some(invoice) = invoice {
        return Some(("lightning_invoice", add_lightning_prefix(invoice)));
    }
    address.map(|value| ("receive_address", value.to_string()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn lightning_prefix_is_added_once() {
        assert_eq!(
            add_lightning_prefix("lnbc1abc"),
            "lightning:lnbc1abc".to_string()
        );
        assert_eq!(
            add_lightning_prefix("lightning:lnbc1abc"),
            "lightning:lnbc1abc".to_string()
        );
        assert_eq!(add_lightning_prefix("bc1qxyz"), "bc1qxyz".to_string());
    }

    #[test]
    fn an_invoice_is_preferred_over_an_address_and_carries_its_scheme() {
        assert_eq!(
            wallet_deposit_qr_payload(Some("lnbc1abc"), Some("bc1qxyz")),
            Some(("lightning_invoice", "lightning:lnbc1abc".to_string()))
        );
        assert_eq!(
            wallet_deposit_qr_payload(None, Some("bc1qxyz")),
            Some(("receive_address", "bc1qxyz".to_string()))
        );
        assert_eq!(wallet_deposit_qr_payload(None, None), None);
    }

    /// A page embeds the element, so the prolog a file needs must be gone —
    /// and only the prolog.
    #[cfg(feature = "ui")]
    #[test]
    fn the_embeddable_form_drops_the_prolog_and_keeps_the_drawing() {
        let element = render_qr_svg_element("bc1qexampleaddress").expect("a QR must render");
        assert!(!element.contains("<?xml"));
        assert!(element.starts_with("<svg"));
        assert!(element.ends_with("</svg>"));
        // The dark modules are what a scanner reads; an empty path would render
        // a blank square that still passed every structural check above.
        assert!(element.contains("<path fill=\"#000000\""));
        assert!(element.contains(" d=\"M"));
    }
}
