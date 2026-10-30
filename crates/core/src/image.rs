//! Raw pixel buffer shared between icon loading and rendering.

/// Premultiplied BGRA, row-major, tightly packed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BgraImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl BgraImage {
    /// Fully transparent image.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
        }
    }

    pub fn is_valid(&self) -> bool {
        self.pixels.len() == self.width as usize * self.height as usize * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_and_len() {
        let i = BgraImage::new(2, 3);
        assert_eq!(i.pixels.len(), 24);
        assert!(i.is_valid());
        let bad = BgraImage {
            width: 2,
            height: 2,
            pixels: vec![0; 3],
        };
        assert!(!bad.is_valid());
    }
}
