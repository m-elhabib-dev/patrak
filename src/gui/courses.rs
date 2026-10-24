use super::widgets::{
    clear, done_check, icon_button, label, page, pill_button, progress, row, section,
};
use super::{Act, Action};
use adw::prelude::*;
use chrono::{Datelike, NaiveDate};
use gtk::glib;
use gtk::pango;
use patrak::config;
use patrak::download::Source;
use patrak::progress::{Course, PlanStatus, Snapshot, fmt_clock, fmt_duration, item_status};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub struct CoursesPage {
    pub root: adw::NavigationSplitView,
    inner: Rc<Inner>,
}

struct Inner {
    split: adw::NavigationSplitView,
    list: gtk::ListBox,
    content_page: adw::NavigationPage,
    content: gtk::Box,
    empty: adw::StatusPage,
    stack: gtk::Stack,
    /// Course keys in list order.
    keys: RefCell<Vec<String>>,
    selected: RefCell<Option<String>>,
    snapshot: RefCell<Option<Rc<Snapshot>>>,
    rebuilding: Cell<bool>,
    act: Act,
}

impl CoursesPage {
    pub fn new(act: Act) -> Self {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        list.add_css_class("navigation-sidebar");
        let sidebar_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .vexpand(true)
            .build();
        let sidebar = adw::NavigationPage::new(&sidebar_scroller, "Courses");

        let (scroller, content) = page(860);
        let empty = adw::StatusPage::builder()
            .icon_name("video-x-generic-symbolic")
            .title("No Courses Yet")
            .description("Put each course in its own folder inside one of your video folders (Preferences → Library). A single long video works too.")
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some("course"));
        stack.add_named(&empty, Some("empty"));
        let content_page = adw::NavigationPage::new(&stack, "Course");

        let split = adw::NavigationSplitView::builder()
            .sidebar(&sidebar)
            .content(&content_page)
            .min_sidebar_width(280.0)
            .max_sidebar_width(380.0)
            .build();

        let inner = Rc::new(Inner {
            split: split.clone(),
            list: list.clone(),
            content_page,
            content,
            empty,
            stack,
            keys: RefCell::new(Vec::new()),
            selected: RefCell::new(None),
            snapshot: RefCell::new(None),
            rebuilding: Cell::new(false),
            act,
        });

        let weak = Rc::downgrade(&inner);
        list.connect_row_selected(move |_, row| {
            let (Some(inner), Some(row)) = (weak.upgrade(), row) else {
                return;
            };
            if inner.rebuilding.get() {
                return;
            }
            let key = inner.keys.borrow().get(row.index() as usize).cloned();
            *inner.selected.borrow_mut() = key;
            inner.show_selected();
        });
        let weak = Rc::downgrade(&inner);
        list.connect_row_activated(move |_, _| {
            if let Some(inner) = weak.upgrade() {
                inner.split.set_show_content(true);
            }
        });

        Self { root: split, inner }
    }

    pub fn update(&self, snap: &Rc<Snapshot>) {
        *self.inner.snapshot.borrow_mut() = Some(snap.clone());
        self.inner.rebuild(snap);
    }
}

impl Inner {
    fn rebuild(&self, snap: &Snapshot) {
        self.rebuilding.set(true);
        self.list.remove_all();
        let keys: Vec<String> = snap.courses.iter().map(|c| c.key.clone()).collect();
        for course in &snap.courses {
            self.list.append(&sidebar_row(course));
        }

        // Keep the selection across refreshes; default to the course being studied.
        let selected = self
            .selected
            .borrow()
            .clone()
            .filter(|k| keys.contains(k))
            .or_else(|| {
                snap.current_course()
                    .or(snap.courses.first())
                    .map(|c| c.key.clone())
            });
        if let Some(i) = selected
            .as_ref()
            .and_then(|k| keys.iter().position(|x| x == k))
        {
            self.list
                .select_row(self.list.row_at_index(i as i32).as_ref());
        }
        *self.keys.borrow_mut() = keys;
        *self.selected.borrow_mut() = selected;
        self.rebuilding.set(false);
        self.show_selected();
    }

    fn show_selected(&self) {
        let snap = self.snapshot.borrow().clone();
        let course = snap.as_ref().and_then(|s| {
            let key = self.selected.borrow().clone()?;
            s.courses.iter().find(|c| c.key == key)
        });
        let Some(course) = course else {
            self.stack.set_visible_child(&self.empty);
            self.content_page.set_title("Courses");
            return;
        };
        self.stack.set_visible_child_name("course");
        self.content_page.set_title(&course.name);
        self.render(
            course,
            snap.as_ref()
                .map_or_else(|| chrono::Local::now().date_naive(), |s| s.today),
        );
    }

