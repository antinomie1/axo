//! Images on a screen: what the kitty and sixel protocols have in common.
//!
//! Pixels are shared immutably, so a renderer can hold on to them by reference
//! count, and a [`Placement`] says where some of them show. A placement is
//! anchored to an absolute line number (see [`crate::Grid::base`]) that survives
//! scrolling into history without being touched, so text pouring past an image
//! costs nothing. Only a scroll that rotates lines inside the screen has to walk
//! the list.
//!
//! The two protocols differ in how text treats an image. A kitty image is a
//! layer, which text ignores. A sixel is screen content, so text written over it
//! replaces the cells it lands on: [`Images::punch`] splits the placement into
//! crops around them, which needs no support from the renderer.

use std::sync::Arc;

/// Pixel bytes one screen keeps, and the largest image: kitty's default quota.
pub(crate) const MAX_BYTES: usize = 320 << 20;
/// The longest image side, in pixels.
pub(crate) const MAX_SIDE: usize = 10_000;

/// Decoded image data.
#[derive(Debug)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    /// Straight-alpha sRGB, row by row without padding.
    pub rgba: Vec<u8>,
}

impl Pixels {
    /// Whether an image of this size may be kept at all.
    pub(crate) fn fits(width: usize, height: usize) -> bool {
        (1..=MAX_SIDE).contains(&width)
            && (1..=MAX_SIDE).contains(&height)
            && width * height * 4 <= MAX_BYTES
    }
}

/// Part of an image shown at a position on the screen.
#[derive(Clone, Debug)]
pub struct Placement {
    pub pixels: Arc<Pixels>,
    /// The absolute line of the anchor cell.
    pub line: i64,
    /// The column of the anchor cell.
    pub col: usize,
    /// Pixels between the anchor cell's corner and the image.
    pub offset: [u32; 2],
    /// The source rectangle: x, y, width, height.
    pub crop: [u32; 4],
    /// Size in cells; zero derives it from the other side or the crop.
    pub cells: [u32; 2],
    /// Drawing order; see the layers in the kitty protocol. Sixels use 0.
    pub z: i32,
    /// The kitty image this shows, 0 when only placements own it.
    pub(crate) image: u32,
    /// The kitty placement id, 0 when anonymous.
    pub(crate) id: u32,
}

impl Placement {
    /// A placement of the whole image at its natural size.
    pub(crate) fn whole(pixels: Arc<Pixels>, line: i64, col: usize) -> Self {
        Self {
            crop: [0, 0, pixels.width, pixels.height],
            pixels,
            line,
            col,
            offset: [0; 2],
            cells: [0; 2],
            z: 0,
            image: 0,
            id: 0,
        }
    }

    /// The drawn size in pixels for a cell of `cell` pixels. A side given in
    /// cells follows the font; one given alone keeps the aspect ratio.
    pub fn size(&self, cell: [u32; 2]) -> [u32; 2] {
        let [_, _, width, height] = self.crop;
        // Bounded, because a client picks the ratio and the result drives how
        // far the cursor moves.
        let scale = |value: u32, to: u32, from: u32| {
            let scaled = u64::from(value) * u64::from(to) / u64::from(from).max(1);
            scaled.min(u16::MAX.into()) as u32
        };
        match self.cells {
            [0, 0] => [width, height],
            [cols, 0] => {
                let to = cols * cell[0];
                [to, scale(height, to, width)]
            }
            [0, rows] => {
                let to = rows * cell[1];
                [scale(width, to, height), to]
            }
            [cols, rows] => [cols * cell[0], rows * cell[1]],
        }
    }

    /// Columns and rows from the anchor cell to the far edge of the image.
    pub(crate) fn span(&self, cell: [u32; 2]) -> [usize; 2] {
        let [width, height] = self.size(cell);
        [
            (self.offset[0] + width).div_ceil(cell[0]) as usize,
            (self.offset[1] + height).div_ceil(cell[1]) as usize,
        ]
    }

    /// The first and last line the image touches. A sixel's crop may start
    /// well below its anchor.
    fn lines(&self, cell: [u32; 2]) -> (i64, i64) {
        let first = self.line + i64::from(self.offset[1] / cell[1]);
        (first, self.line + self.span(cell)[1] as i64 - 1)
    }

