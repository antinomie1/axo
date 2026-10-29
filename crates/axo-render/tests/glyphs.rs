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