    fn render(&self, course: &Course, today: NaiveDate) {
        let act = &self.act;
        clear(&self.content);

        let header = gtk::Box::new(gtk::Orientation::Vertical, 8);
        header.append(&label(&course.name, &["title-1"]));
        header.append(&label(&summary(course), &["dim-label"]));
        header.append(&progress(course.fraction, -1));
        if let Some(start) = course.next {
            let key = course.key.clone();
            let label = if course.lectures[start].position > 0.0 {
                "Resume"
            } else {
                "Play Next"
            };
            let play = pill_button(label, act, move || Action::Play {
                course_key: key.clone(),
                start,
            });
            play.set_halign(gtk::Align::Start);
            play.set_margin_top(8);
            header.append(&play);
        }
        self.content.append(&header);

        let (plan_box, plan_list) = section("Plan");
        plan_list.append(&deadline_row(course, today, act));
        if let Some(source) = &course.source {
            plan_list.append(&source_row(course, source, act));
        }
        self.content.append(&plan_box);

        let (lectures_box, list) = section(if course.is_single_file() {
            "Lecture"
        } else {
            "Lectures"
        });
        for (i, lecture) in course.lectures.iter().enumerate() {
            let status = if lecture.missing {
                format!("File missing  ·  {}", item_status(lecture))
            } else {
                item_status(lecture)
            };
            let r = row(&lecture.title, &status);
            let number = label(&(i + 1).to_string(), &["dim-label", "numeric"]);
            number.set_width_chars(3);
            number.set_xalign(1.0);
            r.add_prefix(&number);
            if Some(i) == course.next {
                r.set_subtitle(&format!("Up next  ·  {}", item_status(lecture)));
                r.add_css_class("accent");
            }
            let id = lecture.id;
            if !lecture.missing {
                let key = course.key.clone();
                r.add_suffix(&icon_button(
                    "media-playback-start-symbolic",
                    "Play from here",
                    act,
                    move || Action::Play {
                        course_key: key.clone(),
                        start: i,
                    },
                ));
                r.add_suffix(&icon_button(
                    "document-edit-symbolic",
                    "Notes",
                    act,
                    move || Action::Note(id),
                ));
            }
            r.add_suffix(&done_check(id, lecture.is_done(), act));
            list.append(&r);
        }
        self.content.append(&lectures_box);

        if let [lecture] = course.lectures.as_slice()
            && !lecture.chapters.is_empty()
        {
            let current = lecture.current_chapter().map(|(i, _)| i);
            let (chapters_box, list) = section("Chapters");
            for (i, ch) in lecture.chapters.iter().enumerate() {
                let r = row(&ch.title, &fmt_clock(ch.start));
                let icon = gtk::Image::new();
                icon.set_pixel_size(16);
                if Some(i) == current && !lecture.is_done() {
                    icon.set_icon_name(Some("media-playback-start-symbolic"));
                } else if lecture.is_done() || lecture.position > ch.start {
                    icon.set_icon_name(Some("object-select-symbolic"));
                }
                r.add_prefix(&icon);
                let (id, secs) = (lecture.id, ch.start);
                if !lecture.missing {
                    r.add_suffix(&icon_button(
                        "media-playback-start-symbolic",
                        "Play this chapter",
                        act,
                        move || Action::PlayAt { item_id: id, secs },
                    ));
                }
                list.append(&r);
            }
            self.content.append(&chapters_box);
        }
    }
}

fn summary(course: &Course) -> String {
    let mut parts = vec![format!(
        "{}/{} lectures",
        course.done,
        course.lectures.len()
    )];
    if course.total_secs > 0.0 {
        parts.push(format!("{:.0}%", course.fraction * 100.0));
    }
    if course.missing {
        parts.push("files missing".into());
    }
    if course.remaining_secs > 0.0 {
        parts.push(format!("{} left", fmt_duration(course.remaining_secs)));
    }
    if let Some(deadline) = course.deadline {
        parts.push(format!("due {}", deadline.format("%b %-d")));
    } else if let Some(eta) = course.eta {
        parts.push(format!("ETA {}", eta.format("%b %-d")));
    }
    if let Some(n) = course
        .source
        .as_ref()
        .map(|s| s.new_titles.len())
        .filter(|&n| n > 0)
    {
        parts.push(format!("{n} new"));
    }
    parts.join("  ·  ")
}

