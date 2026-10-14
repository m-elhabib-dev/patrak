use super::App;
use super::widgets::{dialog_with_header, isolate, label};
use adw::prelude::*;
use gtk::{gio, glib};
use patrak::download::{self, Plan, Progress};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

/// yt-dlp stops cleanly on SIGINT, keeping partial files to resume from.
const SIGINT: i32 = 2;

/// Asks for a playlist URL, then downloads it as a new course.
pub fn show_add(app: &Rc<App>) {
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

    let job = Job::new();
    let stack = gtk::Stack::new();
    stack.add_named(&form, Some("form"));
    stack.add_named(&job.page, Some("progress"));
    open(app, "Add Course", &stack, &job);

    let s = start.clone();
    url.connect_changed(move |e| s.set_sensitive(!e.text().trim().is_empty()));
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
                Ok(Err(e)) => ended(&app, &job, &url, Some(format!("{e:#}"))),
                Err(_) => ended(&app, &job, &url, Some("Looking up the URL crashed.".into())),
            }
        });
    });
}

/// Downloads the new episodes of a course added earlier.
pub fn show_update(app: &Rc<App>, dir: PathBuf) {
    let Some(plan) = download::update_plan(&dir) else {
        return app.toast("This course wasn't downloaded with patrak");
    };
    // Its dialog was minimized, or the button behind it couldn't be pressed.
    if app.downloads.busy(&plan.dir) {
        app.toast(&format!("Already downloading {}", isolate(&plan.title)));
        return app.downloads.button.popup();
    }
    let job = Job::new();
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
/// working moves the job into the header's downloads list, where it carries on.
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
            && job.working.get()
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
    if let Err(e) = download::prepare(&plan) {
        return ended(app, &job, &plan.title, Some(format!("{e:#}")));
    }
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
        if ok && let Err(e) = download::finish(&plan) {
            eprintln!("updating {}: {e:#}", plan.dir.display());
        }
        if let Some(app) = weak.upgrade() {
            let failure = (!ok).then(|| Vec::from(tail).join("\n"));
            ended(&app, &job, &plan.title, failure);
        }
    });
}

/// Shows how a download ended, in its view and as a toast.
fn ended(app: &Rc<App>, job: &Rc<Job>, title: &str, failure: Option<String>) {
    let title = isolate(title);
    let toast = if job.stopped.get() {
        job.show_stopped();
        format!("Stopped downloading {title}")
    } else if let Some(details) = failure {
        job.failed(&details);
        format!("Download of {title} failed")
    } else {
        job.done(&format!("Downloaded {title}"));
        format!("Downloaded {title}")
    };
    app.downloads.end(job);
    app.toast(&toast);
    app.refresh();
}

/// Downloads moved out of their dialogs, listed in a header bar popover.
pub struct Downloads {
    pub button: gtk::MenuButton,
    list: gtk::Box,
    spinner: adw::Spinner,
    /// Every working download, plus finished ones still in the list.
    jobs: RefCell<Vec<Rc<Job>>>,
}

impl Downloads {
    pub fn new() -> Rc<Self> {
        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(480)
            .width_request(360)
            .child(&list)
            .build();
        let popover = gtk::Popover::builder().child(&scroller).build();
        let button = gtk::MenuButton::builder()
            .icon_name("folder-download-symbolic")
            .tooltip_text("Downloads")
            .popover(&popover)
            .visible(false)
            .build();
        Rc::new(Self {
            button,
            list,
            spinner: adw::Spinner::new(),
            jobs: RefCell::new(Vec::new()),
        })
    }

    fn begin(&self, job: &Rc<Job>) {
        job.working.set(true);
        job.show_minimize(true);
        self.jobs.borrow_mut().push(job.clone());
        self.update_button();
    }

