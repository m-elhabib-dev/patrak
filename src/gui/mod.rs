mod books;
mod courses;
mod download_dialog;
mod habit_dialog;
mod habits;
mod notes;
mod prefs;
mod projects;
mod review_dialog;
mod stats;
mod today;
mod widgets;

use adw::prelude::*;
use chrono::NaiveDate;
use gtk::{gio, glib};
use patrak::config::{self, Config, Paths};
use patrak::progress::Snapshot;
use patrak::sync::{self, Report};
use patrak::{Session, db, download, launch, setup};
use std::cell::{Cell, RefCell};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// Playlists are checked for new episodes when the last check is older than this.
const SOURCE_CHECK_EVERY: i64 = 20 * 3600;
/// After a failed check (offline, say), wait this long before trying again.
const SOURCE_RETRY_AFTER: i64 = 3600;

/// Something a page asks the app to do.
pub enum Action {
    /// Play a course as a playlist from lecture `start`.
    Play {
        course_key: String,
        start: usize,
    },
    /// Play one video from a given second (used for chapters).
    PlayAt {
        item_id: i64,
        secs: f64,
    },
    Read(i64),
    /// Open a book at a page (used for `[p. 42]` stamps in notes).
    ReadAt {
        item_id: i64,
        page: i64,
    },
    Note(i64),
    SetDone(i64, bool),
    SetDeadline(String, Option<NaiveDate>),
    /// Daily goals in minutes and pages; 0 turns one off.
    SetGoals {
        minutes: u32,
        pages: u32,
    },
    Review,
    /// Today's amount of a habit, replacing what was logged before.
    LogHabit {
        id: i64,
        amount: i64,
    },
    /// Add one time to a habit, or complete a minutes habit.
    QuickLogHabit(i64),
    NewHabit,
    EditHabit(i64),
    /// Change a project's daily goal.
    EditProject(i64),
    /// Make today a holiday, or a study day again.
    SetHoliday(bool),
    /// Look for new episodes of a downloaded course.
    CheckCourse(PathBuf),
    /// Download new episodes of a downloaded course.
    UpdateCourse(PathBuf),
    OpenUrl(String),
}

pub type Act = Rc<dyn Fn(Action)>;

const CSS: &str = "
.heat { min-width: 13px; min-height: 13px; border-radius: 3px; }
.heat-0 { background-color: alpha(currentColor, 0.08); }
.heat-1 { background-color: alpha(@accent_bg_color, 0.30); }
.heat-2 { background-color: alpha(@accent_bg_color, 0.55); }
.heat-3 { background-color: alpha(@accent_bg_color, 0.85); }
.heat-4 { background-color: @success_bg_color; }
.heat-holiday { background-color: @purple_3; }
.holiday-pill { background-color: @purple_3; color: white; border-radius: 999px; padding: 2px 10px; font-weight: bold; }
.stat-card { padding: 14px 18px; }
levelbar.goal block { min-height: 14px; border-radius: 7px; }
";

/// Loaded above the user's own gtk.css, whose `button:checked` would otherwise paint the
/// holiday button in the theme's accent colour.
const HOLIDAY_CSS: &str = "
button.holiday:checked { background-color: @purple_3; color: white; }
button.holiday:checked:hover { background-color: shade(@purple_3, 1.1); }
";

/// Opens the window, showing `page` (today, courses, books, projects, habits or stats) if given.
pub fn run(page: Option<String>) {
    let app = adw::Application::builder()
        .application_id(setup::APP_ID)
        .build();
    app.connect_startup(|_| {
        let Some(display) = gtk::gdk::Display::default() else {
            return;
        };
        for (css, priority) in [
            (CSS, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION),
            (HOLIDAY_CSS, gtk::STYLE_PROVIDER_PRIORITY_USER + 1),
        ] {
            let provider = gtk::CssProvider::new();
            provider.load_from_string(css);
            gtk::style_context_add_provider_for_display(&display, &provider, priority);
        }
    });
    app.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        App::build(app, page.as_deref());
    });
    // clap has already handled the command line.
    app.run_with_args(&[] as &[&str]);
}

struct App {
    window: adw::ApplicationWindow,
    stack: adw::ViewStack,
    toasts: adw::ToastOverlay,
    banner: adw::Banner,
    today: today::TodayPage,
    courses: courses::CoursesPage,
    books: books::BooksPage,
    projects: projects::ProjectsPage,
    habits: habits::HabitsPage,
    stats: stats::StatsPage,
    /// Whether today is a holiday; the header's toggle button follows its state.
    holiday: gio::SimpleAction,
    paths: Paths,
    cfg: RefCell<Config>,
    snapshot: RefCell<Option<Rc<Snapshot>>>,
    syncing: Cell<bool>,
    sync_again: Cell<bool>,
    refresh_scheduled: Cell<bool>,
    monitors: RefCell<Vec<gio::FileMonitor>>,
    checking_sources: Cell<bool>,
    last_source_check: Cell<i64>,
}

