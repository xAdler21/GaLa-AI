use serde::{Deserialize, Serialize};

/// Achsenparalleles Rechteck in PDF-Punkten, Ursprung oben links.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    /// Erzeugt ein Rechteck; vertauschte Ecken werden normalisiert.
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    pub fn center_y(&self) -> f32 {
        (self.y0 + self.y1) / 2.0
    }

    pub fn union(&self, other: &Rect) -> Rect {
        Rect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// Länge der Überlappung auf der y-Achse (0, wenn keine).
    pub fn vertical_overlap(&self, other: &Rect) -> f32 {
        (self.y1.min(other.y1) - self.y0.max(other.y0)).max(0.0)
    }

    /// Vertikale Überlappung relativ zur kleineren Höhe, 0..=1.
    pub fn vertical_overlap_ratio(&self, other: &Rect) -> f32 {
        let min_h = self.height().min(other.height());
        if min_h <= f32::EPSILON {
            return 0.0;
        }
        self.vertical_overlap(other) / min_h
    }

    /// Skaliert alle Koordinaten, z. B. Pixel → Punkte mit `72.0 / dpi`.
    pub fn scale(&self, factor: f32) -> Rect {
        Rect {
            x0: self.x0 * factor,
            y0: self.y0 * factor,
            x1: self.x1 * factor,
            y1: self.y1 * factor,
        }
    }
}

/// Fundstelle eines Werts: Seite (0-basiert) und Bereich auf der Seite.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SourceRef {
    pub page: u32,
    pub bbox: Rect,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_normalizes_corners() {
        let r = Rect::new(10.0, 20.0, 0.0, 5.0);
        assert_eq!(
            r,
            Rect {
                x0: 0.0,
                y0: 5.0,
                x1: 10.0,
                y1: 20.0
            }
        );
    }

    #[test]
    fn overlap_ratio_uses_smaller_height() {
        let tall = Rect::new(0.0, 0.0, 10.0, 20.0);
        let small = Rect::new(0.0, 15.0, 10.0, 25.0);
        assert_eq!(tall.vertical_overlap(&small), 5.0);
        assert_eq!(tall.vertical_overlap_ratio(&small), 0.5);
        assert_eq!(
            tall.vertical_overlap_ratio(&Rect::new(0.0, 30.0, 1.0, 40.0)),
            0.0
        );
    }

    #[test]
    fn scale_converts_pixels_to_points() {
        let px = Rect::new(300.0, 600.0, 600.0, 900.0);
        assert_eq!(px.scale(72.0 / 300.0), Rect::new(72.0, 144.0, 144.0, 216.0));
    }
}