    /// Moves a job from its closing dialog to the top of the list.
    fn adopt(self: &Rc<Self>, job: &Rc<Job>) {
        job.page.remove(&job.root);
        for set in [
            gtk::Widget::set_margin_top,
            gtk::Widget::set_margin_bottom,
            gtk::Widget::set_margin_start,
            gtk::Widget::set_margin_end,
        ] {
            set(job.root.upcast_ref(), 6);
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

        let (weak, job_weak) = (Rc::downgrade(self), Rc::downgrade(job));
        job.dismiss.connect_clicked(move |_| {
            if let (Some(downloads), Some(job)) = (weak.upgrade(), job_weak.upgrade()) {
                downloads.dismiss(&job);
            }
        });
        self.update_button();
    }

    fn end(&self, job: &Rc<Job>) {
        job.working.set(false);
        job.process.take();
        job.stop.set_visible(false);
        job.show_minimize(false);
        if job.adopted.get() {
            job.dismiss.set_visible(true);
        } else {
            self.jobs.borrow_mut().retain(|j| !Rc::ptr_eq(j, job));
        }
        self.update_button();
    }

    fn dismiss(&self, job: &Rc<Job>) {
        if let Some(separator) = job.root.next_sibling().or_else(|| job.root.prev_sibling()) {
            self.list.remove(&separator);
        }
        self.list.remove(&job.root);
        self.jobs.borrow_mut().retain(|j| !Rc::ptr_eq(j, job));
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

    pub fn stop_all(&self) {
        for job in self.jobs.borrow().iter() {
            job.stop();
        }
    }

    /// Shown while the list has something in it, spinning while any of it is working.
    fn update_button(&self) {
        let jobs = self.jobs.borrow();
        let listed = jobs.iter().filter(|j| j.adopted.get());
        let working = listed.clone().any(|j| j.working.get());
        if listed.count() == 0 {
            self.button.popdown();
            self.button.set_visible(false);
            return;
        }
        self.button.set_visible(true);
        if !working {
            self.button.set_icon_name("folder-download-symbolic");
        } else if self.spinner.parent().is_none() {
            self.button.set_child(Some(&self.spinner));
        }
    }
}

/// One download's progress, shown in its dialog and later in the downloads list.
struct Job {
    /// What the dialog shows; `root` moves out of it into the downloads list.
    page: gtk::Box,
    root: gtk::Box,
    heading: gtk::Label,
    bar: gtk::ProgressBar,
    status: gtk::Label,
    detail: gtk::Label,
    stop: gtk::Button,
    /// Removes a finished download from the list.
    dismiss: gtk::Button,
    /// The minimize button in the dialog's header bar. Only weakly held: a button outliving
    /// its `adw::HeaderBar` crashes when shown or hidden.
    minimize: glib::WeakRef<gtk::Button>,
    /// From the start until the lookup fails or yt-dlp exits.
    working: Cell<bool>,
    /// Moved from its dialog into the downloads list.
    adopted: Cell<bool>,
    stopped: Cell<bool>,
    /// The course folder, so it isn't downloaded into twice at once.
    dir: RefCell<Option<PathBuf>>,
    process: RefCell<Option<gio::Subprocess>>,
}

impl Job {
    fn new() -> Rc<Self> {
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
            .tooltip_text("Remove from the list")
            .valign(gtk::Align::Start)
            .visible(false)
            .build();
        dismiss.add_css_class("flat");
        dismiss.add_css_class("circular");
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        top.append(&heading);
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
        let middle = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        middle.append(&bar);
        middle.append(&stop);

        let status = label("", &[]);
        let detail = label("", &["caption", "dim-label"]);
        detail.set_selectable(true);
        detail.set_lines(6);
        detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
        for w in [
            top.upcast_ref::<gtk::Widget>(),
            middle.upcast_ref(),
            status.upcast_ref(),
            detail.upcast_ref(),
        ] {
            root.append(w);
        }
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.append(&root);

        Rc::new_cyclic(|weak: &Weak<Job>| {
            let weak = weak.clone();
            stop.connect_clicked(move |_| {
                if let Some(job) = weak.upgrade() {
                    job.stop();
                }
            });
            Self {
                page,
                root,
                heading,
                bar,
                status,
                detail,
                stop,
                dismiss,
                minimize: glib::WeakRef::new(),
                working: Cell::new(false),
                adopted: Cell::new(false),
                stopped: Cell::new(false),
                dir: RefCell::new(None),
                process: RefCell::new(None),
            }
        })
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
        self.heading
            .set_label(&format!("Downloading {}", isolate(&plan.title)));
        self.status.set_label(&plan.dir.display().to_string());
        self.bar.pulse();
    }

    fn progress(&self, p: &Progress) {
        match p.overall() {
            Some(f) => self.bar.set_fraction(f),
            None => self.bar.pulse(),
        }
        let position = match (p.index, p.count) {
            (Some(i), Some(n)) => format!("{i} of {n}  ·  "),
            _ => String::new(),
        };
        self.status
            .set_label(&format!("{position}{}", isolate(&p.title)));
    }

    fn message(&self, line: &str) {
        self.detail.set_label(line);
        if self.bar.fraction() == 0.0 {
            self.bar.pulse();
        }
    }

    fn done(&self, text: &str) {
        self.heading.set_label(text);
        self.bar.set_fraction(1.0);
        self.detail.set_label("New lectures appear in Courses.");
    }

    fn failed(&self, details: &str) {
        self.heading.set_label("Download failed");
        self.detail.set_label(details);
        self.detail.remove_css_class("dim-label");
        self.detail.add_css_class("error");
    }

    fn show_stopped(&self) {
        self.heading.set_label("Download stopped");
        self.detail.set_label(if self.dir.borrow().is_some() {
            "What’s downloaded is kept; Download New resumes it."
        } else {
            "What’s downloaded is kept; adding the video again resumes it."
        });
    }
}
