pub struct PerFrame<T> {
    frames: Vec<T>,
    curr_frame: u16,
}

impl<T> PerFrame<T>
where
    T: Clone + Default,
{
    pub fn new_with_clone(num_frames: usize, cpy: T) -> Self {
        Self {
            frames: vec![cpy; num_frames],
            curr_frame: 0,
        }
    }

    pub fn new(num_frames: usize) -> Self {
        Self {
            frames: vec![Default::default(); num_frames],
            curr_frame: 0,
        }
    }

    pub fn curr(&self) -> &T {
        &self.frames[self.curr_frame as usize]
    }

    pub fn curr_mut(&mut self) -> &mut T {
        &mut self.frames[self.curr_frame as usize]
    }

    pub fn curr_idx(&self) -> usize {
        self.curr_frame as usize
    }

    pub fn advance_to_frame(&mut self, frame_idx: usize) {
        if frame_idx < self.frames.len() {
            self.curr_frame = frame_idx as u16;
        }
    }

    pub fn advance_next_frame(&mut self) {
        self.curr_frame = (self.curr_frame + 1) % self.frames.len() as u16;
    }

    pub fn for_each<F>(&self, func: F)
    where
        F: Fn(&T),
    {
        for i in &self.frames {
            func(i);
        }
    }

    pub fn for_each_mut<F>(&mut self, mut func: F)
    where
        F: FnMut(&mut T),
    {
        for i in &mut self.frames {
            func(i);
        }
    }
}
