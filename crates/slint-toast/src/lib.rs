//! Toasts for Slint apps.
//!
//! [`Toasts`] keeps what the stack shows; `ui/toast.slint` draws it. A busy toast ("Checking for
//! updates...") shows a running bar and turns into its result with [`Toasts::finish`]. Info and
//! success go after a few seconds, a warning or an error stays until it is closed.
//!
//! In the app:
//! - `build.rs`: `slint_build::compile_with_config("ui/app.slint", slint_build::CompilerConfiguration::new()
//!   .with_library_paths([("toast".into(), slint_toast::UI_DIR.into())].into()))`, with this crate as a
//!   build dependency without features.
//! - Slint: `import { Toast, ToastStack } from "@toast/toast.slint";`, export `Toast`, place a
//!   `ToastStack` and forward its `dismiss(id)` to [`Toasts::dismiss_by_user`] (or to
//!   [`Toasts::dismiss`] with `busy-closable: true`, so running work can be closed too).
//! - Rust (feature `slint`): keep one `VecModel` of the generated `Toast` and call [`sync`] after
//!   each change.

/// The folder with `toast.slint`, for `with_library_paths`.
pub const UI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/ui");

/// What a toast says about its work. The numbers are the `kind` of the Slint `Toast`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Info = 0,
    Success = 1,
    Warning = 2,
    Error = 3,
    Busy = 4,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub id: i32,
    pub text: String,
    pub kind: Kind,
    /// A button in the toast ("Undo"), empty without one. The stack's `action(id)` reports a click.
    pub action: String,
}

/// The toasts on screen, oldest first.
#[derive(Debug)]
pub struct Toasts {
    items: Vec<Toast>,
    next: i32,
    limit: usize,
}

impl Default for Toasts {
    fn default() -> Self {
        Toasts::new(3)
    }
}

impl Toasts {
    /// At most `limit` toasts show; a new one pushes out the oldest that is not busy.
    pub fn new(limit: usize) -> Self {
        Toasts { items: Vec::new(), next: 0, limit: limit.max(1) }
    }

    pub fn items(&self) -> &[Toast] {
        &self.items
    }

    pub fn get(&self, id: i32) -> Option<&Toast> {
        self.items.iter().find(|t| t.id == id)
    }

    /// Shows `text`; the same text of the same kind shows once and keeps its id.
    pub fn push(&mut self, text: impl Into<String>, kind: Kind) -> i32 {
        let text = text.into();
        if let Some(t) = self.items.iter().find(|t| t.text == text && t.kind == kind) {
            return t.id;
        }
        self.next += 1;
        self.items.push(Toast { id: self.next, text, kind, action: String::new() });
        while self.items.len() > self.limit {
            let oldest = self.items.iter().position(|t| t.kind != Kind::Busy).unwrap_or(0);
            self.items.remove(oldest);
        }
        self.next
    }

    /// A toast with a button (`action`, "Undo"); it stays longer than one without, so there is time
    /// to press it. Always a new toast, so each one's button acts on its own thing.
    pub fn with_action(&mut self, text: impl Into<String>, kind: Kind, action: impl Into<String>) -> i32 {
        self.next += 1;
        self.items.push(Toast { id: self.next, text: text.into(), kind, action: action.into() });
        while self.items.len() > self.limit {
            let oldest = self.items.iter().position(|t| t.kind != Kind::Busy).unwrap_or(0);
            self.items.remove(oldest);
        }
        self.next
    }

    pub fn busy(&mut self, text: impl Into<String>) -> i32 {
        self.push(text, Kind::Busy)
    }

    pub fn info(&mut self, text: impl Into<String>) -> i32 {
        self.push(text, Kind::Info)
    }

    pub fn success(&mut self, text: impl Into<String>) -> i32 {
        self.push(text, Kind::Success)
    }

    pub fn warning(&mut self, text: impl Into<String>) -> i32 {
        self.push(text, Kind::Warning)
    }

    pub fn error(&mut self, text: impl Into<String>) -> i32 {
        self.push(text, Kind::Error)
    }

    /// New text for a running toast ("Updating 2 of 5"); nothing when it was closed.
    pub fn set_text(&mut self, id: i32, text: impl Into<String>) {
        if let Some(t) = self.items.iter_mut().find(|t| t.id == id) {
            t.text = text.into();
        }
    }

    /// Turns the busy toast `id` into its result. Closed meanwhile, the result comes as a new toast.
    pub fn finish(&mut self, id: i32, text: impl Into<String>, kind: Kind) -> i32 {
        let text = text.into();
        match self.items.iter_mut().find(|t| t.id == id) {
            Some(t) => {
                t.text = text;
                t.kind = kind;
                id
            }
            None => self.push(text, kind),
        }
    }

