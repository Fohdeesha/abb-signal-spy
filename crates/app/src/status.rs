use eframe::egui::{self, Color32, Stroke};

use spy_core::session::{ChannelState, Status};

use crate::app::SpyApp;
use crate::net;
use crate::theme::{self, Icon, Mark};

pub struct Summary {
    pub word: String,
    pub color: Color32,
    pub mark: Mark,
    pub rule: Color32,
    pub controller: String,
    pub system: Option<String>,
    pub channels: String,
    pub recording: Option<String>,
    pub rec_dir: Option<String>,
    pub others: Vec<String>,
}

impl Summary {
    pub fn short_word(&self) -> &str {
        self.word.split([',', '(']).next().unwrap_or(&self.word).trim()
    }

    fn others_line(&self) -> String {
        match self.others.as_slice() {
            [one] => format!("also connected: {one}"),
            many => format!("also connected: {} programs", many.len()),
        }
    }

    fn others_tip(&self) -> String {
        format!("Connected to this controller over RobAPI:\n{}\n\nRobotStudio counts whenever it is connected, even when it is not streaming.", self.others.join("\n"))
    }
}

pub const ROW_LABEL: f32 = 100.0;

impl SpyApp {
    pub fn summary(&self, st: &Status, p: &theme::Pal) -> Summary {
        let (word, color, mark) = Self::phase_word(st, p);
        let rule = if mark == Mark::Off && color == p.ink2 { p.ink } else { color };
        let controller = match &st.target {
            Some(t) if st.loopback => format!("VC {} : {}", t.host, t.port),
            Some(t) => format!("{} : {}", t.host, t.port),
            None => "no controller".into(),
        };
        let system = self.rws.as_ref().and_then(|l| l.system.as_ref()).map(|s| format!("{} · RobotWare {} (from its RWS)", s.name, s.rw_version));
        let live: Vec<f64> = st.channels.iter().filter(|c| matches!(c.state, ChannelState::Defined { .. }) && c.rate > 0.0).map(|c| c.rate).collect();
        let rate = match (live.iter().cloned().fold(f64::INFINITY, f64::min), live.iter().cloned().fold(0.0, f64::max)) {
            _ if live.is_empty() => String::new(),
            (lo, hi) if hi - lo <= hi * 0.05 => format!(" · {hi:.0} /s each"),
            (lo, hi) => format!(" · {lo:.0} to {hi:.0} /s"),
        };
        let channels = format!("{}{rate}", crate::channels::count_text(self.chans.len(), self.derived.len()));
        let recording = match (&self.recorder, &self.slow) {
            (Some(r), slow) => {
                let s = r.status();
                Some(format!("recording {} · {}{}", crate::record::clock(s.started.elapsed().as_secs()), crate::record::size_text(s.bytes), if slow.is_some() { " + slow log" } else { "" }))
            }
            (None, Some(r)) => {
                let s = r.status();
                Some(format!("slow log {} · {} rows", crate::record::clock(s.started.elapsed().as_secs()), s.rows))
            }
            (None, None) => None,
        };
        let rec_dir = self.recorder.as_ref().or(self.slow.as_ref()).map(|r| r.status().dir.display().to_string());
        let shown = Self::others_shown(st);
        for o in &shown {
            let c = self.ctx.clone();
            net::lookup(&self.hostnames, &o.address, move || c.request_repaint());
        }
        let others = if shown.is_empty() { Vec::new() } else { self.client_names(&shown) };
        Summary { word, color, mark, rule, controller, system, channels, recording, rec_dir, others }
    }

