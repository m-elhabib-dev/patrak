use super::download_history::History;
use super::widgets::{dialog_with_header, isolate, label, page, placeholder_row};
use super::{Act, Action, App};
use adw::prelude::*;
use gtk::{gio, glib};
use patrak::config::Paths;
use patrak::download::{self, Plan, Progress};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

/// yt-dlp stops cleanly on SIGINT, keeping partial files to resume from.
const SIGINT: i32 = 2;

/// How many lines of yt-dlp output a download's log keeps.
const LOG_LINES: i32 = 1000;

/// Asks for a playlist URL, then downloads it as a new course.
pub fn show_add(app: &Rc<App>) {
    show_add_url(app, "");
}

/// Like [`show_add`], with the URL filled in.
fn show_add_url(app: &Rc<App>, link: &str) {
    let cfg = app.cfg.borrow().clone();
    let target = cfg
        .video_dirs
        .first()
        .cloned()
        .unwrap_or_else(|| "your first video folder".into());

    let url = adw::EntryRow::builder()
        .title("Playlist or video URL")
        .build();
    let name = adw::EntryRow::builder()
        .title("Course name (optional)")
        .build();
    let group = adw::PreferencesGroup::builder()
        .description(format!(
            "Saved to {target}. A playlist becomes a course, and patrak checks it daily for new episodes."
        ))
        .build();
    group.add(&url);
    group.add(&name);

    let start = gtk::Button::builder()
        .label("Download")
        .halign(gtk::Align::Center)
        .sensitive(false)
        .build();
    start.add_css_class("pill");
    start.add_css_class("suggested-action");

    let form = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    form.append(&group);
    form.append(&start);

    let job = Job::new(app);
    let stack = gtk::Stack::new();
    stack.add_named(&form, Some("form"));
    stack.add_named(&job.page, Some("progress"));
    open(app, "Add Course", &stack, &job);

    let s = start.clone();
    url.connect_changed(move |e| s.set_sensitive(!e.text().trim().is_empty()));
    url.set_text(link);
    let s = start.clone();
    url.connect_entry_activated(move |_| s.emit_clicked());

    let weak = Rc::downgrade(app);
    start.connect_clicked(move |_| {
        let Some(app) = weak.upgrade() else { return };
        let url = url.text().trim().to_owned();
        let name = Some(name.text().trim().to_owned()).filter(|n| !n.is_empty());
        stack.set_visible_child_name("progress");
        app.downloads.begin(&job);
        job.looking_up(&url);
        let cfg = app.cfg.borrow().clone();
        let (job, weak) = (job.clone(), Rc::downgrade(&app));
        glib::spawn_future_local(async move {
            let lookup = url.clone();
            let plan =
                gio::spawn_blocking(move || download::plan(&cfg, &lookup, name.as_deref())).await;
            let Some(app) = weak.upgrade() else { return };
            match plan {
                Ok(Ok(plan)) if plan.source.is_some() && app.downloads.busy(&plan.dir) => {
                    let failure = "Already downloading this course.".to_owned();
                    ended(&app, &job, &plan.title, Some(failure));
                }
                Ok(Ok(plan)) => run(&app, plan, job),
                Ok(Err(e)) => {
                    let failure = format!("{e:#}");
                    download::lookup_failed(&app.paths, &url, &failure);
                    ended(&app, &job, &url, Some(failure));
                }
                Err(_) => {
                    let failure = "Looking up the URL crashed.";
                    download::lookup_failed(&app.paths, &url, failure);
                    ended(&app, &job, &url, Some(failure.to_owned()));
                }
            }
        });
    });
}

/// Lists the downloads that were still unfinished when the app last closed.
pub fn restore(app: &Rc<App>) {
    for plan in download::pending(&app.paths) {
        let job = Job::new(app);
        job.restored(plan);
        app.downloads.jobs.borrow_mut().push(job.clone());
        app.downloads.adopt(&job);
    }
    app.downloads.reload_history(&app.paths);
}

