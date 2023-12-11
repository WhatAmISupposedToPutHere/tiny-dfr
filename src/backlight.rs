use std::{
    fs::{File, OpenOptions, self},
    path::{PathBuf, Path},
    time::Instant,
    io::Write
};
use anyhow::{Result, anyhow};
use input::event::{
    Event, switch::{Switch, SwitchEvent, SwitchState},
};
use crate::TIMEOUT_MS;

const MAX_DISPLAY_BRIGHTNESS: u32 = 509;
const MAX_TOUCH_BAR_BRIGHTNESS: u32 = 255;
const LOOKUP_TABLE_SIZE: usize = MAX_DISPLAY_BRIGHTNESS as usize + 1;
const BRIGHTNESS_DIM_TIMEOUT: i32 = TIMEOUT_MS * 3; // should be a multiple of TIMEOUT_MS
const BRIGHTNESS_OFF_TIMEOUT: i32 = TIMEOUT_MS * 6; // should be a multiple of TIMEOUT_MS
const DIMMED_BRIGHTNESS: u32 = 1;

fn read_attr(path: &Path, attr: &str) -> u32 {
    fs::read_to_string(path.join(attr))
        .expect(&format!("Failed to read {attr}"))
        .trim()
        .parse::<u32>()
        .expect(&format!("Failed to parse {attr}"))
}

fn find_backlight() -> Result<PathBuf> {
    for entry in fs::read_dir("/sys/class/backlight/")? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().contains("display-pipe") {
            return Ok(entry.path());
        }
    }
    Err(anyhow!("No Touch Bar backlight device found"))
}

fn find_display_backlight() -> Result<PathBuf> {
    for entry in fs::read_dir("/sys/class/backlight/")? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().eq("apple-panel-bl") {
            return Ok(entry.path());
        }
    }
    Err(anyhow!("No Built-in Retina Display backlight device found"))
}

fn set_backlight(mut file: &File, value: u32) {
    file.write(format!("{}\n", value).as_bytes()).unwrap();
}

pub struct BacklightManager {
    last_active: Instant,
    current_bl: u32,
    lid_state: SwitchState,
    bl_file: File,
    display_bl_path: PathBuf,
    lookup_table: [u32; MAX_DISPLAY_BRIGHTNESS as usize + 1]
}

impl BacklightManager {
    pub fn new() -> BacklightManager {
        let bl_path = find_backlight().unwrap();
        let display_bl_path = find_display_backlight().unwrap();
        let bl_file = OpenOptions::new().write(true).open(bl_path.join("brightness")).unwrap();
        let lookup_table = BacklightManager::generate_lookup_table();
        BacklightManager {
            bl_file,
            lid_state: SwitchState::Off,
            current_bl: read_attr(&bl_path, "brightness"),
            last_active: Instant::now(),
            display_bl_path,
            lookup_table
        }
    }
    pub fn generate_lookup_table() -> [u32; MAX_DISPLAY_BRIGHTNESS as usize + 1] {
        let mut lookup_table = [0; LOOKUP_TABLE_SIZE];
        for i in 0..=MAX_DISPLAY_BRIGHTNESS {
            let normalized = i as f32 / MAX_DISPLAY_BRIGHTNESS as f32;
            let adjusted = (normalized.powf(0.5) * MAX_TOUCH_BAR_BRIGHTNESS as f32) as u32 + 1; // Add one so that the touch bar does not turn off
            lookup_table[i as usize] = adjusted.min(MAX_TOUCH_BAR_BRIGHTNESS as u32); // Clamp the value to the maximum allowed brightness
        }
        lookup_table
    }
    pub fn process_event(&mut self, event: &Event) {
        match event {
            Event::Keyboard(_) | Event::Pointer(_) | Event::Gesture(_) | Event::Touch(_) => {
                self.last_active = Instant::now();
            },
            Event::Switch(SwitchEvent::Toggle(toggle)) => {
                match toggle.switch() {
                    Some(Switch::Lid) => {
                        self.lid_state = toggle.switch_state();
                        println!("Lid Switch event: {:?}", self.lid_state);
                        if toggle.switch_state() == SwitchState::Off {
                            self.last_active = Instant::now();
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    pub fn update_backlight(&mut self) {
        let since_last_active = (Instant::now() - self.last_active).as_millis() as u64;
        let new_bl = if self.lid_state == SwitchState::On {
            0
        } else if since_last_active < BRIGHTNESS_DIM_TIMEOUT as u64 {
            self.lookup_table[read_attr(&self.display_bl_path, "brightness") as usize]
        } else if since_last_active < BRIGHTNESS_OFF_TIMEOUT as u64 {
            DIMMED_BRIGHTNESS
        } else {
            0
        };
        if self.current_bl != new_bl {
            self.current_bl = new_bl;
            set_backlight(&self.bl_file, self.current_bl);
        }
    }
    pub fn current_bl(&self) -> u32 {
        self.current_bl
    }
}
