use super::widgets::{
    done_check, icon_button, isolate, label, page, pill_button, placeholder_row, progress, row,
    section, value_label,
};
use super::{Act, Action};
use adw::prelude::*;
use patrak::db::Kind;
use patrak::progress::{Snapshot, fmt_duration, item_status};

pub struct TodayPage {
    pub root: gtk::ScrolledWindow,
    /// The two goal meters; the window stacks them when narrow.
    pub meters: gtk::Box,
    date: gtk::Label,
    goal_edit: gtk::MenuButton,
    minutes_goal: adw::SpinRow,
    pages_goal: adw::SpinRow,
    video_meter: GoalMeter,
    book_meter: GoalMeter,
    summary: gtk::Label,
    streak: gtk::Label,
    best: gtk::Label,
    plan_box: gtk::Box,
    plan_list: gtk::ListBox,
    continue_list: gtk::ListBox,
    review_box: gtk::Box,
    review_list: gtk::ListBox,
    today_list: gtk::ListBox,
    act: Act,
}

impl TodayPage {
    pub fn new(act: Act) -> Self {
        let (root, content) = page(720);

        let date = label("", &["dim-label", "title-4"]);
        date.set_hexpand(true);
        let (goal_edit, minutes_goal, pages_goal) = goal_editor(&act);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.append(&date);
        header.append(&goal_edit);

        let video_meter = goal_meter("Videos");
        let book_meter = goal_meter("Reading");
        let meters = gtk::Box::builder().spacing(24).homogeneous(true).build();
        meters.append(&video_meter.root);
        meters.append(&book_meter.root);
        let summary = label("", &["dim-label"]);

        let goal_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
        goal_box.append(&header);
        goal_box.append(&meters);
        goal_box.append(&summary);
        content.append(&goal_box);

        let (streak_card, streak) = stat_card("day streak");
        let (best_card, best) = stat_card("best streak");
        let cards = gtk::Box::builder().spacing(12).homogeneous(true).build();
        cards.append(&streak_card);
        cards.append(&best_card);
        content.append(&cards);

        let (plan_box, plan_list) = section("Today’s Plan");
        let (continue_box, continue_list) = section("Continue");
        let (review_box, review_list) = section("Review");
        let (today_box, today_list) = section("Studied today");
        content.append(&plan_box);
        content.append(&continue_box);
        content.append(&review_box);
        content.append(&today_box);

        Self {
            root,
            meters,
            date,
            goal_edit,
            minutes_goal,
            pages_goal,
            video_meter,
            book_meter,
            summary,
            streak,
            best,
            plan_box,
            plan_list,
            continue_list,
            review_box,
            review_list,
            today_list,
            act,
        }
    }

    pub fn update(&self, snap: &Snapshot) {
        let (studied, goals) = (snap.studied_today(), snap.goals);
        self.date
            .set_label(&snap.today.format("%A, %-d %B").to_string());
        self.video_meter
            .set(studied.minutes, goals.minutes, "min", |left| {
                format!("{} to go", fmt_duration(left * 60.0))
            });
        self.book_meter
            .set(studied.pages as f64, goals.pages, "pages", |left| {
                match left.ceil() as i64 {
                    1 => "1 page to go".to_owned(),
                    n => format!("{n} pages to go"),
                }
            });
        let summary = if goals.minutes <= 0.0 && goals.pages <= 0.0 {
            Some("No daily goal set. Use the pencil button to set one.")
        } else if goals.met(studied) {
            Some("Daily goal reached. Nice work!")
        } else {
            None
        };
        self.summary.set_visible(summary.is_some());
        self.summary.set_label(summary.unwrap_or_default());
        // Leave the spinners alone while they're being edited.
        if !self.goal_edit.is_active() {
            self.minutes_goal.set_value(goals.minutes);
            self.pages_goal.set_value(goals.pages);
        }
        self.streak.set_label(&snap.streak.to_string());
        self.best.set_label(&snap.best_streak.to_string());

        let act = &self.act;
        self.continue_list.remove_all();
        match snap.current_course() {
            Some(course) => {
                let start = course.next.unwrap_or(0);
                let lecture = &course.lectures[start];
                let r = row(
                    &lecture.title,
                    &format!(
                        "{}  ·  {}",
                        isolate(&course.name),
                        isolate(&item_status(lecture))
                    ),
                );
                r.add_prefix(&gtk::Image::from_icon_name("video-x-generic-symbolic"));
                let id = lecture.id;
                r.add_suffix(&icon_button(
                    "document-edit-symbolic",
                    "Notes",
                    act,
                    move || Action::Note(id),
                ));
                let key = course.key.clone();
                r.add_suffix(&pill_button("Play", act, move || Action::Play {
                    course_key: key.clone(),
                    start,
                }));
                self.continue_list.append(&r);
            }
            None if snap.courses.is_empty() => self.continue_list.append(&placeholder_row(
                "No courses yet. Add a folder of lectures to one of your video folders.",
            )),
            None => self
                .continue_list
                .append(&placeholder_row("Every course is finished.")),
        }
        if let Some(book) = snap.last_book() {
            let r = row(&book.title, &item_status(book));
            r.add_prefix(&gtk::Image::from_icon_name(
                "accessories-dictionary-symbolic",
            ));
            let id = book.id;
            r.add_suffix(&icon_button(
                "document-edit-symbolic",
                "Notes",
                act,
                move || Action::Note(id),
            ));
            r.add_suffix(&pill_button("Read", act, move || Action::Read(id)));
            self.continue_list.append(&r);
        }

        self.plan_list.remove_all();
        let plan = snap.plan();
        self.plan_box.set_visible(!plan.is_empty());
        for course in plan {
            let need = course.needed_per_day(snap.today).unwrap_or(0.0);
            let share = if need > 0.0 {
                course.today_secs / need
            } else {
                1.0
            };
            let today_part = if need > 0.0 {
                format!(
                    "{} of {} today",
                    fmt_duration(course.today_secs),
                    fmt_duration(need)
                )
            } else {
                String::new()
            };
            let summary = course.plan_summary(snap.today).unwrap_or_default();
            let subtitle = [today_part, summary]
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join("  ·  ");
            let r = row(&course.name, &subtitle);
            r.set_subtitle_lines(2);
            if share >= 1.0 {
                r.add_prefix(&gtk::Image::from_icon_name("object-select-symbolic"));
            }
            r.add_suffix(&progress(share, 90));
            if let Some(start) = course.next {
                let key = course.key.clone();
                r.add_suffix(&pill_button("Play", act, move || Action::Play {
                    course_key: key.clone(),
                    start,
                }));
            }
            self.plan_list.append(&r);
        }

        self.review_list.remove_all();
        self.review_box.set_visible(snap.cards_total > 0);
        let r = if snap.cards_due > 0 {
            let r = row(
                &format!(
                    "{} card{} due",
                    snap.cards_due,
                    if snap.cards_due == 1 { "" } else { "s" }
                ),
                &format!(
                    "{} cards in your notes  ·  {} reviewed today",
                    snap.cards_total, snap.reviewed_today
                ),
            );
            r.add_suffix(&pill_button("Review", act, || Action::Review));
            r
        } else {
            row(
                "All caught up",
                &format!(
                    "{} cards in your notes  ·  {} reviewed today",
                    snap.cards_total, snap.reviewed_today
                ),
            )
        };
        r.add_prefix(&gtk::Image::from_icon_name("dialog-question-symbolic"));
        self.review_list.append(&r);

        self.today_list.remove_all();
        if snap.today_entries.is_empty() {
            self.today_list
                .append(&placeholder_row("Nothing yet today."));
        }
        for e in &snap.today_entries {
            let r = row(&e.item.title, &e.item.course);
            let amount = match e.item.kind {
                Kind::Video => fmt_duration(e.seconds),
                Kind::Book => format!("{} pages", e.pages),
            };
            r.add_suffix(&value_label(&amount, &["numeric"]));
            r.add_suffix(&done_check(e.item.id, e.item.is_done(), act));
            self.today_list.append(&r);
        }
    }
}

