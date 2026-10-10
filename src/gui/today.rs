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
    date: gtk::Label,
    minutes: gtk::Label,
    level: gtk::LevelBar,
    remaining: gtk::Label,
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
        let minutes = label("", &["big-number", "numeric"]);
        let level = gtk::LevelBar::for_interval(0.0, 1.0);
        level.add_css_class("goal");
        let remaining = label("", &["dim-label"]);
        let goal_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        for w in [
            date.upcast_ref::<gtk::Widget>(),
            minutes.upcast_ref(),
            level.upcast_ref(),
            remaining.upcast_ref(),
        ] {
            goal_box.append(w);
        }
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
            date,
            minutes,
            level,
            remaining,
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
        let mins = snap.today_minutes();
        self.date
            .set_label(&snap.today.format("%A, %-d %B").to_string());
        self.minutes
            .set_label(&format!("{mins:.0} / {:.0} min", snap.goal));
        self.level.set_value((mins / snap.goal).min(1.0));
        self.remaining.set_label(&if mins >= snap.goal {
            "Daily goal reached. Nice work!".to_owned()
        } else {
            format!("{} to go", fmt_duration((snap.goal - mins) * 60.0))
        });
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

fn stat_card(caption: &str) -> (gtk::Box, gtk::Label) {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 2);
    card.add_css_class("card");
    card.add_css_class("stat-card");
    let value = label("0", &["big-number", "numeric"]);
    card.append(&value);
    card.append(&label(caption, &["dim-label", "caption"]));
    (card, value)
}
