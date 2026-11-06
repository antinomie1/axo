use std::{cell::RefCell, rc::Rc};

use aegle_glyph::{CacheLimits, GlyphCache};
use axo_render::{
    FontSpec,
    glyphs::{GlyphKey, Glyphs},
};

#[test]
fn rasterizes_text_symbols_and_fallback() {
    let mut glyphs = Glyphs::new();
    let cell = glyphs
        .set_font(
            &FontSpec {
                family: "monospace".into(),
                size: 15.0,
                line_height: 1.0,
            },
            1.0,
        )
        .unwrap();
    assert!(cell.width > 0 && cell.height > cell.width);
    for ch in ['M', '─', '╬', '█', '中'] {
        let bitmap = glyphs.rasterize(GlyphKey {
            ch,
            bold: false,
            italic: false,
        });
        let info = bitmap
            .as_ref()
            .map(|b| (b.width, b.height, b.left, b.top, b.color));
        eprintln!("{ch:?}: {info:?} cell {cell:?}");
        assert!(bitmap.is_some(), "{ch:?} has no bitmap");
        if "─╬█".contains(ch) {
            // Drawn to the cell, so neighbors join.
            let bitmap = bitmap.unwrap();
            assert_eq!((bitmap.width, bitmap.height), (cell.width, cell.height));
        }
    }
}

#[test]
fn a_shared_cache_is_not_duplicated_by_a_second_owner() {
    let cache = Rc::new(RefCell::new(
        GlyphCache::with_limits(CacheLimits::default()),
    ));
    let font = FontSpec {
        family: "monospace".into(),
        size: 15.0,
        line_height: 1.0,
    };
    let mut terminal = Glyphs::new();
    terminal.share(cache.clone());
    terminal.set_font(&font, 1.0).unwrap();
    let mut host = Glyphs::new();
    host.share(cache.clone());
    host.set_font(&font, 1.0).unwrap();
    let key = GlyphKey {
        ch: '中',
        bold: false,
        italic: false,
    };
    assert!(terminal.rasterize(key).is_some());
    let after_first = cache.borrow().stats().entries;
    // The second owner reuses the rasterized pixels: no new entry, same size.
    assert!(host.rasterize(key).is_some());
    assert_eq!(cache.borrow().stats().entries, after_first);
    // Changing the font of a shared cache leaves the host's glyphs in place.
    let mut size = font.clone();
    size.size = 17.0;
    terminal.set_font(&size, 1.0).unwrap();
    assert_eq!(cache.borrow().stats().entries, after_first);
}
