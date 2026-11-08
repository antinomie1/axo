//! Axo Terminal: tabs of split blocks on aegle; terminals draw through the
//! platform's GPU backend: Vulkan on Linux and Windows, wgpu (Metal) on macOS.
// A GUI program: Windows opens no console window for it. Its shells get their
// consoles from ConPTY.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod blocks;
mod chrome;
mod config;
mod keys;
mod layout;
mod menu;
mod shared;
mod workspace;

use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
};

use aegle::prelude::*;
use axo_render::Renderer;

use crate::{
    config::Config,
    shared::{Msg, Shared},
    workspace::Workspace,
};

fn main() -> Result<()> {
    let config = Config::load();
    // Wayland always hands out an alpha surface, and a non-opaque macOS window
    // costs only the compositor's blend; on Windows premultiplied alpha takes
    // DirectComposition, so it is only worth it when the window is translucent.
    let transparent = !cfg!(windows) || config.opacity < 1.0;
    let app = App::with_options(AppOptions {
        app_id: "dev.axo.Terminal".into(),
        theme: config.theme(transparent),
        dark_theme: None,
        high_contrast_theme: None,
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        vulkan: aegle::app::VulkanOptions {
            memory_budget: 96 * 1024 * 1024,
            transparent,
            ..Default::default()
        },
        #[cfg(target_os = "macos")]
        wgpu: aegle::app::WgpuOptions {
            transparent,
            ..Default::default()
        },
        ..AppOptions::default()
    })?;
    let view = aegle::ui!(&app, "ui/window.aegle")?;

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let mut renderer = axo_render::vulkan::VulkanRenderer::new(
        app.vulkan().ok_or("the window has no Vulkan device")?,
    )?;
    #[cfg(target_os = "macos")]
    let mut renderer =
        axo_render::wgpu::WgpuRenderer::new(app.wgpu().ok_or("the window has no wgpu device")?);
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
