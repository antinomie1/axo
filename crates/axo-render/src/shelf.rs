//! Shelf packing of glyphs into a square atlas page, shared by the GPU backends.

/// Transparent texels between glyphs.
const GAP: u32 = 1;

/// Places `width × height` on a shelf-packed page of `side`² and returns its
/// origin. `shelf` is the next free x, the shelf's top and its height.
pub(crate) fn pack(
    shelf: &mut (u32, u32, u32),
    side: u32,
    width: u32,
    height: u32,
) -> Option<(u32, u32)> {
    let (w, h) = (width.checked_add(GAP)?, height.checked_add(GAP)?);
    if w > side || h > side {
        return None;
    }
    let (mut x, mut y, mut height_of_shelf) = *shelf;
    if x + w > side {
        (x, y, height_of_shelf) = (0, y + height_of_shelf, 0);
    }
    if y + h > side {
        return None;
    }
    *shelf = (x + w, y, height_of_shelf.max(h));
    Some((x, y))
}

#[cfg(test)]
mod tests {
    use super::pack;

    #[test]
    fn shelves_fill_a_page_and_report_overflow() {
        let (mut shelf, side) = ((0, 0, 0), 16);
        // Two 7×7 glyphs plus a texel gap each fit side by side on one shelf.
        assert_eq!(pack(&mut shelf, side, 7, 7), Some((0, 0)));
        assert_eq!(pack(&mut shelf, side, 7, 7), Some((8, 0)));
        assert_eq!(shelf, (16, 0, 8));
        // The next glyph starts a shelf below the tallest one.
        assert_eq!(pack(&mut shelf, side, 7, 7), Some((0, 8)));
        // A glyph taller than the page never fits, and one wider than the page
        // is rejected before a shelf is opened.
        assert_eq!(pack(&mut shelf, side, 7, 20), None);
        assert_eq!(pack(&mut shelf, side, 20, 7), None);
        // Filling the page leaves no room.
        let (mut shelf, side) = ((0, 0, 0), 8);
        assert_eq!(pack(&mut shelf, side, 3, 3), Some((0, 0)));
        assert_eq!(pack(&mut shelf, side, 3, 3), Some((4, 0)));
        assert_eq!(pack(&mut shelf, side, 3, 3), Some((0, 4)));
        assert_eq!(pack(&mut shelf, side, 3, 3), Some((4, 4)));
        assert_eq!(pack(&mut shelf, side, 3, 3), None);
    }
}
