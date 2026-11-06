//! The log panel.

use super::{App, level_name, level_value};
use crate::LogLine;
use crate::i18n::{tr, tr_message};
use crate::logfile;
use lunii_core::Level;
use slint::Model;
use std::time::Duration;

impl App {
    pub(super) fn min_log_level(&self) -> i32 {
        match self.ui().get_log_level_index() {
            0 | 1 => 0,
            2 => 1,
            3 => 2,
            _ => 3,
        }
    }

    pub(super) fn log_line_visible(&self, level: i32, text: &str) -> bool {
        let filter = self.ui().get_log_filter().to_lowercase();
        level >= self.min_log_level()
            && (filter.is_empty() || text.to_lowercase().contains(&filter))
    }

    pub fn log(&self, level: Level, message: &str) {
        let level = level_value(level);
        let line = format!("[{}] {}", level_name(level), tr_message(message));
        let replace = {
            let mut st = self.state.borrow_mut();
            // progress lines replace each other
            let replace = st
                .log
                .last()
                .is_some_and(|(_, l)| l.starts_with("[DEBUG] Progress"));
            if replace {
                st.log.pop();
            }
            st.log.push((level, line.clone()));
            replace
        };
        // progress lines would flood the file
        if !line.starts_with("[DEBUG] Progress") {
            logfile::write(&line);
        }
        if level >= 2 && !self.ui().get_log_visible() {
            let alert = {
                let mut st = self.state.borrow_mut();
                st.log_alert = st.log_alert.max(level);
                st.log_alert
            };
            self.ui().set_log_alert(alert);
        }
        let visible = self.log_line_visible(level, &line);
        let last_was_progress = self.log_model.row_count() > 0
            && self
                .log_model
                .row_data(self.log_model.row_count() - 1)
                .is_some_and(|l| l.text.starts_with("[DEBUG] Progress"));
        if replace && last_was_progress {
            self.log_model.remove(self.log_model.row_count() - 1);
        }
        if visible {
            self.log_model.push(LogLine {
                text: line.into(),
                level,
            });
            self.scroll_log();
        }
    }

    pub(super) fn scroll_log(&self) {
        // delayed: the list must have laid out the new line first
        let ui = self.ui.clone();
        slint::Timer::single_shot(Duration::from_millis(10), move || {
            if let Some(ui) = ui.upgrade() {
                ui.set_log_scroll_token(ui.get_log_scroll_token().wrapping_add(1));
            }
        });
    }

    pub(super) fn rebuild_log(&self) {
        let lines: Vec<LogLine> = self
            .state
            .borrow()
            .log
            .iter()
            .filter(|(level, text)| self.log_line_visible(*level, text))
            .map(|(level, text)| LogLine {
                text: text.into(),
                level: *level,
            })
            .collect();
        self.log_model.set_vec(lines);
        self.scroll_log();
    }

    /// Copies the displayed (filtered) lines to the clipboard.
    pub(super) fn copy_log(&self) {
        let content = (0..self.log_model.row_count())
            .filter_map(|i| self.log_model.row_data(i))
            .map(|l| l.text.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(content);
            self.ui().set_status(tr("Log sent to clipboard.").into());
        }
    }

    pub(crate) fn show_log(&self) {
        let ui = self.ui();
        ui.set_log_visible(true);
        self.state.borrow_mut().log_alert = 0;
        ui.set_log_alert(0);
        self.scroll_log();
    }
}
