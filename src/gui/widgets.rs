//! Small widget helpers shared by the pages.

use super::{Act, Action};
use adw::prelude::*;
use gtk::pango;

/// A scrolling page with its content centred at a readable width.
pub fn page(max_width: i32) -> (gtk::ScrolledWindow, gtk::Box) {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    let clamp = adw::Clamp::builder()
        .maximum_size(max_width)
        .child(&content)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build();
    (scroller, content)
}

/// A heading above a boxed list; returns (container, list).
pub fn section(title: &str) -> (gtk::Box, gtk::ListBox) {
    boxed_list_below(&label(title, &["heading"]))
}

/// A section with `extra` at the end of the heading's line.
pub fn section_with(title: &str, extra: &impl IsA<gtk::Widget>) -> (gtk::Box, gtk::ListBox) {
    let heading = label(title, &["heading"]);
    heading.set_hexpand(true);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.append(&heading);
    header.append(extra);
    boxed_list_below(&header)
}

fn boxed_list_below(header: &impl IsA<gtk::Widget>) -> (gtk::Box, gtk::ListBox) {
    let container = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list.add_css_class("boxed-list");
    container.append(header);
    container.append(&list);
    (container, list)
}

pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .build();
    for c in classes {
        l.add_css_class(c);
    }
    l
}

/// A single-line label for numbers next to other widgets.
pub fn value_label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = label(text, classes);
    l.set_wrap(false);
    l.set_valign(gtk::Align::Center);
    l
}

/// A dialog with a header bar above `child`.
pub fn dialog(title: &str, child: &impl IsA<gtk::Widget>) -> adw::Dialog {
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(child));
    adw::Dialog::builder()
        .title(title)
        .content_width(560)
        .child(&toolbar)
        .build()
}

pub fn row(title: &str, subtitle: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .use_markup(false)
        .title_lines(2)
        .subtitle_lines(1)
        .build()
}

pub fn placeholder_row(text: &str) -> adw::ActionRow {
    let r = row(text, "");
    r.add_css_class("dim-label");
    r
}

pub fn icon_button(
    icon: &str,
    tooltip: &str,
    act: &Act,
    action: impl Fn() -> Action + 'static,
) -> gtk::Button {
    let b = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .build();
    b.add_css_class("flat");
    let act = act.clone();
    b.connect_clicked(move |_| act(action()));
    b
}

pub fn pill_button(text: &str, act: &Act, action: impl Fn() -> Action + 'static) -> gtk::Button {
    let b = gtk::Button::builder()
        .label(text)
        .valign(gtk::Align::Center)
        .build();
    b.add_css_class("pill");
    b.add_css_class("suggested-action");
    let act = act.clone();
    b.connect_clicked(move |_| act(action()));
    b
}

pub fn done_check(item_id: i64, done: bool, act: &Act) -> gtk::CheckButton {
    let check = gtk::CheckButton::builder()
        .active(done)
        .tooltip_text(if done {
            "Done — click to mark not done"
        } else {
            "Mark as done"
        })
        .valign(gtk::Align::Center)
        .build();
    let act = act.clone();
    check.connect_toggled(move |c| act(Action::SetDone(item_id, c.is_active())));
    check
}

/// A green check for a goal met today; invisible but still taking its place when not.
pub fn done_mark(done: bool) -> gtk::Image {
    let mark = gtk::Image::from_icon_name("object-select-symbolic");
    mark.add_css_class("success");
    mark.set_opacity(if done { 1.0 } else { 0.0 });
    mark.set_tooltip_text(done.then_some("Done for today"));
    mark
}

pub fn progress(fraction: f64, width: i32) -> gtk::ProgressBar {
    gtk::ProgressBar::builder()
        .fraction(fraction.clamp(0.0, 1.0))
        .width_request(width)
        .valign(gtk::Align::Center)
        .build()
}

/// Wraps text in Unicode isolates so an Arabic course name can't reorder the numbers next to it.
pub fn isolate(text: &str) -> String {
    format!("\u{2068}{text}\u{2069}")
}

pub fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}
