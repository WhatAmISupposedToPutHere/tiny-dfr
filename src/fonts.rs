#![allow(non_upper_case_globals)]
use std::ffi::{c_char, c_int, CStr, CString};
use std::ptr;

#[repr(C)]
struct FcPattern {
    _data: [u8; 0]
}
#[repr(C)]
struct FcConfig {
    _data: [u8; 0]
}

type FcResult = c_int;
const FcResultMatch: FcResult = 0;
const FcResultNoMatch: FcResult = 1;
const FcResultTypeMismatch: FcResult = 2;
const FcResultNoId: FcResult = 3;
const FcResultOutOfMemory: FcResult = 4;

type FcMatchKind = c_int;
const FcMatchPattern: FcMatchKind = 0;

pub enum FontConfigError {
    FontNotFound
}

pub struct FontConfig {
    config: *const FcConfig
}

impl FontConfig {
    pub fn new() -> FontConfig {
        let config = unsafe {
            FcInitLoadConfigAndFonts()
        };
        FontConfig {
            config
        }
    }
    pub fn match_pattern(&self, pattern: &Pattern) -> Result<Pattern, FontConfigError> {
        let mut result: FcResult = 0;
        let match_ = unsafe {
            FcFontMatch(self.config, pattern.pattern, &mut result)
        };
        if match_ == ptr::null_mut() {
            return Err(FontConfigError::FontNotFound);
        }
        Ok(Pattern {
            pattern: match_
        })
    }
    pub fn perform_substitutions(&self, pattern: &mut Pattern) {
        unsafe {
            if (FcConfigSubstitute(self.config, pattern.pattern, FcMatchPattern)) == 0 {
                panic!("Allocation error while loading fontconfig data");
            }
            FcDefaultSubstitute(pattern.pattern);
        }
    }
}

impl Drop for FontConfig {
    fn drop(&mut self) {
        unsafe {
            FcConfigDestroy(self.config)
        }
    }
}

fn throw_on_fcpattern_result(res: FcResult) {
    match res {
        FcResultMatch => {},
        FcResultNoMatch => {
            panic!("NULL pattern");
        },
        FcResultTypeMismatch => {
            panic!("Wrong type for pattern element");
        },
        FcResultNoId => {
            panic!("Unknown pattern element");
        },
        FcResultOutOfMemory => {
            panic!("Out of memory");
        },
        r => {
            panic!("Unknown fontconfig return value {:?}", r)
        }
    }
}

pub struct Pattern {
    pattern: *const FcPattern
}

impl Pattern {
    pub fn new(st: &str) -> Pattern {
        let cstr = CString::new(st).unwrap();
        let pattern = unsafe {
            FcNameParse(cstr.as_ptr())
        };
        Pattern {
            pattern
        }
    }
    pub fn get_file_name(&self) -> &str {
        let name = CString::new("file").unwrap();
        unsafe {
            let mut file_name = ptr::null();
            let res = FcPatternGetString(self.pattern, name.as_ptr(), 0, &mut file_name);
            throw_on_fcpattern_result(res);
            CStr::from_ptr(file_name).to_str().unwrap()
        }
    }
    pub fn get_font_index(&self) -> isize {
        let name = CString::new("index").unwrap();
        unsafe {
            let mut index = 0;
            let res = FcPatternGetInteger(self.pattern, name.as_ptr(), 0, &mut index);
            throw_on_fcpattern_result(res);
            index as isize
        }
    }
}

impl Drop for Pattern {
    fn drop(&mut self) {
        unsafe {
            FcPatternDestroy(self.pattern)
        }
    }
}

extern "C" {
    fn FcInitLoadConfigAndFonts() -> *const FcConfig;
    fn FcConfigDestroy(_: *const FcConfig) -> ();
    fn FcNameParse(_: *const c_char) -> *const FcPattern;
    fn FcPatternDestroy(_: *const FcPattern) -> ();
    fn FcFontMatch(_: *const FcConfig, _: *const FcPattern, _: *mut FcResult) -> *mut FcPattern;
    fn FcPatternGetString(_: *const FcPattern, _: *const c_char, _: c_int, _: *mut *const c_char) -> FcResult;
    fn FcPatternGetInteger(_: *const FcPattern, _: *const c_char, _: c_int, _: *mut c_int) -> FcResult;
    fn FcConfigSubstitute(_: *const FcConfig, _: *const FcPattern, _: FcMatchKind) -> c_int;
    fn FcDefaultSubstitute(_: *const FcPattern);
}
