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
    Err(anyhow!("No backlight device found"))
}

fn set_backlight(mut file: &File, value: u32) {
    file.write(format!("{}\n", value).as_bytes()).unwrap();
}

pub struct BacklightManager {
    last_active: Instant,
    current_bl: u32,
    lid_state: SwitchState,
    bl_file: File
}

impl BacklightManager {
    pub fn new() -> BacklightManager {
        let bl_path = find_backlight().unwrap();
        let bl_file = OpenOptions::new().write(true).open(bl_path.join("brightness")).unwrap();
        BacklightManager {
            bl_file,
            lid_state: SwitchState::Off,
            current_bl: read_attr(&bl_path, "brightness"),
            last_active: Instant::now()
        }
    }
    pub fn process_event(&mut self, event: &Event) {
        match event {
            Event::Keyboard(_) | Event::Pointer(_) | Event::Touch(_) => {
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
        } else if since_last_active < TIMEOUT_MS as u64 {
            128
        } else if since_last_active < TIMEOUT_MS as u64 * 2 {
            1
        } else {
            0
        };
        if self.current_bl != new_bl {
            self.current_bl = new_bl;
            set_backlight(&self.bl_file, self.current_bl);
        }
    }
}