    fn columns(&self, cell: [u32; 2]) -> (usize, usize) {
        let first = self.col + (self.offset[0] / cell[0]) as usize;
        (first, self.col + self.span(cell)[0] - 1)
    }

    pub(crate) fn touches_line(&self, line: i64, cell: [u32; 2]) -> bool {
        let (first, last) = self.lines(cell);
        (first..=last).contains(&line)
    }

    pub(crate) fn touches_column(&self, col: usize, cell: [u32; 2]) -> bool {
        let (first, last) = self.columns(cell);
        (first..=last).contains(&col)
    }

    /// Whether anything of the image is at or below `line`.
    pub(crate) fn reaches(&self, line: i64, cell: [u32; 2]) -> bool {
        self.lines(cell).1 >= line
    }
}

/// An image a client can address by id.
struct Stored {
    id: u32,
    number: u32,
    pixels: Arc<Pixels>,
}

/// The images of one screen.
pub struct Images {
    /// Addressable kitty images, oldest first. Anonymous ones and sixels are
    /// owned by their placements alone and go when those do.
    stored: Vec<Stored>,
    /// Kitty placements, oldest first.
    placements: Vec<Placement>,
    /// Sixel placements, oldest first. Being separate, their absence is all
    /// that the text paths have to check.
    sixels: Vec<Placement>,
    /// The next id handed to an image that asks for one by number only.
    next_id: u32,
    /// Cell size in pixels, for converting between the two grids.
    cell: [u32; 2],
}

impl Default for Images {
    fn default() -> Self {
        Self {
            stored: Vec::new(),
            placements: Vec::new(),
            sixels: Vec::new(),
            // Far from the small ids clients pick for themselves.
            next_id: 1 << 31,
            cell: [10, 20],
        }
    }
}

