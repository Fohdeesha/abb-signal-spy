use eframe::egui::{self, Margin, Response, Stroke, TextEdit, Ui};

use crate::theme;

pub const HEIGHT: f32 = 40.0;

pub fn line(ui: &mut Ui, text: &mut String, name: &str, shape: impl FnOnce(TextEdit<'_>) -> TextEdit<'_>) -> Response {
    let edit = TextEdit::singleline(text).min_size(egui::vec2(0.0, HEIGHT)).vertical_align(egui::Align::Center);
    let mut r = framed(ui, shape(edit));
    answer(ui, &mut r, name, text, false);
    r
}

pub fn lines(ui: &mut Ui, text: &mut String, name: &str, shape: impl FnOnce(TextEdit<'_>) -> TextEdit<'_>) -> Response {
    let mut r = framed(ui, shape(TextEdit::multiline(text)));
    answer(ui, &mut r, name, text, true);
    r
}

fn framed(ui: &mut Ui, edit: TextEdit<'_>) -> Response {
    let under = ui.painter().add(egui::Shape::Noop);
    let r = ui.add(edit.frame(egui::Frame::new().inner_margin(Margin::symmetric(10, 4))));
    let p = theme::pal(ui);
    let rect = r.rect;
    if !r.enabled() {
        ui.painter().set(under, egui::Shape::rect_filled(rect, 0.0, p.off_face));
        theme::dashed_rect(ui.painter(), rect, Stroke::new(1.0, p.off_edge));
    } else {
        let edge = if r.has_focus() {
            Stroke::new(2.0, p.ink)
        } else if r.hovered() {
            Stroke::new(1.0, p.field_edge_hover)
        } else {
            Stroke::new(1.0, p.field_edge)
        };
        ui.painter().set(under, egui::Shape::Rect(egui::epaint::RectShape::new(rect, 0.0, p.field, edge, egui::StrokeKind::Inside)));
    }
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
