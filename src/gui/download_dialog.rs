use super::App;
use super::widgets::{isolate, label};
use adw::prelude::*;
use gtk::{gio, glib};
use patrak::download::{self, Plan, Progress};
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::rc::Rc;

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

    let view = ProgressView::new();
    let stack = gtk::Stack::new();
    stack.add_named(&form, Some("form"));
    stack.add_named(&view.root, Some("progress"));
    let dialog = dialog("Add Course", &stack);

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
        view.looking_up(&url);
        let cfg = app.cfg.borrow().clone();
        let (view, weak) = (view.clone(), Rc::downgrade(&app));
        glib::spawn_future_local(async move {
            let plan =
                gio::spawn_blocking(move || download::plan(&cfg, &url, name.as_deref())).await;
            let Some(app) = weak.upgrade() else { return };
            match plan {
                Ok(Ok(plan)) => run(&app, plan, view),
                Ok(Err(e)) => view.failed(&format!("{e:#}")),
                Err(_) => view.failed("Looking up the URL crashed."),
            }
        });
    });
    dialog.present(Some(&app.window));
}

/// Downloads the new episodes of a course added earlier.
pub fn show_update(app: &Rc<App>, dir: PathBuf) {
    let Some(plan) = download::update_plan(&dir) else {
        return app.toast("This course wasn't downloaded with patrak");
    };
    let view = ProgressView::new();
    dialog(&format!("Updating {}", isolate(&plan.title)), &view.root).present(Some(&app.window));
    run(app, plan, view);
}

fn dialog(title: &str, child: &impl IsA<gtk::Widget>) -> adw::Dialog {
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(child));
    adw::Dialog::builder()
        .title(title)
        .content_width(560)
        .child(&toolbar)
        .build()
}

/// Runs yt-dlp, showing its progress. Closing the dialog doesn't stop the download.
fn run(app: &Rc<App>, plan: Plan, view: Rc<ProgressView>) {
    if let Err(e) = download::prepare(&plan) {
        return view.failed(&format!("{e:#}"));
    }
    let args = download::download_args(&app.cfg.borrow(), &plan, true);
    let argv: Vec<&OsStr> = args.iter().map(|a| a.as_os_str()).collect();
    let flags = gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE;
    let process = match gio::Subprocess::newv(&argv, flags) {
        Ok(p) => p,
        Err(e) => return view.failed(&format!("Couldn't start yt-dlp: {}", e.message())),
    };
    view.started(&plan);
    let Some(stdout) = process.stdout_pipe() else {
        return view.failed("No output from yt-dlp");
    };
    let reader = gio::DataInputStream::new(&stdout);
    let weak = Rc::downgrade(app);
    glib::spawn_future_local(async move {
        let mut tail = VecDeque::new();
        while let Ok(Some(line)) = reader.read_line_utf8_future(glib::Priority::DEFAULT).await {
            if let Some(progress) = download::parse_progress(&line) {
                view.progress(&progress);
                continue;
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            view.message(line);
            tail.push_back(line.to_owned());
            if tail.len() > 6 {
                tail.pop_front();
            }
        }
        let ok = process.wait_future().await.is_ok() && process.is_successful();
        let title = isolate(&plan.title);
        if ok {
            if let Err(e) = download::finish(&plan) {
                eprintln!("updating {}: {e:#}", plan.dir.display());
            }
            view.done(&format!("Downloaded {title}"));
        } else {
            view.failed(&Vec::from(tail).join("\n"));
        }
        if let Some(app) = weak.upgrade() {
            app.toast(&if ok {
                format!("Downloaded {title}")
            } else {
                format!("Download of {title} failed")
            });
            app.refresh();
        }
    });
}

struct ProgressView {
    root: gtk::Box,
    heading: gtk::Label,
    bar: gtk::ProgressBar,
    status: gtk::Label,
    detail: gtk::Label,
}

impl ProgressView {
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
        let bar = gtk::ProgressBar::new();
        let status = label("", &[]);
        let detail = label("", &["caption", "dim-label"]);
        detail.set_selectable(true);
        detail.set_lines(6);
        detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
        for w in [
            heading.upcast_ref::<gtk::Widget>(),
            bar.upcast_ref(),
            status.upcast_ref(),
            detail.upcast_ref(),
        ] {
            root.append(w);
        }
        Rc::new(Self {
            root,
            heading,
            bar,
            status,
            detail,
        })
    }

    fn looking_up(&self, url: &str) {
        self.heading.set_label("Looking up…");
        self.status.set_label(url);
        self.bar.pulse();
    }

    fn started(&self, plan: &Plan) {
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
}
