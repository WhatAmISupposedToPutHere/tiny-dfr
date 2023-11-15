use rand::Rng;
use std::{
    time::Instant,
};
use crate::TIMEOUT_MS;

const INTERVAL_MS: i32 = TIMEOUT_MS * 1; // should be a multiple of TIMEOUT_MS
const PROLONGED_INTERVAL_MS: i32 = TIMEOUT_MS * 5; // should be a multiple of TIMEOUT_MS and more than INTERVAL_MS
const ANIMATION_INTERVAL_MS: i32 = 200; // should be less than TIMEOUT_MS
const ANIMATION_DURATION_MS: i32 = 4000; // should be a multiple of ANIMATION_INTERVAL_MS

// This is the total range on the x-axis that pixels will shift by over time, ie. they will shift by
// PIXEL_SHIFT_WIDTH_PX / 2 to the right and to the left.
// To make sure that no pixel ends up being always on, the minimum value to be safe here is the
// size of the largest continuous colored line in the x-direction. The higher this value, the less
// strain is put on the panel.
pub const PIXEL_SHIFT_WIDTH_PX: u64 = 22; // should be divisible by 2
// in y direction we can't really shift by a lot since icons still need to appear centered,
// 2 pixels in each direction seems to be the maximum before it gets really visible.
const PIXEL_SHIFT_HEIGHT_PX: u64 = 4; // should be divisible by 2

#[derive(Clone, Copy)]
enum ShiftState {
    WaitingAtEnd,
    ShiftingSubpixel,
    Normal
}

pub struct PixelShiftManager {
    last_active: Instant,
    y_constant: f64,
    pixel_progress: u64,
    subpixel_progress: f64,
    direction: i64,
    state: ShiftState
}

fn wait_for_state(state: ShiftState) -> i32 {
    match state {
        ShiftState::ShiftingSubpixel => ANIMATION_INTERVAL_MS,
        ShiftState::Normal => INTERVAL_MS,
        ShiftState::WaitingAtEnd => PROLONGED_INTERVAL_MS,
    }
}

impl PixelShiftManager {
    pub fn new() -> PixelShiftManager {
        let pixel_progress = rand::thread_rng().gen_range(0..PIXEL_SHIFT_WIDTH_PX);

        // add some randomness to the relationship between shifting on the x and y axis
        // so that pixel shifting doesn't follow the same 2d pattern every time
        let y_constant: f64 = rand::thread_rng().gen_range(0..PIXEL_SHIFT_HEIGHT_PX * 2) as f64;

        PixelShiftManager {
            last_active: Instant::now(),
            y_constant,
            state: ShiftState::Normal,
            pixel_progress,
            subpixel_progress: 0.0,
            direction: 1,
        }
    }

    pub fn update(&mut self) -> (bool, i32) {
        let time_now = Instant::now();
        let since_last_pixel_shift = (time_now - self.last_active).as_millis() as i32;

        if since_last_pixel_shift < wait_for_state(self.state) {
            return (false, i32::MAX);
        }
        self.last_active = time_now;

        match self.state {
            ShiftState::Normal => {
                self.state = ShiftState::ShiftingSubpixel;
            },
            ShiftState::ShiftingSubpixel => {
                let shift_by = ANIMATION_INTERVAL_MS as f64 / ANIMATION_DURATION_MS as f64;
                self.subpixel_progress += shift_by * self.direction as f64;
                if self.subpixel_progress <= -0.99 || self.subpixel_progress >= 0.99 {
                    self.pixel_progress = (self.direction + self.pixel_progress as i64) as u64;
                    self.state = ShiftState::Normal;
                    self.subpixel_progress = 0.0;
                    if self.pixel_progress == 0 || self.pixel_progress >= PIXEL_SHIFT_WIDTH_PX {
                        self.state = ShiftState::WaitingAtEnd;
                        self.direction = -self.direction;
                    }
                }
            },
            ShiftState::WaitingAtEnd => {
                self.state = ShiftState::Normal;
                self.subpixel_progress = 0.0;
            }
        }
        (true, wait_for_state(self.state))
    }

    pub fn get(&self) -> (f64, f64) {
        let x_progress = self.pixel_progress as f64 + self.subpixel_progress;
        let mut y_progress = (x_progress + self.y_constant) % (PIXEL_SHIFT_HEIGHT_PX * 2) as f64;
        if y_progress > PIXEL_SHIFT_HEIGHT_PX as f64 {
            y_progress = (PIXEL_SHIFT_HEIGHT_PX * 2) as f64 - y_progress;
        }
        (x_progress - (PIXEL_SHIFT_WIDTH_PX / 2) as f64, y_progress - (PIXEL_SHIFT_HEIGHT_PX / 2) as f64)
    }
}