/// Progress toward one daily goal: "32 / 45 min", a bar, and what's left.
struct GoalMeter {
    root: gtk::Box,
    value: gtk::Label,
    level: gtk::LevelBar,
    left: gtk::Label,
}

impl GoalMeter {
    /// Hidden when `goal` is 0; `to_go` words the amount still missing.
    fn set(&self, done: f64, goal: f64, unit: &str, to_go: impl Fn(f64) -> String) {
        self.root.set_visible(goal > 0.0);
        if goal <= 0.0 {
            return;
        }
        self.value
            .set_label(&format!("{done:.0} / {goal:.0} {unit}"));
        self.level.set_value((done / goal).min(1.0));
        self.left.set_label(&if done >= goal {
            "Done".to_owned()
        } else {
            to_go(goal - done)
        });
    }
}

fn goal_meter(caption: &str) -> GoalMeter {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let value = value_label("", &["big-number", "numeric"]);
    value.set_halign(gtk::Align::Start);
    let level = gtk::LevelBar::for_interval(0.0, 1.0);
    level.add_css_class("goal");
    let left = label("", &["dim-label"]);
    root.append(&label(caption, &["heading"]));
    root.append(&value);
    root.append(&level);
    root.append(&left);
    GoalMeter {
        root,
        value,
        level,
        left,
    }
}

/// The pencil button and its popover for changing the daily goals; returns
/// (button, minutes row, pages row). Closing the popover saves the goals.
fn goal_editor(act: &Act) -> (gtk::MenuButton, adw::SpinRow, adw::SpinRow) {
    let minutes = adw::SpinRow::with_range(0.0, 600.0, 5.0);
    minutes.set_title("Video minutes");
    let pages = adw::SpinRow::with_range(0.0, 1000.0, 5.0);
    pages.set_title("Book pages");
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list.add_css_class("boxed-list");
    list.append(&minutes);
    list.append(&pages);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_width_request(300);
    content.append(&label("Daily goals", &["heading"]));
    content.append(&list);
    content.append(&label(
        "Card review counts as video time. 0 turns a goal off.",
        &["dim-label", "caption"],
    ));
    let popover = gtk::Popover::builder().child(&content).build();
    let (m, p, act) = (minutes.clone(), pages.clone(), act.clone());
    popover.connect_closed(move |_| {
        // Take a number typed without pressing Enter.
        m.update();
        p.update();
        act(Action::SetGoals {
            minutes: m.value().round() as u32,
            pages: p.value().round() as u32,
        });
    });

    let button = gtk::MenuButton::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text("Change daily goals")
        .valign(gtk::Align::Center)
        .popover(&popover)
        .build();
    button.add_css_class("flat");
    (button, minutes, pages)
}

fn stat_card(caption: &str) -> (gtk::Box, gtk::Label) {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 2);
    card.add_css_class("card");
    card.add_css_class("stat-card");
    let value = label("0", &["big-number", "numeric"]);
    card.append(&value);
    card.append(&label(caption, &["dim-label", "caption"]));
    (card, value)
}