/// Continues a stopped or failed download where it left off.
fn resume(app: &Rc<App>, job: &Rc<Job>) {
    let Some(plan) = job.plan.borrow().clone() else {
        return;
    };
    let Some(plan) = download::refreshed(plan) else {
        app.toast("The course folder has moved; use Download New there");
        return app.downloads.dismiss(app, job);
    };
    job.resuming();
    app.downloads.begin(job);
    run(app, plan, job.clone());
}

/// Resumes everything on the Downloads page that was stopped or failed.
pub fn resume_all(app: &Rc<App>) {
    for job in app.downloads.idle() {
        resume(app, &job);
    }
}

/// Fetches a past download again: straight away for a course that's still there, otherwise by
/// looking its URL up again.
pub fn download_again(app: &Rc<App>, url: &str, course: Option<PathBuf>) {
    match course.filter(|dir| dir.join(download::SOURCE_FILE).is_file()) {
        Some(dir) => show_update(app, dir),
        None => show_add_url(app, url),
    }
}

pub fn remove_from_history(app: &Rc<App>, ids: &[i64]) {
    if let Err(e) = download::remove_from_history(&app.paths, ids) {
        app.toast(&format!("Couldn't change the history: {e:#}"));
    }
    app.downloads.reload_history(&app.paths);
}

pub fn confirm_clear_history(app: &Rc<App>) {
    let alert = adw::AlertDialog::new(
        Some("Clear Download History?"),
        Some("The downloaded files stay, and unfinished downloads can still be resumed."),
    );
    alert.add_responses(&[("cancel", "Cancel"), ("clear", "Clear")]);
    alert.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
    alert.set_default_response(Some("cancel"));
    alert.set_close_response("cancel");
    let weak = Rc::downgrade(app);
    alert.connect_response(Some("clear"), move |_, _| {
        if let Some(app) = weak.upgrade() {
            remove_from_history(&app, &app.downloads.history.ids());
        }
    });
    alert.present(Some(&app.window));
}

/// Downloads the new episodes of a course added earlier.
pub fn show_update(app: &Rc<App>, dir: PathBuf) {
    let Some(plan) = download::update_plan(&dir) else {
        return app.toast("This course wasn't downloaded with patrak");
    };
    // Its dialog was minimized, or the button behind it couldn't be pressed.
    if app.downloads.busy(&plan.dir) {
        app.toast(&format!("Already downloading {}", isolate(&plan.title)));
        return app.show_downloads();
    }
    let job = Job::new(app);
    open(
        app,
        &format!("Updating {}", isolate(&plan.title)),
        &job.page,
        &job,
    );
    app.downloads.begin(&job);
    run(app, plan, job);
}

/// Presents a dialog that never closes by itself. Minimizing or closing it while `job` is
/// working moves the job to the Downloads page, where it carries on.
fn open(app: &Rc<App>, title: &str, child: &impl IsA<gtk::Widget>, job: &Rc<Job>) {
    let (dialog, header) = dialog_with_header(title, child);
    let minimize = gtk::Button::builder()
        .icon_name("window-minimize-symbolic")
        .tooltip_text("Continue in the background")
        .visible(job.working.get())
        .build();
    header.pack_end(&minimize);
    job.minimize.set(Some(&minimize));
    let d = dialog.downgrade();
    minimize.connect_clicked(move |_| {
        if let Some(d) = d.upgrade() {
            d.close();
        }
    });
    let (weak, job) = (Rc::downgrade(app), job.clone());
    dialog.connect_closed(move |_| {
        if let Some(app) = weak.upgrade()
            && (job.working.get() || job.can_resume())
        {
            app.downloads.adopt(&job);
        }
    });
    dialog.present(Some(&app.window));
}

