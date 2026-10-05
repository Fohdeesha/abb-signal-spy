//! Text fields that answer UI Automation. egui's own text fields ignore a value set
//! through it (an assistive tool filling a field in, or a script), and a one-line field
//! had no name a tool could find it by: its hint reaches UI Automation only as a
//! placeholder. Every text field in the window goes through here.

use eframe::egui::{Response, TextEdit, Ui};

/// A one-line text field, named `name` for assistive tools; `shape` sets its hint, width
/// and the rest. A value set through UI Automation replaces the text, a line break
/// becoming a space as in a paste.
pub fn line(ui: &mut Ui, text: &mut String, name: &str, shape: impl FnOnce(TextEdit<'_>) -> TextEdit<'_>) -> Response {
    let mut r = ui.add(shape(TextEdit::singleline(text)));
    answer(ui, &mut r, name, text, false);
    r
}

/// A text field of several lines; a value set keeps its line breaks.
pub fn lines(ui: &mut Ui, text: &mut String, name: &str, shape: impl FnOnce(TextEdit<'_>) -> TextEdit<'_>) -> Response {
    let mut r = ui.add(shape(TextEdit::multiline(text)));
    answer(ui, &mut r, name, text, true);
    r
}

#[cfg(feature = "accessibility")]
fn answer(ui: &Ui, r: &mut Response, name: &str, text: &mut String, multiline: bool) {
    use eframe::egui::accesskit::{Action, ActionData};
    ui.ctx().accesskit_node_builder(r.id, |node| node.set_label(name));
    if !r.enabled() {
        return;
    }
    let set = ui.input(|i| {
        i.accesskit_action_requests(r.id, Action::SetValue)
            .filter_map(|q| match &q.data {
                Some(ActionData::Value(v)) => Some(v.to_string()),
                _ => None,
            })
            .last()
    });
    if let Some(v) = set {
        *text = if multiline { v } else { v.replace(['\r', '\n'], " ") };
        r.mark_changed();
    }
}

/// The Windows 7 build has no UI Automation layer.
#[cfg(not(feature = "accessibility"))]
fn answer(_: &Ui, _: &mut Response, _: &str, _: &mut String, _: bool) {}

#[cfg(all(test, feature = "accessibility"))]
mod tests {
    use super::*;
    use eframe::egui::accesskit::{Action, ActionData, ActionRequest, Role};
    use eframe::egui::Event;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};

    #[derive(Default)]
    struct Form {
        one: String,
        many: String,
        enabled: bool,
        changed: Vec<&'static str>,
    }

    fn form() -> Harness<'static, Form> {
        Harness::new_ui_state(
            |ui, f: &mut Form| {
                ui.add_enabled_ui(f.enabled, |ui| {
                    if line(ui, &mut f.one, "Controller address", |t| t.hint_text("address")).changed() {
                        f.changed.push("one");
                    }
                });
                if lines(ui, &mut f.many, "Evidence", |t| t.desired_rows(2)).changed() {
                    f.changed.push("many");
                }
            },
            Form { enabled: true, ..Form::default() },
        )
    }

    /// What UI Automation's set-value arrives as.
    fn set_value(h: &Harness<'static, Form>, role: Role, name: &str, value: &str) {
        let (target_node, target_tree) = h.get_by_role_and_label(role, name).accesskit_node().locate();
        h.event(Event::AccessKitActionRequest(ActionRequest { action: Action::SetValue, target_node, target_tree, data: Some(ActionData::Value(value.into())) }));
    }

    #[test]
    fn a_field_is_found_by_its_name() {
        let h = form();
        assert_eq!(h.get_by_role_and_label(Role::TextInput, "Controller address").value().as_deref(), Some(""));
        assert_eq!(h.get_by_role_and_label(Role::MultilineTextInput, "Evidence").value().as_deref(), Some(""));
    }

    #[test]
    fn a_value_set_through_accessibility_is_taken_and_said_as_a_change() {
        let mut h = form();
        set_value(&h, Role::TextInput, "Controller address", "127.0.0.1\r\n:5515");
        set_value(&h, Role::MultilineTextInput, "Evidence", "line one\nline two");
        h.run();
        assert_eq!(h.state().one, "127.0.0.1  :5515", "a one-line field's line breaks become spaces");
        assert_eq!(h.state().many, "line one\nline two", "a field of several lines keeps them");
        assert_eq!(h.state().changed, ["one", "many"]);
        assert_eq!(h.get_by_role_and_label(Role::TextInput, "Controller address").value().as_deref(), Some("127.0.0.1  :5515"));
    }

    #[test]
    fn typing_after_a_set_value_goes_on_from_its_end() {
        // The field focused with its cursor further in than the new text is long.
        let mut h = form();
        h.get_by_role_and_label(Role::TextInput, "Controller address").focus();
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Controller address").type_text("192.168.125.1");
        h.run();
        set_value(&h, Role::TextInput, "Controller address", "10.0");
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Controller address").type_text(".0.2");
        h.run();
        assert_eq!(h.state().one, "10.0.0.2");
    }

    #[test]
    fn a_disabled_field_takes_nothing() {
        let mut h = form();
        h.state_mut().enabled = false;
        h.run();
        set_value(&h, Role::TextInput, "Controller address", "192.0.2.1");
        h.run();
        assert_eq!(h.state().one, "", "a field greyed out (the address while connected) was changed");
        assert!(h.state().changed.is_empty());
    }
}