impl App {
    fn build(gtk_app: &adw::Application, page: Option<&str>) {
        let paths = Paths::from_env();
        let cfg = Config::load(&paths.config).unwrap_or_default();

        let app = Rc::new_cyclic(|weak: &std::rc::Weak<App>| {
            let weak = weak.clone();
            let act: Act = Rc::new(move |action| {
                if let Some(app) = weak.upgrade() {
                    app.handle(action);
                }
            });

            let today = today::TodayPage::new(act.clone());
            let courses = courses::CoursesPage::new(act.clone());
            let books = books::BooksPage::new(act.clone());
            let projects = projects::ProjectsPage::new(act.clone());
            let habits = habits::HabitsPage::new(act.clone());
            let stats = stats::StatsPage::new();

            let stack = adw::ViewStack::new();
            stack.add_titled_with_icon(&today.root, Some("today"), "Today", "starred-symbolic");
            stack.add_titled_with_icon(
                &courses.root,
                Some("courses"),
                "Courses",
                "video-x-generic-symbolic",
            );
            stack.add_titled_with_icon(
                &books.root,
                Some("books"),
                "Books",
                "accessories-dictionary-symbolic",
            );
            stack.add_titled_with_icon(
                &projects.root,
                Some("projects"),
                "Projects",
                "applications-engineering-symbolic",
            );
            stack.add_titled_with_icon(
                &habits.root,
                Some("habits"),
                "Habits",
                "object-select-symbolic",
            );
            stack.add_titled_with_icon(
                &stats.root,
                Some("stats"),
                "Stats",
                "x-office-spreadsheet-symbolic",
            );

            let switcher = adw::ViewSwitcher::builder()
                .stack(&stack)
                .policy(adw::ViewSwitcherPolicy::Wide)
                .build();
            let switcher_bar = adw::ViewSwitcherBar::builder().stack(&stack).build();

            let menu = gio::Menu::new();
            menu.append(Some("Preferences"), Some("app.preferences"));
            menu.append(Some("About patrak"), Some("app.about"));
            let menu_button = gtk::MenuButton::builder()
                .icon_name("open-menu-symbolic")
                .menu_model(&menu)
                .primary(true)
                .tooltip_text("Main menu")
                .build();
            let refresh = gtk::Button::builder()
                .icon_name("view-refresh-symbolic")
                .tooltip_text("Rescan library (Ctrl+R)")
                .action_name("app.refresh")
                .build();
            let add = gtk::Button::builder()
                .icon_name("list-add-symbolic")
                .tooltip_text("Add a course from YouTube (Ctrl+N)")
                .action_name("app.add-course")
                .build();

            let holiday = gio::SimpleAction::new_stateful("holiday", None, &false.to_variant());
            let holiday_button = gtk::ToggleButton::builder()
                .icon_name("weather-clear-symbolic")
                .tooltip_text("Holiday: turn off today’s goals (Ctrl+H)")
                .action_name("app.holiday")
                .build();
            holiday_button.add_css_class("holiday");

            let header = adw::HeaderBar::new();
            header.set_title_widget(Some(&switcher));
            header.pack_start(&add);
            header.pack_start(&refresh);
            header.pack_end(&menu_button);
            header.pack_end(&holiday_button);

            let banner = adw::Banner::builder()
                .title("Finish setup to track mpv watch time and get reminders")
                .button_label("Set Up")
                .build();

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&header);
            toolbar.add_top_bar(&banner);
            toolbar.add_bottom_bar(&switcher_bar);
            toolbar.set_content(Some(&stack));

            let toasts = adw::ToastOverlay::new();
            toasts.set_child(Some(&toolbar));

            let window = adw::ApplicationWindow::builder()
                .application(gtk_app)
                .title("patrak")
                .default_width(1100)
                .default_height(760)
                .width_request(360)
                .height_request(400)
                .content(&toasts)
                .build();

            let narrow = adw::Breakpoint::new(
                adw::BreakpointCondition::parse("max-width: 700sp").expect("valid breakpoint"),
            );
            narrow.add_setter(&switcher_bar, "reveal", Some(&true.to_value()));
            narrow.add_setter(&switcher, "visible", Some(&false.to_value()));
            narrow.add_setter(&courses.root, "collapsed", Some(&true.to_value()));
            window.add_breakpoint(narrow);

            App {
                window,
                stack,
                toasts,
                banner,
                today,
                courses,
                books,
                projects,
                habits,
                stats,
                holiday,
                paths,
                cfg: RefCell::new(cfg),
                snapshot: RefCell::new(None),
                syncing: Cell::new(false),
                sync_again: Cell::new(false),
                refresh_scheduled: Cell::new(false),
                monitors: RefCell::new(Vec::new()),
                checking_sources: Cell::new(false),
                last_source_check: Cell::new(0),
            }
        });

