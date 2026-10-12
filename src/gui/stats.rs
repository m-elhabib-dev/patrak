use super::widgets::{label, page, progress, value_label};
use adw::prelude::*;
use chrono::{Datelike, Duration};
use patrak::progress::{Day, Snapshot, fmt_duration};
use patrak::projects;

const WEEKS: i64 = 26;

pub struct StatsPage {
    pub root: gtk::ScrolledWindow,
    streak: gtk::Label,
    best: gtk::Label,
    this_week: gtk::Label,
    total: gtk::Label,
    heat: gtk::Grid,
    weeks: gtk::Grid,
}

impl StatsPage {
    pub fn new() -> Self {
        let (root, content) = page(860);

        let cards = gtk::FlowBox::builder()
            .homogeneous(true)
            .min_children_per_line(2)
            .max_children_per_line(4)
            .column_spacing(12)
            .row_spacing(12)
            .selection_mode(gtk::SelectionMode::None)
            .build();
        let card = |caption: &str| {
            let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
            b.add_css_class("card");
            b.add_css_class("stat-card");
            let value = label("0", &["title-1", "numeric"]);
            b.append(&value);
            b.append(&label(caption, &["dim-label", "caption"]));
            cards.append(&b);
            value
        };
        let streak = card("day streak");
        let best = card("best streak");
        let this_week = card("this week");
        let total = card("all time");
        content.append(&cards);

        let heat = gtk::Grid::builder()
            .row_spacing(3)
            .column_spacing(3)
            .halign(gtk::Align::Center)
            .build();
        let heat_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        heat_box.append(&label("Last 26 weeks", &["heading"]));
        let heat_scroller = gtk::ScrolledWindow::builder()
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&heat)
            .build();
        heat_box.append(&heat_scroller);
        heat_box.append(&label(
            "Brighter is closer to the daily goals; green means they were all met.",
            &["dim-label", "caption"],
        ));
        content.append(&heat_box);

        let weeks = gtk::Grid::builder()
            .row_spacing(8)
            .column_spacing(12)
            .build();
        let weeks_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        weeks_box.append(&label("Per week", &["heading"]));
        weeks_box.append(&weeks);
        content.append(&weeks_box);

        Self {
            root,
            streak,
            best,
            this_week,
            total,
            heat,
            weeks,
        }
    }

    pub fn update(&self, snap: &Snapshot) {
        self.streak.set_label(&snap.streak.to_string());
        self.best.set_label(&snap.best_streak.to_string());
        let weekly = snap.weekly(12);
        let this_week = weekly.last().map_or(0.0, |w| w.1.minutes);
        self.this_week.set_label(&fmt_duration(this_week * 60.0));
        let total: Day = snap.days.values().copied().sum();
        self.total.set_label(&fmt_duration(total.minutes * 60.0));

        clear_grid(&self.heat);
        let start = snap.today
            - Duration::days(snap.today.weekday().num_days_from_monday() as i64)
            - Duration::weeks(WEEKS - 1);
        for (row, name) in ["Mon", "", "Wed", "", "Fri", "", "Sun"]
            .into_iter()
            .enumerate()
        {
            self.heat.attach(
                &value_label(name, &["caption", "dim-label"]),
                0,
                row as i32 + 1,
                1,
                1,
            );
        }
        for week in 0..WEEKS {
            let monday = start + Duration::weeks(week);
            let new_month = week == 0 || monday.month() != (monday - Duration::weeks(1)).month();
            let room =
                (week + 3 < WEEKS) && (monday + Duration::weeks(3)).month() == monday.month();
            if new_month && (week > 0 || room) {
                let month =
                    value_label(&monday.format("%b").to_string(), &["caption", "dim-label"]);
                self.heat.attach(&month, week as i32 + 1, 0, 3, 1);
            }
            for day in 0..7 {
                let date = monday + Duration::days(day);
                let cell = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                cell.add_css_class("heat");
                if date <= snap.today {
                    cell.add_css_class(&format!("heat-{}", snap.heat_level(date)));
                    let studied = snap.studied_on(date);
                    let mut parts = vec![
                        date.format("%a %-d %b").to_string(),
                        format!("{:.0} min", studied.minutes),
                        format!("{} pages", studied.pages),
                    ];
                    if studied.commits > 0 {
                        parts.push(projects::count(studied.commits));
                    }
                    if studied.habits_due > 0 {
                        parts.push(format!(
                            "{}/{} habits",
                            studied.habits_met, studied.habits_due
                        ));
                    }
                    if studied.projects_due > 0 {
                        parts.push(format!(
                            "{}/{} projects",
                            studied.projects_met, studied.projects_due
                        ));
                    }
                    cell.set_tooltip_text(Some(&parts.join("  ·  ")));
                }
                self.heat
                    .attach(&cell, week as i32 + 1, day as i32 + 1, 1, 1);
            }
        }

        clear_grid(&self.weeks);
        let max = weekly
            .iter()
            .map(|w| w.1.minutes)
            .fold(snap.goals.minutes * 7.0, f64::max)
            .max(1.0);
        for (i, (monday, week)) in weekly.iter().enumerate() {
            let row = i as i32;
            self.weeks.attach(
                &label(
                    &monday.format("%b %-d").to_string(),
                    &["numeric", "dim-label"],
                ),
                0,
                row,
                1,
                1,
            );
            let bar = progress(week.minutes / max, -1);
            bar.set_hexpand(true);
            self.weeks.attach(&bar, 1, row, 1, 1);
            let value = value_label(&format!("{:.0} min", week.minutes), &["numeric"]);
            value.set_xalign(1.0);
            value.set_width_chars(8);
            self.weeks.attach(&value, 2, row, 1, 1);
            if week.pages > 0 {
                let pages =
                    value_label(&format!("{} pages", week.pages), &["numeric", "dim-label"]);
                pages.set_xalign(1.0);
                pages.set_width_chars(10);
                self.weeks.attach(&pages, 3, row, 1, 1);
            }
            if week.commits > 0 {
                let commits =
                    value_label(&projects::count(week.commits), &["numeric", "dim-label"]);
                commits.set_xalign(1.0);
                commits.set_width_chars(11);
                self.weeks.attach(&commits, 4, row, 1, 1);
            }
        }
    }
}

fn clear_grid(grid: &gtk::Grid) {
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }
}