/// Runs yt-dlp, showing its progress in `job`.
fn run(app: &Rc<App>, plan: Plan, job: Rc<Job>) {
    if plan.source.is_some() {
        *job.dir.borrow_mut() = Some(plan.dir.clone());
    }
    *job.plan.borrow_mut() = Some(plan.clone());
    app.downloads.replace_idle(&job);
    if let Err(e) = download::prepare(&app.paths, &plan) {
        return ended(app, &job, &plan.title, Some(format!("{e:#}")));
    }
    app.downloads.reload_history(&app.paths);
    let args = download::download_args(&app.cfg.borrow(), &plan, true);
    let argv: Vec<&OsStr> = args.iter().map(|a| a.as_os_str()).collect();
    let flags = gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE;
    let process = match gio::Subprocess::newv(&argv, flags) {
        Ok(p) => p,
        Err(e) => {
            let failure = format!("Couldn't start yt-dlp: {}", e.message());
            return ended(app, &job, &plan.title, Some(failure));
        }
    };
    let Some(stdout) = process.stdout_pipe() else {
        return ended(app, &job, &plan.title, Some("No output from yt-dlp".into()));
    };
    job.started(&plan, &process);
    let reader = gio::DataInputStream::new(&stdout);
    let weak = Rc::downgrade(app);
    let paths = app.paths.clone();
    glib::spawn_future_local(async move {
        let mut tail = VecDeque::new();
        while let Ok(Some(line)) = reader.read_line_utf8_future(glib::Priority::DEFAULT).await {
            if let Some(progress) = download::parse_progress(&line) {
                job.progress(&progress);
                continue;
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            job.message(line);
            tail.push_back(line.to_owned());
            if tail.len() > 6 {
                tail.pop_front();
            }
        }
        let ok = process.wait_future().await.is_ok() && process.is_successful();
        if ok {
            // Stop came too late to matter.
            job.stopped.set(false);
            if let Err(e) = download::finish(&paths, &plan) {
                eprintln!("updating {}: {e:#}", plan.dir.display());
            }
        }
        if let Some(app) = weak.upgrade() {
            let failure = (!ok).then(|| Vec::from(tail).join("\n"));
            ended(&app, &job, &plan.title, failure);
        }
    });
}

/// Shows how a download ended, in its view and as a toast, and records it.
fn ended(app: &Rc<App>, job: &Rc<Job>, title: &str, failure: Option<String>) {
    let title = isolate(title);
    let plan = job.plan.borrow().clone();
    let toast = if job.stopped.get() {
        if let Some(plan) = &plan {
            download::stopped(&app.paths, plan);
        }
        job.show_stopped(&title);
        format!("Stopped downloading {title}")
    } else if let Some(details) = failure {
        if let Some(plan) = &plan {
            download::failed(&app.paths, plan, &details);
        }
        job.failed(&title, &details);
        format!("Download of {title} failed")
    } else {
        job.done(&format!("Downloaded {title}"));
        format!("Downloaded {title}")
    };
    app.downloads.end(job);
    app.downloads.reload_history(&app.paths);
    app.toast(&toast);
    app.refresh();
}

/// The Downloads page: what's in progress or can be resumed, and the history. The header button
/// jumps to it and shows when something is going on.
pub struct Downloads {
    pub root: gtk::ScrolledWindow,
    pub button: gtk::Button,
    /// Downloads moved out of their dialogs.
    list: gtk::Box,
    /// Shown instead of `list` when it's empty.
    empty: gtk::ListBox,
    stop_all: gtk::Button,
    resume_all: gtk::Button,
    history: History,
    spinner: adw::Spinner,
    /// Every working download, plus stopped and failed ones that can be resumed.
    jobs: RefCell<Vec<Rc<Job>>>,
}

impl Downloads {
    pub fn new(act: Act) -> Rc<Self> {
        let (root, content) = page(860);

        let flat_button = |text: &str, tooltip: &str, action: fn() -> Action| {
            let b = gtk::Button::builder()
                .label(text)
                .tooltip_text(tooltip)
                .visible(false)
                .build();
            b.add_css_class("flat");
            let act = act.clone();
            b.connect_clicked(move |_| act(action()));
            b
        };
        let stop_all = flat_button("Stop All", "Stop every running download", || {
            Action::StopAllDownloads
        });
        let resume_all = flat_button("Resume All", "Resume every stopped download", || {
            Action::ResumeAllDownloads
        });
        let add = gtk::Button::builder()
            .label("Add…")
            .tooltip_text("Add a course from YouTube (Ctrl+N)")
            .action_name("app.add-course")
            .build();
        add.add_css_class("flat");
        let heading = label("In Progress", &["heading"]);
        heading.set_hexpand(true);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        for w in [
            heading.upcast_ref::<gtk::Widget>(),
            resume_all.upcast_ref(),
            stop_all.upcast_ref(),
            add.upcast_ref(),
        ] {
            header.append(w);
        }

        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .visible(false)
            .build();
        list.add_css_class("card");
        let empty = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        empty.add_css_class("boxed-list");
        empty.append(&placeholder_row("Nothing is downloading."));
        let progress = gtk::Box::new(gtk::Orientation::Vertical, 8);
        progress.append(&header);
        progress.append(&empty);
        progress.append(&list);
        content.append(&progress);

        let history = History::new(act);
        content.append(&history.root);

        let button = gtk::Button::builder()
            .icon_name("folder-download-symbolic")
            .tooltip_text("Downloads")
            .action_name("app.show-downloads")
            .visible(false)
            .build();
        Rc::new(Self {
            root,
            button,
            list,
            empty,
            stop_all,
            resume_all,
            history,
            spinner: adw::Spinner::new(),
            jobs: RefCell::new(Vec::new()),
        })
    }

    /// Shows the current history, unless it failed to load.
    pub fn reload_history(&self, paths: &Paths) {
        match download::history(paths) {
            Ok(records) => self.history.update(records),
            Err(e) => eprintln!("reading the download history: {e:#}"),
        }
    }

    fn begin(&self, job: &Rc<Job>) {
        job.working.set(true);
        job.show_minimize(true);
        let mut jobs = self.jobs.borrow_mut();
        if !jobs.iter().any(|j| Rc::ptr_eq(j, job)) {
            jobs.push(job.clone());
        }
        drop(jobs);
        self.update_button();
    }

    /// Moves a job from its closing dialog to the top of the list.
    fn adopt(&self, job: &Rc<Job>) {
        if let Some(page) = job.root.parent().and_downcast::<gtk::Box>() {
            page.remove(&job.root);
        }
        job.heading.remove_css_class("title-4");
        job.heading.add_css_class("heading");
        if self.list.first_child().is_some() {
            self.list
                .prepend(&gtk::Separator::new(gtk::Orientation::Horizontal));
        }
        self.list.prepend(&job.root);
        job.adopted.set(true);
        job.dismiss.set_visible(!job.working.get());
        self.update_button();
    }

    /// Removes a job and its neighbouring separator from the list.
    fn detach(&self, job: &Rc<Job>) {
        if let Some(separator) = job.root.next_sibling().or_else(|| job.root.prev_sibling()) {
            self.list.remove(&separator);
        }
        self.list.remove(&job.root);
    }

    fn end(&self, job: &Rc<Job>) {
        job.working.set(false);
        job.process.take();
        job.stop.set_visible(false);
        job.show_minimize(false);
        job.resume.set_visible(job.can_resume());
        if job.can_resume() {
            job.dismiss.set_visible(job.adopted.get());
        } else {
            // Over: only the history is left, unless its dialog is still showing it.
            if job.adopted.get() {
                self.detach(job);
            }
            self.jobs.borrow_mut().retain(|j| !Rc::ptr_eq(j, job));
        }
        self.update_button();
    }

    fn dismiss(&self, app: &App, job: &Rc<Job>) {
        if let Some(plan) = job.plan.take()
            && let Err(e) = download::forget(&app.paths, &plan)
        {
            eprintln!("forgetting {}: {e:#}", plan.url);
        }
        self.detach(job);
        self.jobs.borrow_mut().retain(|j| !Rc::ptr_eq(j, job));
        self.update_button();
        self.reload_history(&app.paths);
    }

    /// Drops a stopped entry for what `job` is now downloading: it would only offer to resume
    /// the same thing twice.
    fn replace_idle(&self, job: &Rc<Job>) {
        let same = |j: &Rc<Job>| !Rc::ptr_eq(j, job) && !j.working.get() && j.plan_matches(job);
        let stale: Vec<Rc<Job>> = self
            .jobs
            .borrow()
            .iter()
            .filter(|j| same(j))
            .cloned()
            .collect();
        for old in stale {
            if old.adopted.get() {
                self.detach(&old);
            }
            self.jobs.borrow_mut().retain(|j| !Rc::ptr_eq(j, &old));
        }
        self.update_button();
    }

    /// Whether a course folder is being downloaded into.
    pub fn busy(&self, dir: &Path) -> bool {
        self.jobs
            .borrow()
            .iter()
            .any(|j| j.working.get() && j.dir.borrow().as_deref() == Some(dir))
    }

    /// How many yt-dlp processes are running.
    pub fn running(&self) -> usize {
        let jobs = self.jobs.borrow();
        jobs.iter().filter(|j| j.process.borrow().is_some()).count()
    }

    /// Listed downloads that can be resumed.
    fn idle(&self) -> Vec<Rc<Job>> {
        let jobs = self.jobs.borrow();
        jobs.iter()
            .filter(|j| j.adopted.get() && !j.working.get() && j.can_resume())
            .cloned()
            .collect()
    }

    pub fn stop_all(&self) {
        for job in self.jobs.borrow().iter() {
            job.stop();
        }
    }

    /// Fits the page and the header button to what's listed. The button shows while the list has
    /// something in it, spinning while any of it is working.
    fn update_button(&self) {
        let any_idle = !self.idle().is_empty();
        let jobs = self.jobs.borrow();
        let listed = jobs.iter().filter(|j| j.adopted.get());
        let working = listed.clone().any(|j| j.working.get());
        let any = listed.count() > 0;
        self.list.set_visible(any);
        self.empty.set_visible(!any);
        self.stop_all.set_visible(working);
        self.resume_all.set_visible(any_idle);
        self.button.set_visible(any);
        if !working {
            self.button.set_icon_name("folder-download-symbolic");
        } else if self.spinner.parent().is_none() {
            self.button.set_child(Some(&self.spinner));
        }
    }
}

/// One download's progress, shown in its dialog and later on the Downloads page.
struct Job {
    /// What the dialog shows; `root` moves out of it to the Downloads page.
    page: gtk::Box,
    root: gtk::Box,
    heading: gtk::Label,
    bar: gtk::ProgressBar,
    status: gtk::Label,
    /// The video being fetched: its title, bar and size. Shown once progress arrives.
    file: gtk::Box,
    file_title: gtk::Label,
    file_bar: gtk::ProgressBar,
    file_size: gtk::Label,
    detail: gtk::Label,
    stop: gtk::Button,
    /// Continues a stopped or failed download.
    resume: gtk::Button,
    /// Gives up on a stopped or failed download, moving it to the history.
    dismiss: gtk::Button,
    /// What yt-dlp printed besides progress, shown on request.
    log: gtk::TextBuffer,
    log_view: gtk::TextView,
    /// The minimize button in the dialog's header bar. Only weakly held: a button outliving
    /// its `adw::HeaderBar` crashes when shown or hidden.
    minimize: glib::WeakRef<gtk::Button>,
    /// From the start until the lookup fails or yt-dlp exits.
    working: Cell<bool>,
    /// Moved from its dialog to the Downloads page.
    adopted: Cell<bool>,
    stopped: Cell<bool>,
    /// The course folder, so it isn't downloaded into twice at once.
    dir: RefCell<Option<PathBuf>>,
    /// What's being downloaded, until it finishes. Kept after a stop or failure to resume it.
    plan: RefCell<Option<Plan>>,
    process: RefCell<Option<gio::Subprocess>>,
}

impl Job {
    fn new(app: &Rc<App>) -> Rc<Self> {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        let heading = label("", &["title-4"]);
        heading.set_hexpand(true);
        let dismiss = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Don’t resume this download; its files stay")
            .valign(gtk::Align::Start)
            .visible(false)
            .build();
        dismiss.add_css_class("flat");
        dismiss.add_css_class("circular");
        let log_toggle = gtk::ToggleButton::builder()
            .icon_name("utilities-terminal-symbolic")
            .tooltip_text("Show log")
            .valign(gtk::Align::Start)
            .build();
        log_toggle.add_css_class("flat");
        log_toggle.add_css_class("circular");
        log_toggle.connect_toggled(|t| {
            t.set_tooltip_text(Some(if t.is_active() {
                "Hide log"
            } else {
                "Show log"
            }));
        });
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        top.append(&heading);
        top.append(&log_toggle);
        top.append(&dismiss);

        let bar = gtk::ProgressBar::builder()
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let stop = gtk::Button::builder()
            .label("Stop")
            .tooltip_text("Stop downloading; what’s downloaded is kept")
            .visible(false)
            .build();
        let resume_button = gtk::Button::builder()
            .label("Resume")
            .tooltip_text("Continue from what’s already downloaded")
            .visible(false)
            .build();
        resume_button.add_css_class("suggested-action");
        let middle = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        middle.append(&bar);
        middle.append(&stop);
        middle.append(&resume_button);

        let status = label("", &[]);
        let file_title = label("", &["caption"]);
        file_title.set_wrap(false);
        file_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let file_bar = gtk::ProgressBar::new();
        let file_size = label("", &["caption", "dim-label"]);
        let file = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .visible(false)
            .build();
        for w in [
            file_title.upcast_ref::<gtk::Widget>(),
            file_bar.upcast_ref(),
            file_size.upcast_ref(),
        ] {
            file.append(w);
        }
        let detail = label("", &["caption", "dim-label"]);
        detail.set_selectable(true);
        detail.set_lines(6);
        detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let log = gtk::TextBuffer::new(None);
        let log_view = gtk::TextView::builder()
            .buffer(&log)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(8)
            .right_margin(8)
            .top_margin(6)
            .bottom_margin(6)
            .build();
        let log_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(160)
            .child(&log_view)
            .visible(false)
            .build();
        log_scroller.add_css_class("card");
        log_toggle
            .bind_property("active", &log_scroller, "visible")
            .sync_create()
            .build();
        for w in [
            top.upcast_ref::<gtk::Widget>(),
            middle.upcast_ref(),
            status.upcast_ref(),
            file.upcast_ref(),
            detail.upcast_ref(),
            log_scroller.upcast_ref(),
        ] {
            root.append(w);
        }
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.append(&root);

        Rc::new_cyclic(|weak_job: &Weak<Job>| {
            let weak = weak_job.clone();
            stop.connect_clicked(move |_| {
                if let Some(job) = weak.upgrade() {
                    job.stop();
                }
            });
            let (weak, app) = (weak_job.clone(), Rc::downgrade(app));
            let (dismiss_job, dismiss_app) = (weak.clone(), app.clone());
            resume_button.connect_clicked(move |_| {
                if let (Some(job), Some(app)) = (weak.upgrade(), app.upgrade()) {
                    resume(&app, &job);
                }
            });
            dismiss.connect_clicked(move |_| {
                if let (Some(job), Some(app)) = (dismiss_job.upgrade(), dismiss_app.upgrade()) {
                    app.downloads.dismiss(&app, &job);
                }
            });
            Self {
                page,
                root,
                heading,
                bar,
                status,
                file,
                file_title,
                file_bar,
                file_size,
                detail,
                stop,
                resume: resume_button,
                dismiss,
                log,
                log_view,
                minimize: glib::WeakRef::new(),
                working: Cell::new(false),
                adopted: Cell::new(false),
                stopped: Cell::new(false),
                dir: RefCell::new(None),
                plan: RefCell::new(None),
                process: RefCell::new(None),
            }
        })
    }

    fn can_resume(&self) -> bool {
        self.plan.borrow().is_some()
    }

    /// Whether `other` is downloading the same thing as this job.
    fn plan_matches(&self, other: &Job) -> bool {
        match (&*self.plan.borrow(), &*other.plan.borrow()) {
            (Some(a), Some(b)) => a.same_download(b),
            _ => false,
        }
    }

    fn stop(&self) {
        if let Some(process) = self.process.borrow().as_ref() {
            self.stopped.set(true);
            process.send_signal(SIGINT);
            self.stop.set_sensitive(false);
            self.heading.set_label("Stopping…");
        }
    }

    fn show_minimize(&self, show: bool) {
        if let Some(button) = self.minimize.upgrade() {
            button.set_visible(show);
        }
    }

    fn looking_up(&self, url: &str) {
        self.heading.set_label("Looking up…");
        self.status.set_label(url);
        self.bar.pulse();
    }

    fn started(&self, plan: &Plan, process: &gio::Subprocess) {
        *self.process.borrow_mut() = Some(process.clone());
        self.stop.set_visible(true);
        self.stop.set_sensitive(true);
        self.heading
            .set_label(&format!("Downloading {}", isolate(&plan.title)));
        self.status.set_label(&plan.dir.display().to_string());
        self.bar.pulse();
    }

    fn progress(&self, p: &Progress) {
        let overall = p.overall();
        match overall {
            Some(f) => self.bar.set_fraction(f),
            None => self.bar.pulse(),
        }
        let mut total = String::from("Total");
        if let (Some(i), Some(n)) = (p.index, p.count) {
            total.push_str(&format!("  ·  {i} of {n}"));
        }
        if let Some(f) = overall {
            total.push_str(&format!("  ·  {:.0}%", f * 100.0));
        }
        self.status.set_label(&total);

        self.file.set_visible(true);
        self.file_title.set_label(&isolate(&p.title));
        match p.fraction {
            Some(f) => self.file_bar.set_fraction(f),
            None => self.file_bar.pulse(),
        }
        let size = |b: f64| glib::format_size(b as u64);
        self.file_size.set_label(&match (p.downloaded, p.total) {
            (Some(d), Some(t)) => format!("{} of {}", size(d), size(t)),
            (Some(d), None) => size(d).to_string(),
            _ => String::new(),
        });
    }

    fn message(&self, line: &str) {
        self.detail.set_label(line);
        self.log_line(line);
        if self.bar.fraction() == 0.0 {
            self.bar.pulse();
        }
    }

    /// Appends to the log, keeping the last `LOG_LINES` and following the newest.
    fn log_line(&self, line: &str) {
        self.log
            .insert(&mut self.log.end_iter(), &format!("{line}\n"));
        let extra = self.log.line_count() - LOG_LINES;
        if extra > 0
            && let Some(mut cut) = self.log.iter_at_line(extra)
        {
            self.log.delete(&mut self.log.start_iter(), &mut cut);
        }
        let mark = match self.log.mark("end") {
            Some(mark) => mark,
            None => self
                .log
                .create_mark(Some("end"), &self.log.end_iter(), false),
        };
        self.log.move_mark(&mark, &self.log.end_iter());
        self.log_view.scroll_mark_onscreen(&mark);
    }

    fn done(&self, text: &str) {
        self.plan.take();
        self.heading.set_label(text);
        self.bar.set_fraction(1.0);
        self.file.set_visible(false);
        self.status.set_visible(false);
        self.detail.set_label("New lectures appear in Courses.");
    }

    fn failed(&self, title: &str, details: &str) {
        self.heading
            .set_label(&format!("Download of {title} failed"));
        self.detail.set_label(details);
        self.detail.remove_css_class("dim-label");
        self.detail.add_css_class("error");
    }

    fn show_stopped(&self, title: &str) {
        self.heading
            .set_label(&format!("Download of {title} stopped"));
        let plan = self.plan.borrow();
        let url = plan.as_ref().map_or("", |p| p.url.as_str());
        self.detail.set_label(&format!(
            "What’s downloaded is kept; Resume continues from there.\n{url}"
        ));
    }

    /// A download left unfinished by an earlier run of the app.
    fn restored(&self, plan: Plan) {
        self.status.set_label(&plan.dir.display().to_string());
        if plan.source.is_some() {
            *self.dir.borrow_mut() = Some(plan.dir.clone());
        }
        let title = isolate(&plan.title);
        *self.plan.borrow_mut() = Some(plan);
        self.show_stopped(&title);
        self.resume.set_visible(true);
    }

    /// Back to how a new download looks.
    fn resuming(&self) {
        self.stopped.set(false);
        self.resume.set_visible(false);
        self.dismiss.set_visible(false);
        self.heading.set_label("Resuming…");
        self.detail.set_label("");
        self.detail.remove_css_class("error");
        self.detail.add_css_class("dim-label");
        self.status.set_visible(true);
    }
}