    pub fn status_block(&mut self, ui: &mut egui::Ui) -> Color32 {
        let p = theme::pal(ui);
        let st = self.session.status().clone();
        let s = self.summary(&st, p);
        ui.spacing_mut().item_spacing.y = 4.0;

        let word = theme::keep_together(&s.word);
        let size = theme::fit_size(ui, &word, ui.available_width() - 18.0, &[16.0, 15.0, 14.0]);
        marked_line(ui, s.mark, s.color, &word, size, true);

        let open = self.settings.status_open;
        let mut toggle = false;
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 26.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (icon, name) = if open { (Icon::Up, "Show less of the status") } else { (Icon::Down, "Show the whole status") };
            toggle = theme::icon_button(ui, icon, name, egui::vec2(30.0, 26.0)).clicked();
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add_space(18.0);
                let r = ui.add(egui::Label::new(theme::b(&s.controller).size(14.0).color(p.ink2)).truncate());
                if let Some(sys) = &s.system {
                    r.on_hover_text(sys);
                }
            });
        });

        if let Some(rec) = &s.recording {
            let r = marked_line(ui, Mark::On, p.red, rec, 15.0, false);
            if let Some(dir) = &s.rec_dir {
                r.on_hover_text(dir);
            }
        }
        if !s.others.is_empty() {
            marked_line(ui, Mark::On, p.hold, &s.others_line(), 15.0, false).on_hover_text(s.others_tip());
        }

        if open {
            ui.add_space(2.0);
            let y = ui.cursor().top();
            ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, p.line));
            ui.add_space(4.0);
            row(ui, "channels", &s.channels, p.ink);
            if s.recording.is_none() {
                row(ui, "recording", "off", p.ink2);
            }
            if s.others.is_empty() {
                row(ui, "other programs", "none", p.ink);
            }
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 24.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.link(theme::b("details")).on_hover_text("The connection's counters, rates and identity").clicked() {
                    self.show_diag = true;
                }
            });
        }
        if toggle {
            self.settings.status_open = !open;
            self.mark_settings_dirty();
        }
        s.rule
    }

    pub fn status_rail(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        let st = self.session.status().clone();
        let s = self.summary(&st, p);
        ui.spacing_mut().item_spacing.y = 6.0;
        let tip = |extra: &str| format!("{}\n{}{extra}", s.word, s.controller);
        rail_entry(ui, s.mark, s.color, s.rule, s.short_word()).on_hover_text(tip(""));
        if let Some(rec) = &s.recording {
            let short = rec.replace("recording ", "rec ");
            let short = short.split(" · ").next().unwrap_or(&short).to_string();
            rail_entry(ui, Mark::On, p.red, p.red, &short).on_hover_text(rec);
        }
        if !s.others.is_empty() {
            let n = s.others.len();
            rail_entry(ui, Mark::On, p.hold, p.hold, &format!("+{n} program{}", if n == 1 { "" } else { "s" })).on_hover_text(s.others_tip());
        }
    }

    pub fn status_line(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        let st = self.session.status().clone();
        let s = self.summary(&st, p);
        theme::vrule(ui, 26.0);
        ui.spacing_mut().item_spacing.x = 8.0;
        mark(ui, s.mark, s.color, 10.0);
        ui.label(theme::b(&s.word).color(s.color));
        let r = ui.label(theme::b(&s.controller).color(p.ink2));
        if let Some(sys) = &s.system {
            r.on_hover_text(sys);
        }
        if let Some(rec) = &s.recording {
            ui.add_space(6.0);
            theme::square(ui, p.red, 10.0);
            ui.label(theme::b(rec).color(p.red));
        }
        if !s.others.is_empty() {
            ui.add_space(6.0);
            theme::square(ui, p.hold, 10.0);
            ui.label(theme::b(s.others_line()).color(p.hold)).on_hover_text(s.others_tip());
        }
        ui.add_space(6.0);
        if ui.link(theme::b("details")).on_hover_text("The connection's counters, rates and identity").clicked() {
            self.show_diag = true;
        }
        theme::vrule(ui, 26.0);
    }
}

fn mark(ui: &mut egui::Ui, mark: Mark, color: Color32, size: f32) {
    match mark {
        Mark::On => theme::square(ui, color, size),
        Mark::Off => theme::hollow(ui, color, size),
    }
}

