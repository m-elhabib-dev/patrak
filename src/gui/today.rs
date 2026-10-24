use super::projects::{project_row, summary as project_summary};
use super::widgets::{
    done_check, done_mark, icon_button, isolate, label, page, pill_button, placeholder_row,
    progress, row, section, section_with, value_label,
};
use super::{Act, Action};
use adw::prelude::*;
use patrak::db::Kind;
use patrak::habits::{HabitStatus, Unit};
use patrak::progress::{Snapshot, fmt_duration, item_status, short_title, streak_text};
use patrak::projects;

pub struct TodayPage {
    pub root: gtk::ScrolledWindow,
    date: gtk::Label,
    /// "Holiday", next to the date on a holiday.
    holiday: gtk::Label,
    /// "3 of 4 goals done  ·  5-day streak"
    summary: gtk::Label,
    /// The share of every goal together, as the streak and heatmap see it.
    overall: gtk::LevelBar,
    goal_edit: gtk::MenuButton,
    minutes_goal: adw::SpinRow,
    pages_goal: adw::SpinRow,
    goals_list: gtk::ListBox,
    next_list: gtk::ListBox,
    plan_box: gtk::Box,
    plan_list: gtk::ListBox,
    /// Today's minutes, pages and commits, next to the "Done Today" heading.
    totals: gtk::Label,
    done_list: gtk::ListBox,
    act: Act,
}

impl TodayPage {
    pub fn new(act: Act) -> Self {
        let (root, content) = page(720);

        let date = label("", &["title-2"]);
        date.set_hexpand(true);
        let holiday = value_label("Holiday", &["holiday-pill", "caption"]);
        holiday.set_visible(false);
        let date_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        date_line.append(&date);
        date_line.append(&holiday);
        let summary = label("", &["dim-label"]);
        let overall = gtk::LevelBar::for_interval(0.0, 1.0);
        overall.add_css_class("goal");
        let header = gtk::Box::new(gtk::Orientation::Vertical, 8);
        header.append(&date_line);
        header.append(&summary);
        header.append(&overall);
        content.append(&header);

        let (goal_edit, minutes_goal, pages_goal) = goal_editor(&act);
        let (goals_box, goals_list) = section_with("Goals", &goal_edit);
        let (next_box, next_list) = section("Up Next");
        let (plan_box, plan_list) = section("Deadlines");
        let totals = value_label("", &["dim-label", "numeric"]);
        let (done_box, done_list) = section_with("Done Today", &totals);
        content.append(&goals_box);
        content.append(&next_box);
        content.append(&plan_box);
        content.append(&done_box);

        Self {
            root,
            date,
            holiday,
            summary,
            overall,
            goal_edit,
            minutes_goal,
            pages_goal,
            goals_list,
            next_list,
            plan_box,
            plan_list,
            totals,
            done_list,
            act,
        }
    }

    pub fn update(&self, snap: &Snapshot) {
        self.update_header(snap);
        self.update_goals(snap);
        self.update_next(snap);
        self.update_plan(snap);
        self.update_done(snap);
    }

    fn update_header(&self, snap: &Snapshot) {
        let (studied, goals) = (snap.studied_today(), snap.goals);
        self.date
            .set_label(&snap.today.format("%A, %-d %B").to_string());
        let holiday = snap.holiday_today();
        self.holiday.set_visible(holiday);
        let (met, total) = goals.tally(studied);
        let status = if holiday {
            "Holiday: today’s goals are off and your streak is safe".to_owned()
        } else if total == 0 {
            "No daily goals yet".to_owned()
        } else if met == total {
            "Every goal done today".to_owned()
        } else {
            format!("{met} of {total} goals done")
        };
        let line: Vec<String> = std::iter::once(status)
            .chain(streak_text(snap.streak, snap.best_streak))
            .collect();
        self.summary.set_label(&line.join("  ·  "));
        self.overall.set_visible(!holiday && total > 0);
        self.overall.set_value(goals.progress(studied).min(1.0));
    }

