use super::App;
use super::widgets::{dialog, isolate, label};
use adw::prelude::*;
use patrak::db;
use patrak::habits::{self, Habit, Unit};
use std::rc::Rc;

/// The goal a new habit starts with in each unit.
fn default_goal(unit: Unit) -> f64 {
    match unit {
        Unit::Count => 1.0,
        Unit::Minutes => 30.0,
    }
}

/// Adds a habit, or edits `habit`. The unit can't change later, so past amounts keep their
/// meaning.
pub fn show(app: &Rc<App>, habit: Option<Habit>) {
    let name = adw::EntryRow::builder()
        .title("Name")
        .text(habit.as_ref().map_or("", |h| h.name.as_str()))
        .build();
    let units = gtk::StringList::new(&Unit::ALL.map(Unit::label));
    let unit = adw::ComboRow::builder()
        .title("Measured in")
        .model(&units)
        .build();
    let goal = adw::SpinRow::with_range(1.0, 1440.0, 1.0);
    goal.set_title("Daily goal");
    let group = adw::PreferencesGroup::new();
    group.add(&name);
    group.add(&unit);
    group.add(&goal);

    let selected_unit = |row: &adw::ComboRow| Unit::ALL[(row.selected() as usize).min(1)];
    let set_unit_hints = {
        let goal = goal.clone();
        move |unit: Unit| {
            goal.set_subtitle(match unit {
                Unit::Count => "times a day",
                Unit::Minutes => "minutes a day",
            });
            goal.adjustment().set_step_increment(unit.step() as f64);
        }
    };
    match &habit {
        Some(h) => {
            unit.set_selected(Unit::ALL.iter().position(|&u| u == h.unit).unwrap_or(0) as u32);
            unit.set_sensitive(false);
            unit.set_subtitle("Can’t be changed later");
            goal.set_value(h.goal as f64);
            set_unit_hints(h.unit);
        }
        None => {
            group.set_description(Some(
                "Counts toward the daily goal and streak from today on.",
            ));
            goal.set_value(default_goal(Unit::Count));
            set_unit_hints(Unit::Count);
            let goal = goal.clone();
            unit.connect_selected_notify(move |row| {
                let unit = selected_unit(row);
                // Swap an untouched default for the new unit's.
                if Unit::ALL
                    .iter()
                    .any(|&u| u != unit && goal.value() == default_goal(u))
                {
                    goal.set_value(default_goal(unit));
                }
                set_unit_hints(unit);
            });
        }
    }

    let error = label("", &["error"]);
    error.set_visible(false);

    let save = gtk::Button::builder()
        .label(if habit.is_some() { "Save" } else { "Add Habit" })
        .sensitive(!name.text().trim().is_empty())
        .build();
    save.add_css_class("pill");
    save.add_css_class("suggested-action");
    let buttons = gtk::Box::builder()
        .spacing(12)
        .halign(gtk::Align::Center)
        .build();

    let form = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    form.append(&group);
    form.append(&error);
    form.append(&buttons);
    let title = if habit.is_some() {
        "Edit Habit"
    } else {
        "New Habit"
    };
    let dialog = dialog(title, &form);
    dialog.set_content_width(440);

    if let Some(h) = &habit {
        let delete = gtk::Button::builder().label("Delete").build();
        delete.add_css_class("pill");
        delete.add_css_class("destructive-action");
        let (weak, d, h) = (Rc::downgrade(app), dialog.clone(), h.clone());
        delete.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                confirm_delete(&app, &d, &h);
            }
        });
        buttons.append(&delete);
    }
    buttons.append(&save);

    let s = save.clone();
    name.connect_changed(move |e| s.set_sensitive(!e.text().trim().is_empty()));
    let s = save.clone();
    name.connect_entry_activated(move |_| s.emit_clicked());

    let (weak, d, id) = (Rc::downgrade(app), dialog.clone(), habit.map(|h| h.id));
    save.connect_clicked(move |_| {
        let Some(app) = weak.upgrade() else { return };
        // Take a number typed without pressing Enter.
        goal.update();
        let (text, goal) = (name.text(), goal.value().round() as i64);
        let result = db::open(&app.paths.db()).and_then(|conn| match id {
            Some(id) => habits::update(&conn, id, &text, goal),
            None => {
                let today = app.cfg.borrow().today();
                habits::add(&conn, &text, selected_unit(&unit), goal, today).map(drop)
            }
        });
        match result {
            Ok(()) => {
                d.close();
                app.refresh();
            }
            Err(e) => {
                error.set_label(&format!("Couldn't save: {e:#}"));
                error.set_visible(true);
            }
        }
    });
    dialog.present(Some(&app.window));
}

fn confirm_delete(app: &Rc<App>, dialog: &adw::Dialog, habit: &Habit) {
    let alert = adw::AlertDialog::new(
        Some(&format!("Delete {}?", isolate(&habit.name))),
        Some("Its history is deleted too, and past days are counted without it."),
    );
    alert.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
    alert.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    alert.set_default_response(Some("cancel"));
    alert.set_close_response("cancel");
    let (weak, d, id) = (Rc::downgrade(app), dialog.clone(), habit.id);
    alert.connect_response(Some("delete"), move |_, _| {
        let Some(app) = weak.upgrade() else { return };
        match db::open(&app.paths.db()).and_then(|conn| habits::delete(&conn, id)) {
            Ok(()) => {
                d.close();
                app.refresh();
            }
            Err(e) => app.toast(&format!("Couldn't delete the habit: {e:#}")),
        }
    });
    alert.present(Some(dialog));
}