fn marked_line(ui: &mut egui::Ui, m: Mark, color: Color32, text: &str, size: f32, wrap: bool) -> egui::Response {
    let line_h = ui.fonts_mut(|f| f.row_height(&egui::FontId::new(size, theme::bold())));
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, line_h), egui::Sense::hover());
        let sq = egui::Rect::from_center_size(rect.center(), egui::vec2(10.0, 10.0));
        match m {
            Mark::On => {
                ui.painter().rect_filled(sq, 0.0, color);
            }
            Mark::Off => {
                ui.painter().rect_stroke(sq, 0.0, Stroke::new(1.5, color), egui::StrokeKind::Inside);
            }
        }
        let label = egui::Label::new(theme::b(text).size(size).color(color));
        ui.add(if wrap { label.wrap() } else { label.truncate() })
    })
    .inner
}

fn row(ui: &mut egui::Ui, label: &str, value: &str, color: Color32) {
    labelled_row(ui, label, |ui| ui.add(egui::Label::new(theme::b(value).size(15.0).color(color)).truncate()));
}

pub fn labelled_row(ui: &mut egui::Ui, label: &str, value: impl FnOnce(&mut egui::Ui) -> egui::Response) -> egui::Response {
    let p = theme::pal(ui);
    ui.horizontal(|ui| {
        ui.set_min_height(24.0);
        ui.allocate_ui_with_layout(egui::vec2(ROW_LABEL, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_width(ROW_LABEL);
            ui.label(egui::RichText::new(label).size(14.0).color(p.ink2));
        });
        value(ui)
    })
    .inner
}

pub fn draw_mark(ui: &mut egui::Ui, m: Mark, color: Color32) {
    mark(ui, m, color, 10.0);
}

fn rail_entry(ui: &mut egui::Ui, m: Mark, color: Color32, rule: Color32, text: &str) -> egui::Response {
    let y = ui.cursor().top();
    ui.painter().hline(ui.max_rect().x_range(), y + 1.0, Stroke::new(2.0, rule));
    ui.add_space(8.0);
    ui.vertical_centered(|ui| mark(ui, m, color, 10.0));
    theme::upright_label(ui, text, color)
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Color32, FontId};

    use crate::theme;

    #[test]
    fn the_longest_retry_word_fits_one_line_at_the_lists_usual_width() {
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        theme::apply(&ctx, true, 1.0);
        let width = crate::app::SIGNALS_WIDTH - 20.0 - 18.0;
        let mut seen = Vec::new();
        let mut squeezed = (0.0, 0.0, 0.0);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let wide = |ui: &egui::Ui, word: &str, size: f32| ui.fonts_mut(|f| f.layout_no_wrap(word.to_string(), FontId::new(size, theme::bold()), Color32::WHITE).size().x);
            for word in ["reconnecting, try 2 (every 2 s)", "reconnecting, try 12 (every 30 s)", "waiting for your answer"] {
                let word = theme::keep_together(word);
                let size = theme::fit_size(ui, &word, width, &[16.0, 15.0, 14.0]);
                seen.push((word.clone(), size, wide(ui, &word, size)));
            }
            let long = theme::keep_together("reconnecting, try 12 (every 30 s)");
            let between = (wide(ui, &long, 14.0) + wide(ui, &long, 16.0)) / 2.0;
            let size = theme::fit_size(ui, &long, between, &[16.0, 15.0, 14.0]);
            squeezed = (between, size, wide(ui, &long, size));
        })
        .textures_delta
        .clear();
        for (word, size, w) in &seen {
            assert!(*w <= width, "{word:?} is {w:.0} px at {size} px, wider than {width:.0}");
        }
        assert_eq!(seen[0].1, 16.0, "a usual retry keeps the full size: {seen:?}");
        let (between, size, w) = squeezed;
        assert!(size < 16.0 && w <= between, "a narrower list gets a smaller size that fits: {size} px is {w:.0} of {between:.0}");
    }
}
