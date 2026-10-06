//! メッセージの言語。英語が既定で、Windows の表示言語が日本語なら日本語にする。
//! `WSLC_COMPOSE_LANG=ja|en` で上書きできる。

use std::sync::OnceLock;

pub fn ja() -> bool {
    static JA: OnceLock<bool> = OnceLock::new();
    *JA.get_or_init(|| match std::env::var("WSLC_COMPOSE_LANG").as_deref() {
        Ok(v) if v.starts_with("ja") => true,
        Ok(v) if v.starts_with("en") => false,
        _ => system_is_japanese(),
    })
}

#[cfg(windows)]
fn system_is_japanese() -> bool {
    // LANGID GetUserDefaultUILanguage(); PRIMARYLANGID(id) = id & 0x3ff、LANG_JAPANESE = 0x11
    #[link(name = "kernel32")]
    unsafe extern "system" {
        safe fn GetUserDefaultUILanguage() -> u16;
    }
    GetUserDefaultUILanguage() & 0x3ff == 0x11
}

#[cfg(not(windows))]
fn system_is_japanese() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .is_some_and(|v| v.starts_with("ja"))
}

/// `tr!("日本語 {x}", "English {x}", args...)`。書式は format! と同じ
#[macro_export]
macro_rules! tr {
    ($ja:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::ja() { format!($ja $(, $arg)*) } else { format!($en $(, $arg)*) }
    };
}
