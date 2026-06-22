#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub argb8888: Vec<u32>,
    rgb565: Vec<u16>,
}

impl Frame {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            argb8888: vec![0xff000000; (width * height) as usize],
            rgb565: Vec::new(),
        }
    }

    pub fn replace_argb8888(&mut self, width: u32, height: u32, argb8888: Vec<u32>) {
        self.width = width;
        self.height = height;
        self.argb8888 = argb8888;
        self.rgb565.clear();
    }

    pub fn rgb565(&mut self) -> &[u16] {
        if self.rgb565.len() != self.argb8888.len() {
            self.rgb565 = self
                .argb8888
                .iter()
                .map(|pixel| {
                    let r = ((pixel >> 16) & 0xff) as u16;
                    let g = ((pixel >> 8) & 0xff) as u16;
                    let b = (pixel & 0xff) as u16;

                    ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3)
                })
                .collect();
        }

        &self.rgb565
    }
}
