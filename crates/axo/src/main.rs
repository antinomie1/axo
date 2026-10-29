//! Axo Terminal: tabs of split blocks on aegle; terminals render with Vulkan.

mod blocks;
mod config;
mod keys;
mod layout;
mod shared;
mod workspace;

use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
};

use aegle::{app::VulkanOptions, prelude::*};
use axo_render::{Renderer, vulkan::VulkanRenderer};

use crate::{
    config::Config,
    shared::{Msg, Shared},
    workspace::Workspace,
};

fn main() -> Result<()> {
    let config = Config::load();
    let transparent = cfg!(target_os = "linux") || config.opacity < 1.0;
    let app = App::with_options(AppOptions {
        app_id: "dev.axo.Terminal".into(),
        theme: config.theme(transparent),
        dark_theme: None,
        high_contrast_theme: None,
        vulkan: VulkanOptions {
            memory_budget: 96 * 1024 * 1024,
            transparent,
            ..VulkanOptions::default()
        },
        ..AppOptions::default()
    })?;
    let view = aegle::ui!(&app, "ui/window.aegle")?;

    let device = app.vulkan().ok_or("the window has no Vulkan device")?;
    let mut renderer = VulkanRenderer::new(device)?;
    let scale = view.root.scale().unwrap_or(1.0);
    renderer.set_font(&config.font(), scale)?;
    let shared = Rc::new(Shared {
        renderer: RefCell::new(Box::new(renderer) as Box<dyn Renderer>),
        palette: RefCell::new(config.palette()),
        config: RefCell::new(config),
        scale: Cell::new(scale),
        transparent,
        proxy: OnceCell::new(),
        focused: Cell::new(0),
    });

    let workspace = Workspace::new(
        view.root.clone(),
        view.bar.clone(),
        view.tabs.clone(),
        view.pages.clone(),
        shared.clone(),
    );
    let ws = workspace.clone();
    let proxy = app.proxy(move |msg: Msg| ws.handle(msg))?;
    let _ = shared.proxy.set(proxy);
    workspace::start(&workspace, &view.add, &view.settings)?;
    app.run()
}
