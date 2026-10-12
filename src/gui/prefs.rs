use super::App;
use adw::prelude::*;
use gtk::{gio, glib};
use patrak::config::{Config, parse_hhmm};
use patrak::setup;
use std::cell::RefCell;
use std::rc::Rc;

pub fn show(app: &Rc<App>) {
    let old = app.cfg.borrow().clone();
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Preferences");

    let general = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("starred-symbolic")
        .build();

    let goal_group = adw::PreferencesGroup::builder().title("Daily Goal").build();
    let goal = adw::SpinRow::with_range(0.0, 600.0, 5.0);
    goal.set_title("Video minutes per day");
    goal.set_subtitle("Lectures plus card review; 0 turns it off");
    goal.set_value(f64::from(old.daily_goal_minutes));
    let pages = adw::SpinRow::with_range(0.0, 1000.0, 5.0);
    pages.set_title("Book pages per day");
    pages.set_subtitle("0 turns it off");
    pages.set_value(f64::from(old.daily_goal_pages));
    let threshold = adw::SpinRow::with_range(50.0, 100.0, 1.0);
    threshold.set_title("Lecture counts as done at (%)");
    threshold.set_value((old.complete_threshold * 100.0).round());
    let day_start = time_row("Day starts at", &old.day_starts_at);
    let reminder = time_row("Daily reminder at", &old.reminder_time);
    for w in [
        goal.upcast_ref::<gtk::Widget>(),
        pages.upcast_ref(),
        threshold.upcast_ref(),
        day_start.upcast_ref(),
        reminder.upcast_ref(),
    ] {
        goal_group.add(w);
    }
    general.add(&goal_group);

    let apps_group = adw::PreferencesGroup::builder().title("Apps").build();
    let terminal = adw::EntryRow::builder()
        .title("Terminal for “Open in $EDITOR”")
        .text(&old.terminal)
        .build();
    apps_group.add(&terminal);
    general.add(&apps_group);

    let library = adw::PreferencesPage::builder()
        .title("Library")
        .icon_name("folder-symbolic")
        .build();
    let (video_group, video_dirs) = folder_group(
        app,
        "Video Folders",
        "Every folder inside is a course; a loose video is a one-lecture course.",
        &old.video_dirs,
    );
    let (book_group, book_dirs) = folder_group(
        app,
        "Book Folders",
        "PDF, EPUB and DjVu files anywhere inside are books.",
        &old.book_dirs,
    );
    let (project_group, project_dirs) = folder_group(
        app,
        "Project Folders",
        "Every git repository inside is a project, and so is a folder that is one itself. Your commits count toward each project’s daily goal.",
        &old.project_dirs,
    );
    library.add(&video_group);
    library.add(&book_group);
    library.add(&project_group);

    dialog.add(&general);
    dialog.add(&library);

    let weak = Rc::downgrade(app);
    dialog.connect_closed(move |_| {
        let Some(app) = weak.upgrade() else { return };
        let valid_time = |row: &adw::EntryRow, fallback: &str| {
            let text = row.text().trim().to_owned();
            match parse_hhmm(&text) {
                Some(_) => text,
                None => {
                    app.toast(&format!(
                        "“{text}” isn't a time like 21:00; kept {fallback}"
                    ));
                    fallback.to_owned()
                }
            }
        };
        let cfg = Config {
            video_dirs: video_dirs.borrow().clone(),
            book_dirs: book_dirs.borrow().clone(),
            project_dirs: project_dirs.borrow().clone(),
            daily_goal_minutes: goal.value().round() as u32,
            daily_goal_pages: pages.value().round() as u32,
            complete_threshold: threshold.value() / 100.0,
            day_starts_at: valid_time(&day_start, &old.day_starts_at),
            reminder_time: valid_time(&reminder, &old.reminder_time),
            terminal: terminal.text().trim().to_owned(),
            ..old.clone()
        };
        if cfg == old {
            return;
        }
        if let Err(e) = cfg.save(&app.paths.config) {
            app.toast(&format!("Couldn't save preferences: {e:#}"));
            return;
        }
        if cfg.reminder_time != old.reminder_time && setup::reminder_installed() {
            let (weak, timer_cfg) = (Rc::downgrade(&app), cfg.clone());
            glib::spawn_future_local(async move {
                let result = gio::spawn_blocking(move || setup::install_reminder(&timer_cfg)).await;
                if let (Some(app), Ok(Err(e))) = (weak.upgrade(), result) {
                    app.toast(&format!("Couldn't update the reminder timer: {e:#}"));
                }
            });
        }
        app.set_config(cfg);
    });
    dialog.present(Some(&app.window));
}

fn time_row(title: &str, value: &str) -> adw::EntryRow {
    adw::EntryRow::builder()
        .title(format!("{title} (HH:MM)"))
        .text(value)
        .build()
}

/// A list of folders with add and remove buttons; returns the group and the live list.
fn folder_group(
    app: &Rc<App>,
    title: &str,
    description: &str,
    dirs: &[String],
) -> (adw::PreferencesGroup, Rc<RefCell<Vec<String>>>) {
    let group = adw::PreferencesGroup::builder()
        .title(title)
        .description(description)
        .build();
    let list = Rc::new(RefCell::new(Vec::new()));
    for d in dirs {
        add_folder_row(&group, &list, d.clone());
    }

    let add = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Add a folder")
        .build();
    add.add_css_class("flat");
    let (weak, g, l) = (Rc::downgrade(app), group.clone(), list.clone());
    add.connect_clicked(move |_| {
        let Some(app) = weak.upgrade() else { return };
        let (g, l) = (g.clone(), l.clone());
        let window = app.window.clone();
        glib::spawn_future_local(async move {
            let chooser = gtk::FileDialog::builder()
                .title("Choose a Folder")
                .modal(true)
                .build();
            if let Ok(folder) = chooser.select_folder_future(Some(&window)).await
                && let Some(path) = folder.path()
            {
                let dir = contract_home(&path.to_string_lossy());
                if !l.borrow().contains(&dir) {
                    add_folder_row(&g, &l, dir);
                }
            }
        });
    });
    group.set_header_suffix(Some(&add));
    (group, list)
}

fn add_folder_row(group: &adw::PreferencesGroup, list: &Rc<RefCell<Vec<String>>>, dir: String) {
    let row = adw::ActionRow::builder()
        .title(&dir)
        .use_markup(false)
        .build();
    row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
    let remove = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Stop tracking this folder (files are not deleted)")
        .valign(gtk::Align::Center)
        .build();
    remove.add_css_class("flat");
    let (g, l, r, d) = (group.clone(), list.clone(), row.clone(), dir.clone());
    remove.connect_clicked(move |_| {
        l.borrow_mut().retain(|x| x != &d);
        g.remove(&r);
    });
    row.add_suffix(&remove);
    group.add(&row);
    list.borrow_mut().push(dir);
}

fn contract_home(path: &str) -> String {
    match dirs::home_dir().map(|h| h.to_string_lossy().into_owned()) {
        Some(home) if path == home => "~".into(),
        Some(home) => path
            .strip_prefix(&format!("{home}/"))
            .map_or_else(|| path.to_owned(), |rest| format!("~/{rest}")),
        None => path.to_owned(),
    }
}
