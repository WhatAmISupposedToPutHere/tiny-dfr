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

fn get_pixel_shift(x_progress: f64, y_constant: f64) -> (f64, f64) {
    let half_width = (PIXEL_SHIFT_WIDTH_PX / 2) as f64;
    let half_height = (PIXEL_SHIFT_HEIGHT_PX / 2) as f64;
    let mut shift_x = x_progress % (PIXEL_SHIFT_WIDTH_PX * 2) as f64;
    if shift_x <= half_width {
        shift_x = shift_x;
    } else if shift_x <= half_width * 2.0 {
        shift_x = half_width - (shift_x - half_width);
    } else if shift_x <= half_width * 3.0 {
        shift_x = 0.0 - (shift_x - half_width * 2.0);
    } else if shift_x <= half_width * 4.0 {
        shift_x = -half_width + (shift_x - half_width * 3.0);
    }

    let mut shift_y = (shift_x.abs() + y_constant) % (PIXEL_SHIFT_HEIGHT_PX * 2) as f64;
    if shift_y <= half_height {
        shift_y = shift_y;
    } else if shift_y <= half_height * 2.0 {
        shift_y = half_height - (shift_y - half_height);
    } else if shift_y <= half_height * 3.0 {
        shift_y = 0.0 - (shift_y - half_height * 2.0);
    } else if shift_y <= half_height * 4.0 {
        shift_y = -half_height + (shift_y - half_height * 3.0);
    }

    (shift_x, shift_y)
}

pub struct PixelShiftManager {
    last_active: Instant,
    x_progress: f64,
    y_constant: f64,
    in_animation: bool,
    in_prolonged_timeout: bool,
}

impl PixelShiftManager {
    pub fn new() -> PixelShiftManager {
        let x_progress: f64 = rand::thread_rng().gen_range(0..PIXEL_SHIFT_WIDTH_PX * 2) as f64;

        // add some randomness to the relationship between shifting on the x and y axis
        // so that pixel shifting doesn't follow the same 2d pattern every time
        let y_constant: f64 = rand::thread_rng().gen_range(0..PIXEL_SHIFT_HEIGHT_PX * 2) as f64;

        PixelShiftManager {
            last_active: Instant::now(),
            x_progress,
            y_constant,
            in_animation: false,
            in_prolonged_timeout: false
        }
    }

    pub fn update_pixel_shift(&mut self) -> (bool, i32) {
        let mut pixels_changed = false;
        let mut next_timeout_ms = -1;
        let time_now = Instant::now();
        let since_last_pixel_shift = (time_now - self.last_active).as_millis() as i32;

        if (self.in_animation && since_last_pixel_shift >= ANIMATION_INTERVAL_MS) ||
           (self.in_prolonged_timeout && since_last_pixel_shift >= PROLONGED_INTERVAL_MS) ||
           (!self.in_animation && !self.in_prolonged_timeout && since_last_pixel_shift >= INTERVAL_MS) {
            if ANIMATION_INTERVAL_MS == 0 || ANIMATION_DURATION_MS == 0 {
                self.x_progress += 1.0;
            } else {
                self.x_progress += ANIMATION_INTERVAL_MS as f64 / ANIMATION_DURATION_MS as f64;
            }
            self.last_active = time_now;
            pixels_changed = true;

            if (self.x_progress % 1.0).abs() <= 0.01 || (self.x_progress % 1.0).abs() >= 0.99 {
                self.x_progress = self.x_progress.round();
                self.in_animation = false;
                self.in_prolonged_timeout = false;
                //println!("finished pixel shift, now {:?}", get_pixel_shift(self.x_progress, self.y_constant));

                if self.x_progress as u64 % (PIXEL_SHIFT_WIDTH_PX * 2) == 0 {
                    self.x_progress = 0.0;
                } else if self.x_progress as u64 % (PIXEL_SHIFT_WIDTH_PX) == 0 {
                } else if self.x_progress as u64 % (PIXEL_SHIFT_WIDTH_PX / 2) == 0 {
                    self.in_prolonged_timeout = true;
                    //println!("pixel shift reached left or right edge, prolonging timeout");
                }
            } else {
                next_timeout_ms = ANIMATION_INTERVAL_MS;
                self.in_animation = true;
            }
        }

        (pixels_changed, next_timeout_ms)
    }

    pub fn get_pixel_shift(&self) -> (f64, f64) {
        get_pixel_shift(self.x_progress, self.y_constant)
    }
}
