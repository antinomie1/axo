//! The settings block, laid out in `ui/settings.aegle`.

use std::{cell::RefCell, rc::Rc};

use aegle::{
    prelude::*,
    ui::{InputKind, PointerKind},
    widgets::Label,
};
use axo_render::CursorShape;

use super::{Block, next_id};
use crate::{
    config::{Config, scheme_names},
    shared::{Msg, SharedRef},
};

/// What a blank shell field starts, as the label above it says.
#[cfg(windows)]
const SHELL_HINT: &str = "Shell (default: powershell.exe; applies to new terminals)";
#[cfg(unix)]
const SHELL_HINT: &str = "Shell (empty: $SHELL; applies to new terminals)";

pub struct Settings {
    id: u64,
    root: Node,
    shared: SharedRef,
    font_family: TextField,
    font_size: NumberField,
    line_height: NumberField,
    theme: Dropdown,
    /// Color overrides of the loaded file; the settings keep them as they are,
    /// and a reset to defaults clears them.
    colors: RefCell<Vec<(String, String)>>,
    tab_bar: Switch,
    opacity: Slider,
    opacity_label: Label,
    padding: NumberField,
    cursor: Dropdown,
    scrollback: NumberField,
    shell: TextField,
    status: Label,
}

impl Settings {
    /// Builds the block in `parent`; `apply` receives every edited configuration.
    pub fn new(
        parent: &Container,
        shared: SharedRef,
        apply: Rc<dyn Fn(Config)>,
    ) -> Result<Rc<Self>> {
        let view = aegle::ui!(parent, "ui/settings.aegle")?;
        let root: Node = (*view.root).clone();
        root.set_grow(1.0);
        root.set_basis(0.0);
        root.set_min_width(0.0);
        root.set_min_height(0.0);
        view.shell_label.set_text(SHELL_HINT);
        let settings = Rc::new(Self {
            id: next_id(),
            root,
            shared,
            font_family: view.font_family.clone(),
            font_size: view.font_size.clone(),
            line_height: view.line_height.clone(),
            theme: view.theme.clone(),
            colors: RefCell::new(Vec::new()),
            tab_bar: view.tab_bar.clone(),
            opacity: view.opacity.clone(),
            opacity_label: view.opacity_label.clone(),
            padding: view.padding.clone(),
            cursor: view.cursor.clone(),
            scrollback: view.scrollback.clone(),
            shell: view.shell.clone(),
            status: view.status.clone(),
        });
        settings.fill(&settings.shared.config.borrow());

        let menu = crate::menu::BlockMenu::new(&settings.root, settings.id, &settings.shared, None);
        settings.root.on_context_menu(move |_, at| menu.show_at(at));

        let changed = {
            let settings = Rc::downgrade(&settings);
            Rc::new(move || {
                if let Some(settings) = settings.upgrade() {
                    let config = settings.read();
                    settings.update_label(&config);
                    apply(config);
                }
            })
        };
        // Text applies on Enter, so half-typed font names never load.
        let on = changed.clone();
        view.font_family.on_submit(move |_| on());
        let on = changed.clone();
        view.shell.on_submit(move |_| on());
        for field in [
            &view.font_size,
            &view.line_height,
            &view.padding,
            &view.scrollback,
        ] {
            let on = changed.clone();
            field.on_change(move |_| on());
        }
        for dropdown in [&view.theme, &view.cursor] {
            let on = changed.clone();
            dropdown.on_change(move |_| on());
        }
        let on = changed.clone();
        view.tab_bar.on_change(move |_| on());
        let on = changed.clone();
        view.opacity.on_change(move |_| on());

        let weak = Rc::downgrade(&settings);
        let on = changed.clone();
        view.defaults.on_click(move |_| {
            if let Some(settings) = weak.upgrade() {
                settings.fill(&Config::default());
                on();
            }
        });
        let weak = Rc::downgrade(&settings);
        view.save.on_click(move |_| {
            let Some(settings) = weak.upgrade() else {
                return;
            };
            changed();
            let message = match settings.shared.config.borrow().save() {
                Ok(path) => format!("Saved to {}", path.display()),
                Err(error) => format!("Could not save: {error}"),
            };
            settings.status.set_text(&message);
        });
        let (shared, id) = (settings.shared.clone(), settings.id);
        settings
            .root
            .on_input_capture(move |_, event: &mut aegle::ui::InputEvent<'_>| {
                let focusing = match event.kind {
                    InputKind::Pointer {
                        kind: PointerKind::Down { .. },
                        ..
                    } => true,
                    InputKind::Key(key) => key.pressed,
                    _ => false,
                };
                if focusing && shared.focused.get() != id {
                    shared.focused.set(id);
                    if let Some(proxy) = shared.proxy.get() {
                        let _ = proxy.send(Msg::Focus(id));
                    }
                }
            });
        Ok(settings)
    }

    fn fill(&self, config: &Config) {
        self.font_family.set_text(&config.font_family);
        self.font_size.set_value(config.font_size as f64);
        self.line_height.set_value(config.line_height as f64);
        self.fill_theme(&config.theme);
        self.colors.replace(config.colors.clone());
        self.tab_bar.set_checked(config.tab_bar);
        self.opacity.set_value(config.opacity as f64);
        self.padding.set_value(config.padding as f64);
        self.cursor.set_selected(match config.cursor {
            CursorShape::Block => 0,
            CursorShape::Bar => 1,
            CursorShape::Underline => 2,
        });
        self.scrollback.set_value(config.scrollback as f64);
        self.shell.set_text(&config.shell);
        self.update_label(config);
    }

    /// Offers every bundled scheme and every `themes/*.conf`, with the current
    /// one selected; an unknown name (say, a scheme file that went away) stays
    /// in the list so the choice is not silently replaced.
    fn fill_theme(&self, theme: &str) {
        let mut names = scheme_names();
        match names.iter().position(|name| name == theme) {
            Some(_) => {}
            None => names.insert(0, theme.to_string()),
        }
        let items: Vec<&str> = names.iter().map(String::as_str).collect();
        let selected = names.iter().position(|name| name == theme).unwrap_or(0);
        self.theme.set_items(&items, selected);
    }

    fn read(&self) -> Config {
        Config {
            font_family: match self.font_family.text().trim() {
                "" => "monospace".into(),
                family => family.into(),
            },
            font_size: self.font_size.value() as f32,
            line_height: self.line_height.value() as f32,
            theme: self.theme.text(),
            colors: self.colors.borrow().clone(),
            tab_bar: self.tab_bar.is_checked(),
            opacity: self.opacity.value() as f32,
            padding: self.padding.value() as f32,
            cursor: match self.cursor.selected() {
                1 => CursorShape::Bar,
                2 => CursorShape::Underline,
                _ => CursorShape::Block,
            },
            scrollback: self.scrollback.value() as usize,
            shell: self.shell.text().trim().into(),
        }
    }

    fn update_label(&self, config: &Config) {
        let percent = (config.opacity * 100.0).round();
        let text = match self.shared.transparent || config.opacity >= 1.0 {
            true => format!("Window opacity: {percent}%"),
            false => {
                format!("Window opacity: {percent}% (save and restart to enable transparency)")
            }
        };
        self.opacity_label.set_text(&text);
    }
}

impl Block for Settings {
    fn id(&self) -> u64 {
        self.id
    }
    fn node(&self) -> Node {
        self.root.clone()
    }
    fn focus(&self) {
        self.font_family.focus();
    }
    fn title(&self) -> String {
        "Settings".into()
    }
    fn apply(&self, config: &Config) {
        if (self.font_size.value() as f32 - config.font_size).abs() > 0.01 {
            self.font_size.set_value(config.font_size as f64);
        }
        if self.tab_bar.is_checked() != config.tab_bar {
            self.tab_bar.set_checked(config.tab_bar);
        }
        self.update_label(config);
    }
}
