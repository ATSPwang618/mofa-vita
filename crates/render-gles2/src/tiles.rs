//! Adjacent source tiles for direct bilinear sampling of compact logical images.
use crate::{
    Gpu, Image, Result,
    drawing::{Draw, rect},
    image::{Plane, Tile},
    shader::Program,
};
use krkr_protocol::graphics::DrawFace;

impl Plane {
    fn regular_grid(&self) -> bool {
        let first = self.tiles[0].rectangle;
        let columns = self.size.width.div_ceil(first.width);
        let rows = self.size.height.div_ceil(first.height);
        if u64::from(columns) * u64::from(rows) == self.tiles.len() as u64
            && self.tiles.iter().enumerate().all(|(index, tile)| {
                let x = index as u32 % columns * first.width;
                let y = index as u32 / columns * first.height;
                tile.rectangle
                    == krkr_protocol::graphics::Rect {
                        left: x as i32,
                        top: y as i32,
                        width: first.width.min(self.size.width - x),
                        height: first.height.min(self.size.height - y),
                    }
            })
        {
            return true;
        }
        let columns = self
            .tiles
            .iter()
            .take_while(|t| t.rectangle.top == 0)
            .count();
        if columns == 0 || !self.tiles.len().is_multiple_of(columns) {
            return false;
        }
        let mut y = 0;
        for row in self.tiles.chunks_exact(columns) {
            let height = row[0].rectangle.height;
            let mut x = 0;
            for (tile, first) in row.iter().zip(&self.tiles[..columns]) {
                let r = tile.rectangle;
                if r.left != x
                    || r.top != y
                    || r.height != height
                    || r.width != first.rectangle.width
                {
                    return false;
                }
                x += r.width as i32;
            }
            if x != self.size.width as i32 {
                return false;
            }
            y += height as i32;
        }
        y == self.size.height as i32
    }
    fn tile_at(&self, x: u32, y: u32) -> &Tile {
        let first = self.tiles[0].rectangle;
        let columns = self.size.width.div_ceil(first.width);
        let contains = |t: &&Tile| {
            let r = t.rectangle;
            x >= r.left as u32
                && x < r.left as u32 + r.width
                && y >= r.top as u32
                && y < r.top as u32 + r.height
        };
        self.tiles
            .get((y / first.height * columns + x / first.width) as usize)
            .filter(contains)
            .or_else(|| self.tiles.iter().find(contains))
            .expect("validated neighbour grid covers the source")
    }
}
impl Gpu {
    /// Converted assets are no larger than their logical images. An unusual
    /// oversized upload is reduced once on GPU, so a logical bilinear footprint
    /// still crosses at most one texture boundary per axis.
    pub(crate) fn compact_source(&self, source: &Image) -> Result<Option<Image>> {
        let plane = source.plane(false)?;
        let stored = plane.size;
        if stored.width <= source.size.width && stored.height <= source.size.height {
            if plane.regular_grid() {
                return Ok(None);
            }
            // Four-neighbour kernels require aligned tile rows/columns. Retile
            // only this uncommon consumer; ordinary drawing directly samples
            // the shared-margin layout without an intermediate surface.
            let mut next = self.create_surface_image(stored)?;
            self.draw(
                next.plane(false)?,
                Some(plane),
                stored.rect(),
                &Draw::copy([1., 0., 0., 0., 1., 0.], [true; 4]),
            )?;
            next.size = source.size;
            next.canvas = source.canvas;
            return Ok(Some(next));
        }
        let mut next = self.create_surface_image(source.size)?;
        self.copy_rect(
            &mut next,
            source,
            source.size.rect(),
            0,
            0,
            source.size.rect(),
            DrawFace::Alpha,
            false,
        )?;
        Ok(Some(next))
    }
    pub(crate) fn bind_neighbours(
        &self,
        program: &Program,
        plane: &Plane,
        tile: &Tile,
    ) -> Result<()> {
        let r = tile.rectangle;
        let right = (r.left as u32 + r.width).min(plane.size.width - 1);
        let down = (r.top as u32 + r.height).min(plane.size.height - 1);
        for (unit, name, backing, input) in [
            (0, "u_source_rect", "u_source_backing", tile),
            (
                1,
                "u_right_rect",
                "u_right_backing",
                plane.tile_at(right, r.top as u32),
            ),
            (
                2,
                "u_down_rect",
                "u_down_backing",
                plane.tile_at(r.left as u32, down),
            ),
            (
                3,
                "u_diagonal_rect",
                "u_diagonal_backing",
                plane.tile_at(right, down),
            ),
        ] {
            self.bind_texture(unit, &input.texture)?;
            program.four(name, rect(input.rectangle));
            program.four(backing, rect(input.sample_rectangle()));
        }
        Ok(())
    }
}
