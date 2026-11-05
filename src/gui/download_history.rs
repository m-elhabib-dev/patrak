//! The history section of the Downloads page.

use super::widgets::{icon_button, placeholder_row, row, section_with};
use super::{Act, Action};
use adw::prelude::*;
use chrono::{Local, TimeZone};
use gtk::gio;
use patrak::db::{DownloadRecord, DownloadStatus};
use std::cell::RefCell;

pub struct History {
    pub root: gtk::Box,
    list: gtk::ListBox,
    clear: gtk::Button,
    act: Act,
    shown: RefCell<Option<Vec<DownloadRecord>>>,
}

impl History {
    pub fn new(act: Act) -> Self {
        let clear = gtk::Button::builder()
            .label("Clear")
            .tooltip_text("Clear the history; downloaded files stay")
            .build();
        clear.add_css_class("flat");
        let a = act.clone();
        clear.connect_clicked(move |_| a(Action::ClearDownloads));
        let (root, list) = section_with("History", &clear);
        Self {
            root,
            list,
            clear,
            act,
            shown: RefCell::new(None),
        }
    }

    /// The ids of the listed downloads.
    pub fn ids(&self) -> Vec<i64> {
        let shown = self.shown.borrow();
        shown.iter().flatten().map(|r| r.id).collect()
    }

    pub fn update(&self, records: Vec<DownloadRecord>) {
        if self.shown.borrow().as_ref() == Some(&records) {
            return;
        }
        self.list.remove_all();
        for record in &records {
            self.list.append(&history_row(record, &self.act));
        }
        if records.is_empty() {
            self.list.append(&placeholder_row("No downloads yet."));
        }
        self.clear.set_visible(!records.is_empty());
        *self.shown.borrow_mut() = Some(records);
    }
}

fn history_row(record: &DownloadRecord, act: &Act) -> adw::ActionRow {
    let (icon, status) = match record.status {
        DownloadStatus::Finished => ("emblem-ok-symbolic", "Finished"),
        DownloadStatus::Failed => ("dialog-error-symbolic", "Failed"),
        DownloadStatus::Running | DownloadStatus::Stopped => {
            ("media-playback-stop-symbolic", "Stopped")
        }
    };
    let r = row(&record.title, &format!("{status}  ·  {}", when(record.at)));
    r.add_prefix(&gtk::Image::from_icon_name(icon));

    let mut details = vec![record.url.clone()];
    details.extend(record.dir.iter().map(|d| d.display().to_string()));
    details.extend(record.error.clone());
    r.set_tooltip_text(Some(&details.join("\n")));

    if let Some(dir) = record.dir.as_ref().filter(|d| d.is_dir()) {
        let uri = gio::File::for_path(dir).uri().to_string();
        r.add_suffix(&icon_button(
            "folder-open-symbolic",
            "Open folder",
            act,
            move || Action::OpenUrl(uri.clone()),
        ));
    }
    let url = record.url.clone();
    r.add_suffix(&icon_button(
        "web-browser-symbolic",
        "Open the link",
        act,
        move || Action::OpenUrl(url.clone()),
    ));
    let url = record.url.clone();
    let course = record.dir.clone().filter(|_| record.course);
    r.add_suffix(&icon_button(
        "folder-download-symbolic",
        "Download again; what’s already there is skipped",
        act,
        move || Action::DownloadAgain {
            url: url.clone(),
            course: course.clone(),
        },
    ));
    let id = record.id;
    r.add_suffix(&icon_button(
        "user-trash-symbolic",
        "Remove from the history; downloaded files stay",
        act,
        move || Action::RemoveDownload(id),
    ));
    r
}

fn when(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .earliest()
        .map(|t| t.format("%-d %b %Y, %H:%M").to_string())
        .unwrap_or_default()
}