    /// Every goal that decides the streak: minutes, pages, each habit and each project with a
    /// goal. A holiday turns them off; what's done still shows, greyed out.
    fn update_goals(&self, snap: &Snapshot) {
        let (studied, goals) = (snap.studied_today(), snap.goals);
        let list = &self.goals_list;
        list.remove_all();
        list.set_sensitive(!snap.holiday_today());
        if goals.minutes > 0.0 {
            let text = amount_text(studied.minutes, goals.minutes, "min", |left| {
                fmt_duration(left * 60.0)
            });
            list.append(&goal_row(
                "Videos",
                &text,
                studied.minutes / goals.minutes,
                studied.minutes >= goals.minutes,
                None,
            ));
        }
        if goals.pages > 0.0 {
            let done = studied.pages as f64;
            let text = amount_text(done, goals.pages, "pages", |left| pages(left.ceil() as i64));
            list.append(&goal_row(
                "Reading",
                &text,
                done / goals.pages,
                done >= goals.pages,
                None,
            ));
        }
        for status in &snap.habits {
            let habit = &status.habit;
            let mut text = habit.progress_text(status.today);
            if let Some(streak) = status.streak_text() {
                text.push_str(&format!("  ·  {streak}"));
            }
            list.append(&goal_row(
                &habit.name,
                &text,
                habit.share(status.today),
                status.done(),
                Some(quick_log_button(status, &self.act).upcast()),
            ));
        }
        for status in snap
            .projects
            .iter()
            .filter(|p| p.project.has_goal() && !p.project.missing)
        {
            list.append(&project_row(status, &project_summary(status)));
        }
        if list.first_child().is_none() {
            list.append(&placeholder_row(
                "No daily goals. Set minutes or pages with the pencil button, or add a habit.",
            ));
        }
        // Leave the spinners alone while they're being edited.
        if !self.goal_edit.is_active() {
            self.minutes_goal.set_value(goals.minutes);
            self.pages_goal.set_value(goals.pages);
        }
    }

    /// The lecture and book to pick up, and the cards due.
    fn update_next(&self, snap: &Snapshot) {
        let (act, list) = (&self.act, &self.next_list);
        list.remove_all();
        match snap.current_course() {
            Some(course) => {
                let start = course.next.unwrap_or(0);
                let lecture = &course.lectures[start];
                let r = row(
                    short_title(&lecture.title, &course.name),
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
                list.append(&r);
            }
            None if snap.courses.is_empty() => list.append(&placeholder_row(
                "No courses yet. Add a folder of lectures to one of your video folders.",
            )),
            None => list.append(&placeholder_row("Every course is finished.")),
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
            list.append(&r);
        }
        if snap.cards_due > 0 {
            let r = row(
                &format!("{} due", cards(snap.cards_due as i64)),
                &format!("{} in your notes", cards(snap.cards_total as i64)),
            );
            r.add_prefix(&gtk::Image::from_icon_name("dialog-question-symbolic"));
            r.add_suffix(&pill_button("Review", act, || Action::Review));
            list.append(&r);
        }
    }

    /// Each course with a deadline: today's share of it and whether it's on track.
    fn update_plan(&self, snap: &Snapshot) {
        let (act, list) = (&self.act, &self.plan_list);
        list.remove_all();
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
            r.add_prefix(&done_mark(share >= 1.0));
            r.add_suffix(&progress(share, 90));
            if let Some(start) = course.next {
                let key = course.key.clone();
                r.add_suffix(&pill_button("Play", act, move || Action::Play {
                    course_key: key.clone(),
                    start,
                }));
            }
            list.append(&r);
        }
    }