impl Images {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.placements.is_empty() && self.sixels.is_empty()
    }

    /// Everything placed, kitty images before sixels, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &Placement> {
        self.placements.iter().chain(&self.sixels)
    }

    #[inline]
    pub(crate) fn has_sixels(&self) -> bool {
        !self.sixels.is_empty()
    }

    pub(crate) fn cell(&self) -> [u32; 2] {
        self.cell
    }

    pub(crate) fn set_cell(&mut self, cell: [u32; 2]) {
        self.cell = cell;
    }

    /// Forgets everything but the cell size.
    pub(crate) fn clear(&mut self) {
        *self = Self {
            cell: self.cell,
            ..Self::default()
        };
    }

    pub(crate) fn next_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1 << 31);
        id
    }

    /// Stores an image, replacing any with the same id along with what shows it.
    pub(crate) fn store(&mut self, id: u32, number: u32, pixels: Arc<Pixels>) {
        self.stored.retain(|stored| stored.id != id);
        self.placements.retain(|placement| placement.image != id);
        self.stored.push(Stored { id, number, pixels });
        self.trim();
    }

    pub(crate) fn stored(&self, id: u32) -> Option<&Arc<Pixels>> {
        let stored = self.stored.iter().find(|stored| stored.id == id)?;
        Some(&stored.pixels)
    }

    /// The id of the newest image with `number`.
    pub(crate) fn numbered(&self, number: u32) -> Option<u32> {
        let stored = self.stored.iter().rev().find(|s| s.number == number)?;
        Some(stored.id)
    }

    /// Adds a kitty placement; a non-zero id replaces its earlier self.
    pub(crate) fn place(&mut self, placement: Placement) {
        if placement.id != 0 {
            self.placements
                .retain(|old| (old.image, old.id) != (placement.image, placement.id));
        }
        self.placements.push(placement);
        self.trim();
    }

    /// Adds a sixel, which replaces the sixel cells it covers. That is also how
    /// an animation repainting one spot frees the frames before it.
    pub(crate) fn add_sixel(&mut self, placement: Placement) {
        let (first, last) = placement.lines(self.cell);
        let (left, right) = placement.columns(self.cell);
        for line in first..=last {
            self.punch(line, left, right + 1);
        }
        self.sixels.push(placement);
        self.trim();
    }

    /// Replaces the cells `from..to` of `line` in every sixel.
    pub(crate) fn punch(&mut self, line: i64, from: usize, to: usize) {
        let [cw, ch] = self.cell.map(i64::from);
        for i in (0..self.sixels.len()).rev() {
            let placement = &self.sixels[i];
            let [x, y, width, height] = placement.crop.map(i64::from);
            // The hole, in image pixels, clipped to this crop.
            let col = placement.col as i64;
            let top = (line - placement.line) * ch;
            let (hole_left, hole_right) = (
                ((from as i64 - col) * cw).max(x),
                ((to as i64 - col) * cw).min(x + width),
            );
            let (hole_top, hole_bottom) = (top.max(y), (top + ch).min(y + height));
            if hole_left >= hole_right || hole_top >= hole_bottom {
                continue;
            }
            let crop = |left: i64, top: i64, right: i64, bottom: i64| {
                (left < right && top < bottom).then(|| Placement {
                    crop: [left, top, right - left, bottom - top].map(|v| v as u32),
                    offset: [left, top].map(|v| v as u32),
                    ..placement.clone()
                })
            };
            let pieces = [
                crop(x, y, x + width, hole_top),
                crop(x, hole_top, hole_left, hole_bottom),
                crop(hole_right, hole_top, x + width, hole_bottom),
                crop(x, hole_bottom, x + width, y + height),
            ];
            self.sixels.splice(i..=i, pieces.into_iter().flatten());
        }
    }

    /// Moves what touches the lines `top..=bottom` with them, by `delta` lines.
    /// What left the region is dropped; an image only partly out of it keeps
    /// drawing whole until it leaves, rather than cropping every step.
    #[cold]
    pub(crate) fn scroll(&mut self, top: i64, bottom: i64, delta: i64) {
        let cell = self.cell;
        for list in [&mut self.placements, &mut self.sixels] {
            list.retain_mut(|placement| {
                let (first, last) = placement.lines(cell);
                if last < top || first > bottom {
                    return true;
                }
                placement.line += delta;
                last + delta >= top && first + delta <= bottom
            });
        }
    }

    /// Moves everything anchored at or below `line` by `delta` lines.
    #[cold]
    pub(crate) fn shift_below(&mut self, line: i64, delta: i64) {
        for list in [&mut self.placements, &mut self.sixels] {
            for placement in list.iter_mut().filter(|p| p.line >= line) {
                placement.line += delta;
            }
        }
    }

    /// Drops what lies entirely above `line`, the oldest line history holds.
    #[cold]
    pub(crate) fn prune(&mut self, line: i64) {
        let cell = self.cell;
        for list in [&mut self.placements, &mut self.sixels] {
            list.retain(|placement| placement.reaches(line, cell));
        }
    }

    /// Removes the kitty placements `doomed` picks. With `free`, stored images
    /// `orphan` picks go as well once nothing shows them.
    pub(crate) fn delete(
        &mut self,
        doomed: impl Fn(&Placement) -> bool,
        free: bool,
        orphan: impl Fn(u32) -> bool,
    ) {
        self.placements.retain(|placement| !doomed(placement));
        if free {
            let placements = &self.placements;
            self.stored.retain(|stored| {
                !orphan(stored.id) || placements.iter().any(|p| p.image == stored.id)
            });
        }
    }

    /// Pixel bytes held, counting a crop of an image only placements own as
    /// what it shows.
    fn bytes(&self) -> usize {
        let stored = self.stored.iter().map(|stored| stored.pixels.rgba.len());
        let shown = self
            .iter()
            .filter(|placement| placement.image == 0)
            .map(|placement| placement.crop[2] as usize * placement.crop[3] as usize * 4);
        stored.chain(shown).sum()
    }

    /// Evicts until the quota holds: oldest images nothing shows, then the
    /// oldest placements.
    fn trim(&mut self) {
        while self.bytes() > MAX_BYTES {
            let placed = |id| self.placements.iter().any(|p| p.image == id);
            if let Some(at) = self.stored.iter().position(|s| !placed(s.id)) {
                self.stored.remove(at);
            } else if !self.placements.is_empty() {
                self.placements.remove(0);
            } else {
                self.sixels.remove(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: [u32; 2] = [10, 20];

    fn sixel(width: u32, height: u32) -> Placement {
        let rgba = vec![0; (width * height * 4) as usize];
        let pixels = Arc::new(Pixels {
            width,
            height,
            rgba,
        });
        Placement::whole(pixels, 0, 0)
    }

    fn images(placement: Placement) -> Images {
        let mut images = Images::default();
        images.add_sixel(placement);
        images
    }

    #[test]
    fn sizes_follow_the_cells_given() {
        let mut placement = sixel(40, 30);
        assert_eq!(placement.size(CELL), [40, 30]);
        assert_eq!(placement.span(CELL), [4, 2]);
        placement.cells = [8, 0];
        assert_eq!(placement.size(CELL), [80, 60], "width given: same aspect");
        placement.cells = [0, 3];
        assert_eq!(placement.size(CELL), [80, 60], "height given: same aspect");
        placement.cells = [1, 1];
        assert_eq!(placement.size(CELL), [10, 20], "both given: stretched");
        placement.offset = [5, 5];
        assert_eq!(placement.span(CELL), [2, 2], "an offset pushes the edge");
    }

    #[test]
    fn punching_a_cell_leaves_the_crops_around_it() {
        let mut images = images(sixel(30, 60));
        images.punch(1, 1, 2);
        let mut crops: Vec<[u32; 4]> = images.sixels.iter().map(|p| p.crop).collect();
        crops.sort();
        assert_eq!(
            crops,
            [
                [0, 0, 30, 20],
                [0, 20, 10, 20],
                [0, 40, 30, 20],
                [20, 20, 10, 20]
            ]
        );
        // Each crop still draws where it did.
        assert!(
            images
                .sixels
                .iter()
                .all(|p| p.offset == [p.crop[0], p.crop[1]])
        );
        // A punch that hits nothing changes nothing.
        images.punch(1, 1, 2);
        assert_eq!(images.sixels.len(), 4);
        // Punching all of it leaves no placement behind.
        for line in 0..3 {
            images.punch(line, 0, 3);
        }
        assert!(images.is_empty());
    }

    #[test]
    fn a_new_sixel_frees_the_one_it_covers() {
        let mut images = images(sixel(30, 40));
        images.add_sixel(sixel(30, 40));
        assert_eq!(images.sixels.len(), 1);
        // A smaller one only takes the cells it covers.
        images.add_sixel(sixel(10, 20));
        assert_eq!(
            images.iter().map(|p| p.crop[2] * p.crop[3]).sum::<u32>(),
            1200
        );
    }

    #[test]
    fn scrolling_moves_what_it_touches_and_drops_what_left() {
        let mut images = images(sixel(10, 20));
        images.sixels[0].line = 5;
        images.scroll(0, 3, -1);
        assert_eq!(images.sixels[0].line, 5, "outside the region");
        images.scroll(0, 9, -2);
        assert_eq!(images.sixels[0].line, 3);
        images.scroll(0, 9, -4);
        assert!(images.is_empty(), "scrolled off the region");
    }

    #[test]
    fn the_quota_evicts_oldest_first() {
        let pixels = |bytes: usize| {
            Arc::new(Pixels {
                width: 1,
                height: 1,
                rgba: vec![0; bytes],
            })
        };
        let mut images = Images::default();
        images.store(1, 0, pixels(MAX_BYTES / 2));
        images.store(2, 0, pixels(MAX_BYTES / 2));
        images.store(3, 0, pixels(MAX_BYTES / 2));
        assert!(images.stored(1).is_none());
        assert!(images.stored(2).is_some() && images.stored(3).is_some());
        // Something shown outlives something that is not.
        images.place(Placement {
            image: 2,
            ..Placement::whole(pixels(0), 0, 0)
        });
        images.store(4, 0, pixels(MAX_BYTES / 2));
        assert!(images.stored(2).is_some() && images.stored(3).is_none());
    }
}
