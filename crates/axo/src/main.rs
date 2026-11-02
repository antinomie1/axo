//! Axo Terminal: tabs of split blocks on aegle; terminals draw through the
//! platform's GPU backend, which is Vulkan on Linux and Windows.
//!
//! macOS compiles the same blocks and composition path, but aegle has no GPU
//! backend there yet, so the binary reports that instead of opening a window.
#![cfg_attr(target_os = "macos", allow(dead_code))]
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

use aegle::prelude::*;

#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
};

#[cfg(any(target_os = "linux", target_os = "windows"))]
use aegle::app::VulkanOptions;

#[cfg(any(target_os = "linux", target_os = "windows"))]
use axo_render::{Renderer, vulkan::VulkanRenderer};

#[cfg(any(target_os = "linux", target_os = "windows"))]
use crate::{
    config::Config,
    shared::{Msg, Shared},
    workspace::Workspace,
};

/// The terminal needs a renderer: the same code runs on every platform aegle
/// has a GPU backend for, and the missing one says so instead of guessing.
#[cfg(target_os = "macos")]
fn main() -> Result<()> {
    Err("axo: no GPU renderer on macOS yet; the window, its text input and the \
         composition path compile, but nothing can draw the terminal"
        .into())
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn main() -> Result<()> {
    let config = Config::load();
    // Wayland always hands out an alpha surface; elsewhere premultiplied alpha
    // is only worth it when the window is translucent.
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