    /// What was watched, read, committed and reviewed today, with the totals in the heading.
    fn update_done(&self, snap: &Snapshot) {
        let studied = snap.studied_today();
        let mut totals = Vec::new();
        if studied.minutes > 0.0 {
            totals.push(fmt_duration(studied.minutes * 60.0));
        }
        if studied.pages > 0 {
            totals.push(pages(studied.pages));
        }
        let commits: i64 = snap.projects.iter().map(|p| p.today).sum();
        if commits > 0 {
            totals.push(projects::count(commits));
        }
        self.totals.set_label(&totals.join("  ·  "));

        let list = &self.done_list;
        list.remove_all();
        for e in &snap.today_entries {
            let (icon, amount) = match e.item.kind {
                Kind::Video => ("video-x-generic-symbolic", fmt_duration(e.seconds)),
                Kind::Book => ("accessories-dictionary-symbolic", pages(e.pages)),
            };
            let r = row(short_title(&e.item.title, &e.item.course), &e.item.course);
            r.add_prefix(&gtk::Image::from_icon_name(icon));
            r.add_suffix(&value_label(&amount, &["numeric"]));
            r.add_suffix(&done_check(e.item.id, e.item.is_done(), &self.act));
            list.append(&r);
        }
        for status in snap.projects.iter().filter(|p| p.today > 0) {
            let r = row(&status.project.name, "");
            r.add_prefix(&gtk::Image::from_icon_name(
                "applications-engineering-symbolic",
            ));
            r.add_suffix(&value_label(&projects::count(status.today), &["numeric"]));
            list.append(&r);
        }
        if snap.reviewed_today > 0 {
            let r = row("Card review", "");
            r.add_prefix(&gtk::Image::from_icon_name("dialog-question-symbolic"));
            r.add_suffix(&value_label(&cards(snap.reviewed_today), &["numeric"]));
            list.append(&r);
        }
        if list.first_child().is_none() {
            list.append(&placeholder_row("Nothing yet today."));
        }
    }
}

/// A goal: a check once it's met, the amount under the name, then `extra` and a bar.
fn goal_row(
    title: &str,
    subtitle: &str,
    share: f64,
    done: bool,
    extra: Option<gtk::Widget>,
) -> adw::ActionRow {
    let r = row(title, subtitle);
    r.set_subtitle_lines(2);
    r.add_prefix(&done_mark(done));
    if let Some(extra) = extra {
        r.add_suffix(&extra);
    }
    r.add_suffix(&progress(share, 90));
    r
}

/// "95 of 120 min", then what's left: "  ·  25m to go".
fn amount_text(done: f64, goal: f64, unit: &str, left: impl Fn(f64) -> String) -> String {
    let mut text = format!("{done:.0} of {goal:.0} {unit}");
    if done < goal {
        text.push_str(&format!("  ·  {} to go", left(goal - done)));
    }
    text
}

/// Adds one time to a count habit, or completes a minutes habit (hidden once it's done).
fn quick_log_button(status: &HabitStatus, act: &Act) -> gtk::Button {
    let habit = &status.habit;
    let (text, tooltip) = match habit.unit {
        Unit::Count => ("+1", "Add 1 time".to_owned()),
        Unit::Minutes => (
            "Mark Done",
            format!("Log {}", habit.unit.amount(habit.goal)),
        ),
    };
    let button = gtk::Button::builder()
        .label(text)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .visible(habit.unit == Unit::Count || !status.done())
        .build();
    let (act, id) = (act.clone(), habit.id);
    button.connect_clicked(move |_| act(Action::QuickLogHabit(id)));
    button
}

fn pages(n: i64) -> String {
    if n == 1 {
        "1 page".to_owned()
    } else {
        format!("{n} pages")
    }
}

fn cards(n: i64) -> String {
    if n == 1 {
        "1 card".to_owned()
    } else {
        format!("{n} cards")
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
        "Card review counts as video time. 0 turns a goal off. Habits and projects have their goals on their own pages.",
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
        .tooltip_text("Change the minutes and pages goals")
        .valign(gtk::Align::Center)
        .popover(&popover)
        .build();
    button.add_css_class("flat");
    (button, minutes, pages)
}