/// The deadline with a calendar to change it.
fn deadline_row(course: &Course, today: NaiveDate, act: &Act) -> adw::ActionRow {
    let subtitle = course
        .plan_summary(today)
        .unwrap_or_else(|| "None. Set one to get a daily share in Today’s plan.".into());
    let r = row("Deadline", &subtitle);
    r.set_subtitle_lines(2);
    if course.plan_status(today) == Some(PlanStatus::Behind)
        || course.plan_status(today) == Some(PlanStatus::Overdue)
    {
        r.add_css_class("warning");
    }
    r.add_prefix(&gtk::Image::from_icon_name("x-office-calendar-symbolic"));

    let calendar = gtk::Calendar::new();
    let initial = course
        .deadline
        .unwrap_or(today + chrono::Duration::weeks(4));
    if let Ok(dt) = glib::DateTime::from_local(
        initial.year(),
        initial.month() as i32,
        initial.day() as i32,
        12,
        0,
        0.0,
    ) {
        calendar.select_day(&dt);
    }
    let set = gtk::Button::builder().label("Set Deadline").build();
    set.add_css_class("suggested-action");
    let popover_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    popover_box.append(&calendar);
    popover_box.append(&set);
    let popover = gtk::Popover::builder().child(&popover_box).build();
    let choose = gtk::MenuButton::builder()
        .icon_name("x-office-calendar-symbolic")
        .tooltip_text("Choose a deadline")
        .popover(&popover)
        .valign(gtk::Align::Center)
        .build();
    choose.add_css_class("flat");
    let (key, act2, pop) = (course.key.clone(), act.clone(), popover.clone());
    set.connect_clicked(move |_| {
        let dt = calendar.date();
        let date = NaiveDate::from_ymd_opt(dt.year(), dt.month() as u32, dt.day_of_month() as u32);
        pop.popdown();
        act2(Action::SetDeadline(key.clone(), date));
    });
    if course.deadline.is_some() {
        let key = course.key.clone();
        r.add_suffix(&icon_button(
            "edit-clear-symbolic",
            "Remove the deadline",
            act,
            move || Action::SetDeadline(key.clone(), None),
        ));
    }
    r.add_suffix(&choose);
    r
}

/// Where a downloaded course comes from, with buttons to check and fetch new episodes.
fn source_row(course: &Course, source: &Source, act: &Act) -> adw::ActionRow {
    let checked = source
        .checked_at
        .map(|t| format!("checked {}", ago(config::now() - t)))
        .unwrap_or_else(|| "not checked yet".into());
    let new = match source.new_titles.len() {
        0 => "no new episodes".to_owned(),
        1 => format!("new: {}", source.new_titles[0]),
        n => format!("{n} new episodes"),
    };
    let r = row("YouTube playlist", &format!("{new}  ·  {checked}"));
    r.add_prefix(&gtk::Image::from_icon_name("folder-download-symbolic"));
    if !source.new_titles.is_empty() {
        r.set_tooltip_text(Some(&source.new_titles.join("\n")));
    }
    let url = source.url.clone();
    r.add_suffix(&icon_button(
        "web-browser-symbolic",
        "Open the playlist",
        act,
        move || Action::OpenUrl(url.clone()),
    ));
    let dir = course.dir.clone();
    r.add_suffix(&icon_button(
        "view-refresh-symbolic",
        "Check for new episodes",
        act,
        move || Action::CheckCourse(dir.clone()),
    ));
    let dir = course.dir.clone();
    let download = pill_button("Download New", act, move || {
        Action::UpdateCourse(dir.clone())
    });
    download.set_sensitive(!source.new_titles.is_empty() || source.checked_at.is_none());
    r.add_suffix(&download);
    r
}

fn ago(secs: i64) -> String {
    match secs {
        s if s < 90 => "just now".into(),
        s if s < 5400 => format!("{} min ago", s / 60),
        s if s < 2 * 86400 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86400),
    }
}

fn sidebar_row(course: &Course) -> gtk::ListBoxRow {
    let vbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    let name = gtk::Label::builder()
        .label(&course.name)
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .tooltip_text(&course.name)
        .build();
    name.add_css_class("heading");
    let meta = gtk::Label::builder()
        .label(summary(course))
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    meta.add_css_class("caption");
    meta.add_css_class("dim-label");
    vbox.append(&name);
    vbox.append(&meta);
    vbox.append(&progress(course.fraction, -1));
    gtk::ListBoxRow::builder().child(&vbox).build()
}