    /// Closes a toast: the end of an info or success toast's time, or the app.
    pub fn dismiss(&mut self, id: i32) {
        self.items.retain(|t| t.id != id);
    }

    /// Closes a toast for its X: running work stays until it has a result.
    pub fn dismiss_by_user(&mut self, id: i32) {
        self.items.retain(|t| t.id != id || t.kind == Kind::Busy);
    }
}

/// Brings `model` to `rows` (one per toast, in [`Toasts::items`] order) row by row: a toast that
/// stays keeps its element, so its timer does not start again. `id` reads a row's toast id.
#[cfg(feature = "slint")]
pub fn sync<T: Clone + 'static>(model: &slint::VecModel<T>, rows: Vec<T>, id: impl Fn(&T) -> i32) {
    use slint::Model;
    let ids: Vec<i32> = rows.iter().map(&id).collect();
    let mut i = 0;
    while let Some(row) = model.row_data(i) {
        if ids.contains(&id(&row)) {
            i += 1;
        } else {
            model.remove(i);
        }
    }
    // What is left is in order (ids only grow), so each row is the one shown there or new.
    for (i, row) in rows.into_iter().enumerate() {
        match model.row_data(i) {
            Some(old) if id(&old) == id(&row) => model.set_row_data(i, row),
            _ => model.insert(i, row),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(toasts: &Toasts) -> Vec<&str> {
        toasts.items().iter().map(|t| t.text.as_str()).collect()
    }

    #[test]
    fn a_busy_toast_turns_into_its_result() {
        let mut toasts = Toasts::default();
        let id = toasts.busy("Checking for updates...");
        assert_eq!(toasts.get(id).unwrap().kind, Kind::Busy);
        toasts.set_text(id, "Checking 2 of 5...");
        assert_eq!(toasts.finish(id, "All addons are up to date", Kind::Success), id);
        assert_eq!(toasts.items(), [Toast { id, text: "All addons are up to date".into(), kind: Kind::Success, action: String::new() }]);
    }

    #[test]
    fn a_result_of_a_closed_toast_comes_as_a_new_one() {
        let mut toasts = Toasts::default();
        let id = toasts.busy("Installing Lucky...");
        toasts.dismiss(id);
        let new = toasts.finish(id, "Could not install Lucky", Kind::Error);
        assert_ne!(new, id);
        assert_eq!(texts(&toasts), ["Could not install Lucky"]);
    }

    #[test]
    fn the_same_message_shows_once() {
        let mut toasts = Toasts::default();
        let id = toasts.error("\"abc\" is not a web address");
        assert_eq!(toasts.error("\"abc\" is not a web address"), id);
        assert_ne!(toasts.warning("\"abc\" is not a web address"), id, "another kind is another toast");
        toasts.info("Opening the browser...");
        assert_eq!(toasts.items().iter().map(|t| t.kind).collect::<Vec<_>>(), [Kind::Error, Kind::Warning, Kind::Info]);
    }

    #[test]
    fn the_oldest_finished_toast_makes_room_but_running_work_stays() {
        let mut toasts = Toasts::new(3);
        toasts.busy("Checking for updates...");
        for n in 0..4 {
            toasts.success(format!("Done {n}"));
        }
        assert_eq!(texts(&toasts), ["Checking for updates...", "Done 2", "Done 3"]);
    }

    #[test]
    fn a_toast_can_carry_a_button() {
        let mut toasts = Toasts::default();
        let first = toasts.with_action("Lucky removed", Kind::Success, "Undo");
        let second = toasts.with_action("Lucky removed", Kind::Success, "Undo");
        assert_ne!(first, second, "each removal its own button");
        assert_eq!(toasts.get(first).unwrap().action, "Undo");
        let info = toasts.info("Opening the browser...");
        assert_eq!(toasts.get(info).unwrap().action, "");
    }

    #[test]
    fn the_user_cannot_close_running_work() {
        let mut toasts = Toasts::default();
        let busy = toasts.busy("Checking for updates...");
        let info = toasts.info("Opening the browser...");
        toasts.dismiss_by_user(busy);
        toasts.dismiss_by_user(info);
        assert_eq!(texts(&toasts), ["Checking for updates..."]);
        toasts.finish(busy, "All addons are up to date", Kind::Success);
        toasts.dismiss_by_user(busy);
        assert!(toasts.items().is_empty(), "its result can be closed");
    }

    #[cfg(feature = "slint")]
    #[test]
    fn sync_keeps_the_rows_of_toasts_that_stay() {
        use slint::Model;
        let model = slint::VecModel::from(vec![(1, "a"), (2, "b"), (3, "c")]);
        sync(&model, vec![(1, "a"), (3, "c done"), (4, "d")], |r| r.0);
        assert_eq!(model.iter().collect::<Vec<_>>(), [(1, "a"), (3, "c done"), (4, "d")]);
    }
}