        if let Some(page) = page {
            app.stack.set_visible_child_name(page);
        }
        app.install_actions(gtk_app);
        app.update_banner();
        app.watch_sources();

        let weak = Rc::downgrade(&app);
        app.banner.connect_button_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.run_setup();
            }
        });

        let weak = Rc::downgrade(&app);
        app.window.connect_is_active_notify(move |w| {
            if let Some(app) = weak.upgrade()
                && w.is_active()
            {
                app.refresh();
            }
        });

        let weak = Rc::downgrade(&app);
        glib::timeout_add_seconds_local(60, move || match weak.upgrade() {
            Some(app) => {
                app.refresh();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });

        // Everything else holds the app weakly; the window owns it until it is destroyed.
        let keep_alive = app.clone();
        app.window.connect_destroy(move |_| {
            let _ = &keep_alive;
        });

        app.window.present();
        app.refresh();
    }

    fn install_actions(self: &Rc<Self>, gtk_app: &adw::Application) {
        let add = |name: &str, accels: &[&str], f: fn(&Rc<App>)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(app) = weak.upgrade() {
                    f(&app);
                }
            });
            gtk_app.add_action(&action);
            gtk_app.set_accels_for_action(&format!("app.{name}"), accels);
        };
        add("refresh", &["<Control>r", "F5"], |app| app.refresh());
        add("preferences", &["<Control>comma"], prefs::show);
        add("about", &[], |app| app.show_about());
        add("add-course", &["<Control>n"], download_dialog::show_add);
        add("quit", &["<Control>q"], |app| app.window.close());

        let weak = Rc::downgrade(self);
        self.holiday.connect_activate(move |action, _| {
            let on = action.state().and_then(|s| s.get::<bool>()) == Some(true);
            if let Some(app) = weak.upgrade() {
                app.handle(Action::SetHoliday(!on));
            }
        });
        gtk_app.add_action(&self.holiday);
        gtk_app.set_accels_for_action("app.holiday", &["<Control>h"]);
    }

    fn handle(self: &Rc<Self>, action: Action) {
        let Some(snap) = self.snapshot.borrow().clone() else {
            return;
        };
        match action {
            Action::Play { course_key, start } => {
                if let Some(course) = snap.courses.iter().find(|c| c.key == course_key) {
                    self.spawn(launch::play_args(course, start));
                }
            }
            Action::PlayAt { item_id, secs } => {
                if let Some(item) = snap.item(item_id) {
                    self.spawn(launch::play_at_args(item, secs));
                }
            }
            Action::ReadAt { item_id, page } => {
                if let Some(book) = snap.item(item_id) {
                    self.spawn(launch::read_at_args(book, page));
                }
            }
            Action::Read(id) => {
                if let Some(book) = snap.item(id) {
                    self.spawn(launch::read_args(book));
                }
            }
            Action::Note(id) => {
                if let Some(item) = snap.item(id) {
                    notes::show(self, item.clone());
                }
            }
            Action::SetDone(id, done) => {
                let result =
                    db::open(&self.paths.db()).and_then(|conn| sync::set_done(&conn, id, done));
                match result {
                    Ok(()) => self.refresh(),
                    Err(e) => self.toast(&format!("Couldn't update: {e:#}")),
                }
            }
            Action::SetDeadline(key, date) => {
                let result =
                    db::open(&self.paths.db()).and_then(|conn| db::set_deadline(&conn, &key, date));
                match result {
                    Ok(()) => self.refresh(),
                    Err(e) => self.toast(&format!("Couldn't save the deadline: {e:#}")),
                }
            }
            Action::SetGoals { minutes, pages } => {
                let cfg = Config {
                    daily_goal_minutes: minutes,
                    daily_goal_pages: pages,
                    ..self.cfg.borrow().clone()
                };
                if cfg == *self.cfg.borrow() {
                    return;
                }
                match cfg.save(&self.paths.config) {
                    Ok(()) => self.set_config(cfg),
                    Err(e) => self.toast(&format!("Couldn't save the goals: {e:#}")),
                }
            }
            Action::Review => review_dialog::show(self),
            // The day on screen, which the amount was read from.
            Action::LogHabit { id, amount } => {
                let result = db::open(&self.paths.db())
                    .and_then(|conn| patrak::habits::set_amount(&conn, id, snap.today, amount));
                match result {
                    Ok(()) => self.refresh(),
                    Err(e) => self.toast(&format!("Couldn't log the habit: {e:#}")),
                }
            }
            Action::QuickLogHabit(id) => {
                let Some(h) = snap.habits.iter().find(|h| h.habit.id == id) else {
                    return;
                };
                let result = db::open(&self.paths.db())
                    .and_then(|conn| patrak::habits::quick_log(&conn, &h.habit, snap.today));
                match result {
                    Ok(()) => self.refresh(),
                    Err(e) => self.toast(&format!("Couldn't log the habit: {e:#}")),
                }
            }
            Action::NewHabit => habit_dialog::show(self, None),
            Action::EditHabit(id) => {
                if let Some(h) = snap.habits.iter().find(|h| h.habit.id == id) {
                    habit_dialog::show(self, Some(h.habit.clone()));
                }
            }
            Action::EditProject(id) => {
                if let Some(p) = snap.projects.iter().find(|p| p.project.id == id) {
                    projects::edit_goal(self, &p.project);
                }
            }
            // The day on screen.
            Action::SetHoliday(on) => {
                let result = db::open(&self.paths.db())
                    .and_then(|conn| db::set_holiday(&conn, snap.today, on));
                match result {
                    Ok(()) => {
                        self.holiday.set_state(&on.to_variant());
                        self.toast(if on {
                            "Today is a holiday: goals are off and your streak is safe"
                        } else {
                            "Today is a study day again"
                        });
                        self.refresh();
                    }
                    Err(e) => self.toast(&format!("Couldn't change the holiday: {e:#}")),
                }
            }
            Action::CheckCourse(dir) => self.check_sources(vec![dir], true),
            Action::UpdateCourse(dir) => download_dialog::show_update(self, dir),
            Action::OpenUrl(url) => {
                gtk::UriLauncher::new(&url).launch(
                    Some(&self.window),
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
        }
    }

    /// Starts an external program and resyncs when it exits.
    fn spawn(self: &Rc<Self>, args: Vec<OsString>) {
        let argv: Vec<&OsStr> = args.iter().map(OsString::as_os_str).collect();
        match gio::Subprocess::newv(&argv, gio::SubprocessFlags::NONE) {
            Ok(process) => {
                let weak = Rc::downgrade(self);
                glib::spawn_future_local(async move {
                    let _ = process.wait_future().await;
                    if let Some(app) = weak.upgrade() {
                        app.refresh();
                    }
                });
            }
            Err(e) => self.toast(&format!(
                "Couldn't start {}: {}",
                args[0].to_string_lossy(),
                e.message()
            )),
        }
    }

    /// Syncs on a worker thread and updates every page with the result.
    fn refresh(self: &Rc<Self>) {
        if self.syncing.replace(true) {
            self.sync_again.set(true);
            return;
        }
        let cfg = self.cfg.borrow().clone();
        let paths = self.paths.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || -> anyhow::Result<(Report, Snapshot)> {
                let conn = db::open(&paths.db())?;
                let mut session = Session { paths, cfg, conn };
                let report = session.sync()?;
                Ok((report, session.snapshot()?))
            })
            .await;
            let Some(app) = weak.upgrade() else { return };
            app.syncing.set(false);
            match result {
                Ok(Ok((report, snap))) => app.apply(report, snap),
                Ok(Err(e)) => app.toast(&format!("Sync failed: {e:#}")),
                Err(_) => app.toast("Sync crashed"),
            }
            if app.sync_again.take() {
                app.refresh();
            }
        });
    }

    /// Debounced refresh for file-change bursts.
    fn schedule_refresh(self: &Rc<Self>) {
        if self.refresh_scheduled.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_secs(1), move || {
            if let Some(app) = weak.upgrade() {
                app.refresh_scheduled.set(false);
                app.refresh();
            }
        });
    }

    fn apply(self: &Rc<Self>, report: Report, snap: Snapshot) {
        for title in &report.completed {
            self.toast(&format!("Finished “{}”", widgets::isolate(title)));
        }
        if self.snapshot.borrow().as_deref() == Some(&snap) {
            return;
        }
        let snap = Rc::new(snap);
        self.today.update(&snap);
        self.courses.update(&snap);
        self.books.update(&snap);
        self.projects.update(&snap);
        self.habits.update(&snap);
        self.stats.update(&snap);
        self.holiday.set_state(&snap.holiday_today().to_variant());
        *self.snapshot.borrow_mut() = Some(snap.clone());
        self.check_stale_sources(&snap);
    }

    /// Checks downloaded courses for new episodes about once a day, in the background.
    fn check_stale_sources(self: &Rc<Self>, snap: &Snapshot) {
        let now = config::now();
        if now - self.last_source_check.get() < SOURCE_RETRY_AFTER {
            return;
        }
        let stale: Vec<PathBuf> = snap
            .courses
            .iter()
            .filter(|c| {
                c.source
                    .as_ref()
                    .is_some_and(|s| now - s.checked_at.unwrap_or(0) > SOURCE_CHECK_EVERY)
            })
            .map(|c| c.dir.clone())
            .collect();
        if !stale.is_empty() {
            self.check_sources(stale, false);
        }
    }

    /// Asks yt-dlp which episodes are new. `report` toasts the result even when nothing is new.
    fn check_sources(self: &Rc<Self>, dirs: Vec<PathBuf>, report: bool) {
        if self.checking_sources.replace(true) {
            return;
        }
        self.last_source_check.set(config::now());
        if report {
            self.toast("Checking for new episodes…");
        }
        let cfg = self.cfg.borrow().clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let results = gio::spawn_blocking(move || {
                dirs.into_iter()
                    .map(|dir| {
                        let name = dir
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        (name, download::check(&cfg, &dir))
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            let Some(app) = weak.upgrade() else { return };
            app.checking_sources.set(false);
            for (name, result) in results {
                let name = widgets::isolate(&name);
                match result {
                    Ok(new) if !new.is_empty() => {
                        app.toast(&format!("{} new episodes in {name}", new.len()))
                    }
                    Ok(_) if report => app.toast(&format!("{name} is up to date")),
                    Err(e) if report => app.toast(&format!("Couldn't check {name}: {e:#}")),
                    _ => {}
                }
            }
            app.refresh();
        });
    }

    /// Live updates: the mpv script writes its log every minute of playback, zathura writes on close.
    fn watch_sources(self: &Rc<Self>) {
        let cfg = self.cfg.borrow();
        let files = [
            self.paths.events_log.clone(),
            config::expand(&cfg.zathura_db),
        ];
        let mut monitors = self.monitors.borrow_mut();
        monitors.clear();
        for file in files {
            let Ok(monitor) = gio::File::for_path(&file)
                .monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
            else {
                continue;
            };
            let weak = Rc::downgrade(self);
            monitor.connect_changed(move |_, _, _, _| {
                if let Some(app) = weak.upgrade() {
                    app.schedule_refresh();
                }
            });
            monitors.push(monitor);
        }
    }

    fn set_config(self: &Rc<Self>, cfg: Config) {
        *self.cfg.borrow_mut() = cfg;
        self.watch_sources();
        self.update_banner();
        self.refresh();
    }

    fn update_banner(&self) {
        let script = config::config_home().join("mpv/scripts/patrak.lua");
        self.banner
            .set_revealed(!self.paths.config.exists() || !script.exists());
    }

    /// The same steps as `patrak setup`, without overwriting anything.
    fn run_setup(self: &Rc<Self>) {
        let paths = self.paths.clone();
        let cfg = self.cfg.borrow().clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || -> anyhow::Result<()> {
                setup::write_config(&paths, &cfg, false)?;
                setup::create_library_dirs(&cfg)?;
                setup::install_mpv_script(&paths, false)?;
                setup::install_desktop_entry(false)?;
                setup::install_reminder(&cfg)?;
                Ok(())
            })
            .await;
            let Some(app) = weak.upgrade() else { return };
            match result {
                Ok(Ok(())) => {
                    app.toast("Set up: mpv script, launcher and daily reminder installed")
                }
                Ok(Err(e)) => app.toast(&format!("Setup failed: {e:#}")),
                Err(_) => app.toast("Setup crashed"),
            }
            app.update_banner();
            app.refresh();
        });
    }

    fn show_about(&self) {
        adw::AboutDialog::builder()
            .application_name("patrak")
            .application_icon("accessories-dictionary-symbolic")
            .version(env!("CARGO_PKG_VERSION"))
            .comments("Daily learning tracker for video courses watched in mpv and books read in zathura.")
            .build()
            .present(Some(&self.window));
    }

    fn toast(&self, text: &str) {
        let toast = adw::Toast::builder()
            .title(text)
            .use_markup(false)
            .timeout(4)
            .build();
        self.toasts.add_toast(toast);
    }
}
