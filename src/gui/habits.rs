use super::widgets::{icon_button, label, page, pill_button};
use super::{Act, Action};
use adw::prelude::*;
use chrono::NaiveDate;
use patrak::habits::{Habit, HabitStatus, Unit};
use patrak::progress::Snapshot;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

pub struct HabitsPage {
    pub root: gtk::Stack,
    inner: Rc<Inner>,
}

struct Inner {
    root: gtk::Stack,
    /// Shown on a holiday.
    holiday: gtk::Label,
    list: gtk::ListBox,
    rows: RefCell<Vec<HabitRow>>,
    /// The day and habits the rows were built for.
    built_for: RefCell<Option<(NaiveDate, Vec<Habit>)>>,
    /// Amounts saved from a spinner that no snapshot has shown yet. A snapshot taken just
    /// before the save would otherwise roll the spinner back, and the next click would build
    /// on the old amount.
    pending: RefCell<HashMap<i64, i64>>,
    /// Set while spinners are changed from a snapshot, so that isn't saved as a new amount.
    updating: Cell<bool>,
    act: Act,
}

/// One habit: today's amount in a spinner, with the progress and streak below the name.
struct HabitRow {
    status: HabitStatus,
    spin: adw::SpinRow,
    done: gtk::Image,
}

impl HabitRow {
    fn show(&self, amount: i64) {
        let habit = &self.status.habit;
        let mut subtitle = habit.progress_text(amount);
        if let Some(streak) = self.status.streak_text() {
            subtitle.push_str(&format!("  ·  {streak}"));
        }
        self.spin.set_subtitle(&subtitle);
        let met = habit.met(amount);
        self.done.set_opacity(if met { 1.0 } else { 0.0 });
        self.done.set_tooltip_text(met.then_some("Done for today"));
    }
}

impl HabitsPage {
    pub fn new(act: Act) -> Self {
        let (scroller, content) = page(720);
        let heading = label("Today", &["heading"]);
        heading.set_hexpand(true);
        let add = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("list-add-symbolic")
                    .label("New Habit")
                    .build(),
            )
            .build();
        add.add_css_class("flat");
        let a = act.clone();
        add.connect_clicked(move |_| a(Action::NewHabit));
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.append(&heading);
        header.append(&add);

        let holiday = label(
            "Today is a holiday: habits are optional and don’t affect your streaks.",
            &["dim-label"],
        );
        holiday.set_visible(false);

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        list.add_css_class("boxed-list");
        let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
        section.append(&header);
        section.append(&holiday);
        section.append(&list);
        section.append(&label(
            "Habits are part of the daily goal: a day keeps your streak only when every habit is done too. Use − and + or type today’s amount.",
            &["dim-label", "caption"],
        ));
        content.append(&section);

        let new_habit = pill_button("New Habit", &act, || Action::NewHabit);
        new_habit.set_halign(gtk::Align::Center);
        let empty = adw::StatusPage::builder()
            .icon_name("object-select-symbolic")
            .title("No Habits Yet")
            .description("Track anything you do every day, like running 30 minutes or eating three times. Habits count toward your daily goal and streak.")
            .child(&new_habit)
            .build();

        let root = gtk::Stack::new();
        root.add_named(&scroller, Some("habits"));
        root.add_named(&empty, Some("empty"));
        root.set_visible_child_name("empty");

        let inner = Rc::new(Inner {
            root: root.clone(),
            holiday,
            list,
            rows: RefCell::new(Vec::new()),
            built_for: RefCell::new(None),
            pending: RefCell::new(HashMap::new()),
            updating: Cell::new(false),
            act,
        });
        Self { root, inner }
    }

    pub fn update(&self, snap: &Snapshot) {
        let inner = &self.inner;
        inner
            .root
            .set_visible_child_name(if snap.habits.is_empty() {
                "empty"
            } else {
                "habits"
            });
        inner.holiday.set_visible(snap.holiday_today());
        // Rebuilding would wipe a number being typed, so rows are only rebuilt when the
        // habits themselves or the day change.
        let key = (
            snap.today,
            snap.habits.iter().map(|h| h.habit.clone()).collect(),
        );
        if inner.built_for.borrow().as_ref() == Some(&key) {
            inner.refresh_rows(&snap.habits);
        } else {
            Inner::rebuild(inner, &snap.habits);
            *inner.built_for.borrow_mut() = Some(key);
        }
    }
}

impl Inner {
    fn rebuild(self: &Rc<Self>, statuses: &[HabitStatus]) {
        self.pending.borrow_mut().clear();
        self.list.remove_all();
        let rows: Vec<HabitRow> = statuses.iter().map(|s| self.habit_row(s)).collect();
        for row in &rows {
            self.list.append(&row.spin);
        }
        *self.rows.borrow_mut() = rows;
    }

    fn refresh_rows(&self, statuses: &[HabitStatus]) {
        self.updating.set(true);
        let mut pending = self.pending.borrow_mut();
        for (row, status) in self.rows.borrow_mut().iter_mut().zip(statuses) {
            let id = status.habit.id;
            match pending.get(&id) {
                Some(&saved) if saved != status.today => continue,
                Some(_) => {
                    pending.remove(&id);
                }
                None => {}
            }
            row.status = status.clone();
            // Setting the same value would reset text typed without Enter.
            if row.spin.value().round() as i64 != status.today {
                row.spin.set_value(status.today as f64);
            }
            row.show(status.today);
        }
        self.updating.set(false);
    }

    fn habit_row(self: &Rc<Self>, status: &HabitStatus) -> HabitRow {
        let habit = &status.habit;
        let max = match habit.unit {
            Unit::Count => 1000.0,
            Unit::Minutes => 1440.0,
        };
        let spin = adw::SpinRow::with_range(0.0, max, habit.unit.step() as f64);
        spin.set_title(&habit.name);
        spin.set_use_markup(false);
        spin.set_subtitle_lines(2);
        spin.set_value(status.today as f64);
        let done = gtk::Image::from_icon_name("object-select-symbolic");
        done.add_css_class("success");
        spin.add_prefix(&done);
        let id = habit.id;
        spin.add_suffix(&icon_button(
            "document-edit-symbolic",
            "Edit habit",
            &self.act,
            move || Action::EditHabit(id),
        ));

        let weak = Rc::downgrade(self);
        spin.connect_value_notify(move |spin| {
            let Some(inner) = weak.upgrade() else { return };
            if inner.updating.get() {
                return;
            }
            let amount = spin.value().round() as i64;
            inner.pending.borrow_mut().insert(id, amount);
            if let Some(row) = inner.rows.borrow().iter().find(|r| r.status.habit.id == id) {
                row.show(amount);
            }
            (inner.act)(Action::LogHabit { id, amount });
        });

        let row = HabitRow {
            status: status.clone(),
            spin,
            done,
        };
        row.show(status.today);
        row
    }
}
