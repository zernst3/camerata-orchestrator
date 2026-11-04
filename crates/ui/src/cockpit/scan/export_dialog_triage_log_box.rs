//! Tests: export dialog triage log box.

use super::AuditReportExportPanel;
use dioxus::prelude::*;

fn app() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    let dispositions = use_signal(std::collections::HashMap::new);
    rsx! {
        AuditReportExportPanel { project_id: "p1".to_string(), dispositions }
    }
}

#[test]
fn the_dialog_has_an_unticked_triage_log_checkbox() {
    let mut vdom = VirtualDom::new(app);
    vdom.rebuild_in_place();
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("export-triage-log"), "{html}");
    assert!(html.contains(r#"type="checkbox""#), "{html}");
    assert!(html.contains("Triage log"), "{html}");
    assert!(!html.contains("checked=true"), "off by default: {html}");
}
