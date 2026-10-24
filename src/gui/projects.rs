use super::widgets::{done_mark, icon_button, isolate, label, page, progress, row, section};
use super::{Act, Action, App};
use adw::prelude::*;
use chrono::NaiveDate;
use gtk::gio;
use patrak::db;
use patrak::progress::Snapshot;
use patrak::projects::{self, Project, ProjectStatus};
use std::rc::Rc;

pub struct ProjectsPage {
    pub root: gtk::Stack,
    list: gtk::ListBox,
    act: Act,
}

impl ProjectsPage {
    pub fn new(act: Act) -> Self {
        let (scroller, content) = page(720);
        let (section_box, list) = section("Projects");
        section_box.append(&label(
            "Your commits in each project count toward its daily goal, which is part of the daily goal: a day keeps your streak only when every project with a goal reaches it. Merges and other people’s commits don’t count.",
            &["dim-label", "caption"],
        ));
        content.append(&section_box);

        let empty = adw::StatusPage::builder()
            .icon_name("applications-engineering-symbolic")
            .title("No Projects Yet")
            .description("Put git repositories, or links to them, in one of your project folders (Preferences → Library). Each commit you make counts toward the project’s daily goal.")
            .build();
        let root = gtk::Stack::new();
        root.add_named(&scroller, Some("projects"));
        root.add_named(&empty, Some("empty"));
        root.set_visible_child_name("empty");
        Self { root, list, act }
    }

    pub fn update(&self, snap: &Snapshot) {
        self.root
            .set_visible_child_name(if snap.projects.is_empty() {
                "empty"
            } else {
                "projects"
            });
        self.list.remove_all();
        for status in &snap.projects {
            let r = project_row(status, &details(status, snap.today));
            let id = status.project.id;
            if !status.project.missing {
                r.add_suffix(&icon_button(
                    "document-edit-symbolic",
                    "Change daily goal",
                    &self.act,
                    move || Action::EditProject(id),
                ));
                let uri = gio::File::for_path(&status.project.path).uri().to_string();
                r.add_suffix(&icon_button(
                    "folder-open-symbolic",
                    "Open folder",
                    &self.act,
                    move || Action::OpenUrl(uri.clone()),
                ));
            }
            self.list.append(&r);
        }
    }
}

/// A project with today's commits against its goal: a check once it is met, then a bar.
pub fn project_row(status: &ProjectStatus, subtitle: &str) -> adw::ActionRow {
    let r = row(&status.project.name, subtitle);
    r.set_subtitle_lines(2);
    r.add_prefix(&done_mark(status.done()));
    if status.project.has_goal() {
        r.add_suffix(&progress(status.project.share(status.today), 90));
    }
    r
}

/// "2 of 3 commits today  ·  4-day streak"
pub fn summary(status: &ProjectStatus) -> String {
    let mut parts = vec![format!(
        "{} today",
        status.project.progress_text(status.today)
    )];
    parts.extend(status.streak_text());
    parts.join("  ·  ")
}

/// The summary, plus when the last commit was and whether there's a goal.
fn details(status: &ProjectStatus, today: NaiveDate) -> String {
    let mut parts = vec![if status.project.missing {
        "folder missing".to_owned()
    } else {
        summary(status)
    }];
    if status.project.missing {
        parts.extend(status.streak_text());
        if status.total > 0 {
            parts.push(format!("{} in all", projects::count(status.total)));
        }
    } else if !status.project.has_goal() {
        parts.push("no daily goal".into());
    }
    if let Some(last) = status.last_commit.filter(|&d| d < today) {
        parts.push(format!("last commit {}", last.format("%b %-d")));
    }
    parts.join("  ·  ")
}

/// Asks for a project's daily goal in commits; 0 turns it off.
pub fn edit_goal(app: &Rc<App>, project: &Project) {
    let goal = adw::SpinRow::with_range(0.0, 100.0, 1.0);
    goal.set_title("Commits a day");
    goal.set_value(project.goal as f64);
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list.add_css_class("boxed-list");
    list.append(&goal);

    let alert = adw::AlertDialog::new(
        Some(&format!("Daily Goal for {}", isolate(&project.name))),
        Some("With 0, the project is still tracked but doesn’t affect your streak."),
    );
    alert.set_extra_child(Some(&list));
    alert.add_responses(&[("cancel", "Cancel"), ("save", "Save")]);
    alert.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("save"));
    alert.set_close_response("cancel");
    let (weak, id) = (Rc::downgrade(app), project.id);
    alert.connect_response(Some("save"), move |_, _| {
        let Some(app) = weak.upgrade() else { return };
        // Take a number typed without pressing Enter.
        goal.update();
        let goal = goal.value().round() as i64;
        match db::open(&app.paths.db()).and_then(|conn| projects::set_goal(&conn, id, goal)) {
            Ok(()) => app.refresh(),
            Err(e) => app.toast(&format!("Couldn't save the goal: {e:#}")),
        }
    });
    alert.present(Some(&app.window));
}
