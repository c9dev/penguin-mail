//! Caller-owned strings returned by WebView2 getters.

use windows::Win32::System::Com::CoTaskMemFree;
use windows::core::PWSTR;

/// Owns an out-parameter buffer even when a getter or UTF-16 conversion fails.
#[derive(Default)]
pub(crate) struct OwnedText(PWSTR);

impl OwnedText {
    pub(crate) fn out(&mut self) -> *mut PWSTR {
        &mut self.0
    }

    pub(crate) fn is_null(&self) -> bool {
        self.0.is_null()
    }

    pub(crate) fn read(&self) -> Result<String, std::string::FromUtf16Error> {
        if self.is_null() {
            Ok(String::new())
        } else {
            // WebView2 getters return a NUL-terminated CoTaskMem allocation.
            unsafe { self.0.to_string() }
        }
    }
}

impl Drop for OwnedText {
    fn drop(&mut self) {
        if !self.is_null() {
            unsafe { CoTaskMemFree(Some(self.0.as_ptr().cast())) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::CoTaskMemAlloc;

    fn allocated(words: &[u16]) -> OwnedText {
        let mut text = OwnedText::default();
        unsafe {
            let pointer = CoTaskMemAlloc(size_of_val(words)).cast::<u16>();
            assert!(!pointer.is_null());
            pointer.copy_from_nonoverlapping(words.as_ptr(), words.len());
            *text.out() = PWSTR(pointer);
        }
        text
    }

    #[test]
    fn a_getter_buffer_copies_valid_text_and_rejects_invalid_utf16() {
        assert_eq!(OwnedText::default().read().unwrap(), "");
        assert_eq!(
            allocated(&[b'h' as u16, b'i' as u16, 0]).read().unwrap(),
            "hi"
        );
        assert!(allocated(&[0xD800, 0]).read().is_err());
    }
}
