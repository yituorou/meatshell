//! Terminal colour schemes (Settings › Terminal colours).
//!
//! A scheme sets the terminal background, default text, cursor, selection and
//! the 16 ANSI colours. The user picks one scheme for dark mode and one for
//! light mode, so the terminal keeps following the app's light/dark switch.
//!
//! "default" keeps MeatShell's own terminal colours, including the light-mode
//! readability remaps in `presentation.rs`. Any other scheme is drawn the way
//! its author designed it, with no remapping.
//!
//! The built-ins are a curated set from iTerm2-Color-Schemes (MIT), compiled in
//! as plain constants: no asset to load and nothing decoded at runtime. More
//! schemes can be imported from `.itermcolors` files; the file is copied into
//! `<data dir>/terminal-schemes/` and read back when the list is built.
//!
//! Rendering reads the active pair through [`active`], which costs one atomic
//! load per call: each thread caches the pair and only re-reads the shared
//! copy after [`set_active`] bumps the generation.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

pub(crate) type Rgb = (u8, u8, u8);

/// The colours a scheme sets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SchemeColors {
    pub bg: Rgb,
    pub fg: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    pub ansi: [Rgb; 16],
}

/// Where a scheme comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SchemeSource {
    Default,
    Builtin,
    Imported,
}

/// One entry of the scheme list.
#[derive(Clone, Debug)]
pub(crate) struct TermScheme {
    pub id: String,
    pub name: String,
    /// Dark background (by luminance), shown as the 深色 / 浅色 tag.
    pub dark: bool,
    pub source: SchemeSource,
    pub colors: SchemeColors,
}

pub(crate) const DEFAULT_ID: &str = "default";
/// Ids of imported schemes are this prefix plus the file stem.
pub(crate) const USER_PREFIX: &str = "user:";
const FILE_EXT: &str = "itermcolors";

const fn rgb(v: u32) -> Rgb {
    ((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

struct Builtin {
    id: &'static str,
    name: &'static str,
    dark: bool,
    /// bg, fg, cursor, selection
    base: [u32; 4],
    ansi: [u32; 16],
}

const fn builtin(
    id: &'static str,
    name: &'static str,
    dark: bool,
    base: [u32; 4],
    ansi: [u32; 16],
) -> Builtin {
    Builtin {
        id,
        name,
        dark,
        base,
        ansi,
    }
}

impl Builtin {
    fn colors(&self) -> SchemeColors {
        SchemeColors {
            bg: rgb(self.base[0]),
            fg: rgb(self.base[1]),
            cursor: rgb(self.base[2]),
            selection: rgb(self.base[3]),
            ansi: self.ansi.map(rgb),
        }
    }

    fn scheme(&self) -> TermScheme {
        TermScheme {
            id: self.id.to_string(),
            name: self.name.to_string(),
            dark: self.dark,
            source: SchemeSource::Builtin,
            colors: self.colors(),
        }
    }
}

// Generated from the iTerm2-Color-Schemes `.itermcolors` files (MIT,
// https://github.com/mbadolato/iTerm2-Color-Schemes). Dark schemes first.
//          id, name, dark, [bg, fg, cursor, selection], [ansi 0..15]
const BUILTINS: &[Builtin] = &[
    builtin(
        "dracula",
        "Dracula",
        true,
        [0x282a36, 0xf8f8f2, 0xf8f8f2, 0x44475a],
        [
            0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2,
            0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
        ],
    ),
    builtin(
        "catppuccin-mocha",
        "Catppuccin Mocha",
        true,
        [0x1e1e2e, 0xcdd6f4, 0xf5e0dc, 0xf5e0dc],
        [
            0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de,
            0x585b70, 0xf7aec2, 0xc2ecbf, 0xfcd682, 0xaeccfc, 0xf398da, 0xb1eae1, 0xa6adc8,
        ],
    ),
    builtin(
        "catppuccin-macchiato",
        "Catppuccin Macchiato",
        true,
        [0x24273a, 0xcad3f5, 0xf4dbd6, 0xf4dbd6],
        [
            0x494d64, 0xed8796, 0xa6da95, 0xeed49f, 0x8aadf4, 0xf5bde6, 0x8bd5ca, 0xb8c0e0,
            0x5b6078, 0xf2a7b2, 0xbde3b0, 0xf4e3c1, 0xadc5f7, 0xf493da, 0xa5ded6, 0xa5adcb,
        ],
    ),
    builtin(
        "catppuccin-frappe",
        "Catppuccin Frappe",
        true,
        [0x303446, 0xc6d0f5, 0xf2d5cf, 0xf2d5cf],
        [
            0x51576d, 0xe78284, 0xa6d189, 0xe5c890, 0x8caaee, 0xf4b8e4, 0x81c8be, 0xb5bfe2,
            0x626880, 0xeda0a2, 0xb9dba2, 0xecd7ae, 0xadc2f3, 0xf38ed8, 0x98d2ca, 0xa5adce,
        ],
    ),
    builtin(
        "tokyonight",
        "TokyoNight",
        true,
        [0x1a1b26, 0xc0caf5, 0xc0caf5, 0x33467c],
        [
            0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6,
            0x414868, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5,
        ],
    ),
    builtin(
        "tokyonight-storm",
        "TokyoNight Storm",
        true,
        [0x24283b, 0xc0caf5, 0xc0caf5, 0x364a82],
        [
            0x1d202f, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6,
            0x414868, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5,
        ],
    ),
    builtin(
        "nord",
        "Nord",
        true,
        [0x2e3440, 0xd8dee9, 0xeceff4, 0xeceff4],
        [
            0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0,
            0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
        ],
    ),
    builtin(
        "gruvbox-dark",
        "Gruvbox Dark",
        true,
        [0x282828, 0xebdbb2, 0xebdbb2, 0x665c54],
        [
            0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984,
            0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
        ],
    ),
    builtin(
        "one-half-dark",
        "One Half Dark",
        true,
        [0x282c34, 0xdcdfe4, 0xa3b3cc, 0x474e5d],
        [
            0x282c34, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4,
            0x5d677a, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4,
        ],
    ),
    builtin(
        "atom-one-dark",
        "Atom One Dark",
        true,
        [0x21252b, 0xabb2bf, 0xabb2bf, 0x323844],
        [
            0x21252b, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf,
            0x767676, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf,
        ],
    ),
    builtin(
        "solarized-dark",
        "Solarized Dark",
        true,
        [0x002b36, 0x839496, 0x839496, 0x073642],
        [
            0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5,
            0x002b36, 0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
        ],
    ),
    builtin(
        "monokai-pro",
        "Monokai Pro",
        true,
        [0x2d2a2e, 0xfcfcfa, 0xc1c0c0, 0x5b595c],
        [
            0x2d2a2e, 0xff6188, 0xa9dc76, 0xffd866, 0xfc9867, 0xab9df2, 0x78dce8, 0xfcfcfa,
            0x727072, 0xff6188, 0xa9dc76, 0xffd866, 0xfc9867, 0xab9df2, 0x78dce8, 0xfcfcfa,
        ],
    ),
    builtin(
        "ayu-mirage",
        "Ayu Mirage",
        true,
        [0x1f2430, 0xcccac2, 0xffcc66, 0x409fff],
        [
            0x171b24, 0xed8274, 0x87d96c, 0xfacc6e, 0x6dcbfa, 0xdabafa, 0x90e1c6, 0xc7c7c7,
            0x686868, 0xf28779, 0xd5ff80, 0xffd173, 0x73d0ff, 0xdfbfff, 0x95e6cb, 0xffffff,
        ],
    ),
    builtin(
        "everforest-dark",
        "Everforest Dark",
        true,
        [0x1e2326, 0xd3c6aa, 0xe69875, 0x4c3743],
        [
            0x7a8478, 0xe67e80, 0xa7c080, 0xdbbc7f, 0x7fbbb3, 0xd699b6, 0x83c092, 0xf2efdf,
            0xa6b0a0, 0xf85552, 0x8da101, 0xdfa000, 0x3a94c5, 0xdf69ba, 0x35a77c, 0xfffbef,
        ],
    ),
    builtin(
        "kanagawa-wave",
        "Kanagawa Wave",
        true,
        [0x1f1f28, 0xdcd7ba, 0xdcd7ba, 0xdcd7ba],
        [
            0x090618, 0xc34043, 0x76946a, 0xc0a36e, 0x7e9cd8, 0x957fb8, 0x6a9589, 0xc8c093,
            0x727169, 0xe82424, 0x98bb6c, 0xe6c384, 0x7fb4ca, 0x938aa9, 0x7aa89f, 0xdcd7ba,
        ],
    ),
    builtin(
        "rose-pine",
        "Rose Pine",
        true,
        [0x191724, 0xe0def4, 0xe0def4, 0x403d52],
        [
            0x26233a, 0xeb6f92, 0x31748f, 0xf6c177, 0x9ccfd8, 0xc4a7e7, 0xebbcba, 0xe0def4,
            0x6e6a86, 0xeb6f92, 0x31748f, 0xf6c177, 0x9ccfd8, 0xc4a7e7, 0xebbcba, 0xe0def4,
        ],
    ),
    builtin(
        "github-dark",
        "GitHub Dark",
        true,
        [0x0d1117, 0xe6edf3, 0x2f81f7, 0xe6edf3],
        [
            0x484f58, 0xff7b72, 0x3fb950, 0xd29922, 0x58a6ff, 0xbc8cff, 0x39c5cf, 0xb1bac4,
            0x6e7681, 0xffa198, 0x56d364, 0xe3b341, 0x79c0ff, 0xd2a8ff, 0x56d4dd, 0xffffff,
        ],
    ),
    builtin(
        "night-owl",
        "Night Owl",
        true,
        [0x011627, 0xd6deeb, 0x7e57c2, 0x5f7e97],
        [
            0x011627, 0xef5350, 0x22da6e, 0xaddb67, 0x82aaff, 0xc792ea, 0x21c7a8, 0xffffff,
            0x575656, 0xef5350, 0x22da6e, 0xffeb95, 0x82aaff, 0xc792ea, 0x7fdbca, 0xffffff,
        ],
    ),
    builtin(
        "tomorrow-night",
        "Tomorrow Night",
        true,
        [0x1d1f21, 0xc5c8c6, 0xc5c8c6, 0x373b41],
        [
            0x000000, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xffffff,
            0x000000, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xffffff,
        ],
    ),
    builtin(
        "snazzy",
        "Snazzy",
        true,
        [0x1e1f29, 0xebece6, 0xe4e4e4, 0x81aec6],
        [
            0x000000, 0xfc4346, 0x50fb7c, 0xf0fb8c, 0x49baff, 0xfc4cb4, 0x8be9fe, 0xededec,
            0x555555, 0xfc4346, 0x50fb7c, 0xf0fb8c, 0x49baff, 0xfc4cb4, 0x8be9fe, 0xededec,
        ],
    ),
    builtin(
        "catppuccin-latte",
        "Catppuccin Latte",
        false,
        [0xeff1f5, 0x4c4f69, 0xdc8a78, 0xdc8a78],
        [
            0xbcc0cc, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0x5c5f77,
            0xacb0be, 0xe7103f, 0x46b02f, 0xe49931, 0x3878f6, 0xef95d7, 0x19a1a8, 0x6c6f85,
        ],
    ),
    builtin(
        "tokyonight-day",
        "TokyoNight Day",
        false,
        [0xe1e2e7, 0x3760bf, 0x3760bf, 0x99a7df],
        [
            0xe9e9ed, 0xf52a65, 0x587539, 0x8c6c3e, 0x2e7de9, 0x9854f1, 0x007197, 0x6172b0,
            0xa1a6c5, 0xf52a65, 0x587539, 0x8c6c3e, 0x2e7de9, 0x9854f1, 0x007197, 0x3760bf,
        ],
    ),
    builtin(
        "solarized-light",
        "Solarized Light",
        false,
        [0xfdf6e3, 0x657b83, 0x657b83, 0xeee8d5],
        [
            0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5,
            0x002b36, 0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
        ],
    ),
    builtin(
        "gruvbox-light",
        "Gruvbox Light",
        false,
        [0xfbf1c7, 0x3c3836, 0x3c3836, 0x3c3836],
        [
            0xfbf1c7, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0x7c6f64,
            0x928374, 0x9d0006, 0x79740e, 0xb57614, 0x076678, 0x8f3f71, 0x427b58, 0x3c3836,
        ],
    ),
    builtin(
        "one-half-light",
        "One Half Light",
        false,
        [0xfafafa, 0x383a42, 0xbfceff, 0xbfceff],
        [
            0x383a42, 0xe45649, 0x50a14f, 0xc18401, 0x0184bc, 0xa626a4, 0x0997b3, 0xfafafa,
            0x4f525e, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xffffff,
        ],
    ),
    builtin(
        "atom-one-light",
        "Atom One Light",
        false,
        [0xf9f9f9, 0x2a2c33, 0xbbbbbb, 0xd0dffc],
        [
            0x000000, 0xde3e35, 0x3f953a, 0xd2b67c, 0x2f5af3, 0x950095, 0x3f953a, 0xbbbbbb,
            0x000000, 0xde3e35, 0x3f953a, 0xd2b67c, 0x2f5af3, 0xa00095, 0x3f953a, 0xffffff,
        ],
    ),
    builtin(
        "ayu-light",
        "Ayu Light",
        false,
        [0xf8f9fa, 0x5c6166, 0xffaa33, 0x035bd6],
        [
            0x000000, 0xea6c6d, 0x6cbf43, 0xeca944, 0x3199e1, 0x9e75c7, 0x46ba94, 0xbababa,
            0x686868, 0xf07171, 0x86b300, 0xf2ae49, 0x399ee6, 0xa37acc, 0x4cbf99, 0xd1d1d1,
        ],
    ),
    builtin(
        "everforest-light",
        "Everforest Light",
        false,
        [0xefebd4, 0x5c6a72, 0xf57d26, 0xeaedc8],
        [
            0x7a8478, 0xe67e80, 0xa7c080, 0xdbbc7f, 0x7fbbb3, 0xd699b6, 0x83c092, 0xf2efdf,
            0xa6b0a0, 0xf85552, 0x8da101, 0xdfa000, 0x3a94c5, 0xdf69ba, 0x35a77c, 0xfffbef,
        ],
    ),
    builtin(
        "rose-pine-dawn",
        "Rose Pine Dawn",
        false,
        [0xfaf4ed, 0x575279, 0x575279, 0xdfdad9],
        [
            0xf2e9e1, 0xb4637a, 0x286983, 0xea9d34, 0x56949f, 0x907aa9, 0xd7827e, 0x575279,
            0x9893a5, 0xb4637a, 0x286983, 0xea9d34, 0x56949f, 0x907aa9, 0xd7827e, 0x575279,
        ],
    ),
    builtin(
        "github-light",
        "GitHub Light",
        false,
        [0xffffff, 0x1f2328, 0x0969da, 0x1f2328],
        [
            0x24292f, 0xcf222e, 0x116329, 0x4d2d00, 0x0969da, 0x8250df, 0x1b7c83, 0x6e7781,
            0x57606a, 0xa40e26, 0x1a7f37, 0x633c01, 0x218bff, 0xa475f9, 0x3192aa, 0x8c959f,
        ],
    ),
    builtin(
        "tomorrow",
        "Tomorrow",
        false,
        [0xffffff, 0x4d4d4c, 0x4d4d4c, 0xd6d6d6],
        [
            0x000000, 0xc82829, 0x718c00, 0xeab700, 0x4271ae, 0x8959a8, 0x3e999f, 0xffffff,
            0x000000, 0xc82829, 0x718c00, 0xeab700, 0x4271ae, 0x8959a8, 0x3e999f, 0xffffff,
        ],
    ),
    builtin(
        "kanagawa-lotus",
        "Kanagawa Lotus",
        false,
        [0xf2ecbc, 0x545464, 0x43436c, 0x545464],
        [
            0x1f1f28, 0xc84053, 0x6f894e, 0x77713f, 0x4d699b, 0xb35b79, 0x597b75, 0x545464,
            0x8a8980, 0xd7474b, 0x6e915f, 0x836f4a, 0x6693bf, 0x624c83, 0x5e857a, 0x43436c,
        ],
    ),
    builtin(
        "nord-light",
        "Nord Light",
        false,
        [0xe5e9f0, 0x414858, 0x88c0d0, 0xd8dee9],
        [
            0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xd8dee9,
            0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
        ],
    ),
    builtin(
        "flexoki-light",
        "Flexoki Light",
        false,
        [0xfffcf0, 0x100f0f, 0x100f0f, 0xcecdc3],
        [
            0x100f0f, 0xaf3029, 0x66800b, 0xad8301, 0x205ea6, 0xa02f6f, 0x24837b, 0x6f6e69,
            0xb7b5ac, 0xd14d41, 0x879a39, 0xd0a215, 0x4385be, 0xce5d97, 0x3aa99f, 0xcecdc3,
        ],
    ),
];

/// MeatShell's own terminal colours, for the list row and the preview card.
/// Rendering does not use this: the default path in `presentation.rs` keeps its
/// light-mode remaps, and the background comes from the colour theme.
pub(crate) fn default_colors(dark: bool) -> SchemeColors {
    let (bg, fg, selection) = if dark {
        ((0x0e, 0x0f, 0x13), (0xd4, 0xd4, 0xd4), (0x4a, 0x90, 0xe2))
    } else {
        ((0xfa, 0xfa, 0xfa), (0x2d, 0x2d, 0x2f), (0x00, 0x71, 0xe3))
    };
    SchemeColors {
        bg,
        fg,
        cursor: fg,
        selection,
        ansi: if dark {
            super::presentation::ANSI16_DARK
        } else {
            super::presentation::ANSI16_LIGHT
        },
    }
}

fn default_scheme(dark: bool) -> TermScheme {
    TermScheme {
        id: DEFAULT_ID.to_string(),
        name: String::new(), // the UI shows a localised "Default"
        dark,
        source: SchemeSource::Default,
        colors: default_colors(dark),
    }
}

/// Folder that holds imported `.itermcolors` files.
pub(crate) fn schemes_dir() -> PathBuf {
    crate::config::data_dir().join("terminal-schemes")
}

/// Every scheme for the picker: default (in the slot's own mode), the
/// built-ins, then imported ones by name.
pub(crate) fn all(dark_slot: bool) -> Vec<TermScheme> {
    all_in(&schemes_dir(), dark_slot)
}

fn all_in(dir: &Path, dark_slot: bool) -> Vec<TermScheme> {
    let mut rows = vec![default_scheme(dark_slot)];
    rows.extend(BUILTINS.iter().map(Builtin::scheme));
    rows.extend(imported_in(dir));
    rows
}

fn imported_in(dir: &Path) -> Vec<TermScheme> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut rows: Vec<TermScheme> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case(FILE_EXT))
        })
        .filter_map(|path| {
            let stem = path.file_stem()?.to_string_lossy().into_owned();
            let colors = parse_itermcolors(&std::fs::read_to_string(&path).ok()?).ok()?;
            Some(TermScheme {
                id: format!("{USER_PREFIX}{stem}"),
                dark: luminance(colors.bg) < 0.5,
                name: stem,
                source: SchemeSource::Imported,
                colors,
            })
        })
        .collect();
    rows.sort_by_key(|s| s.name.to_lowercase());
    rows
}

/// Colours for a saved scheme id. `None` = the default colours, which is also
/// what an unknown id or a deleted / broken imported file falls back to.
pub(crate) fn lookup(id: &str) -> Option<SchemeColors> {
    lookup_in(&schemes_dir(), id)
}

fn lookup_in(dir: &Path, id: &str) -> Option<SchemeColors> {
    if let Some(stem) = id.strip_prefix(USER_PREFIX) {
        let text = std::fs::read_to_string(dir.join(format!("{stem}.{FILE_EXT}"))).ok()?;
        return parse_itermcolors(&text).ok();
    }
    BUILTINS.iter().find(|b| b.id == id).map(Builtin::colors)
}

/// Import an `.itermcolors` file: validate it, copy it into the schemes folder
/// (a file with the same name is replaced) and return its id.
pub(crate) fn import(path: &Path) -> Result<String, String> {
    import_into(&schemes_dir(), path)
}

fn import_into(dir: &Path, path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse_itermcolors(&text)?;
    let stem = path
        .file_stem()
        .map(|s| sanitize_stem(&s.to_string_lossy()))
        .unwrap_or_else(|| "scheme".to_string());
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{stem}.{FILE_EXT}")), text).map_err(|e| e.to_string())?;
    Ok(format!("{USER_PREFIX}{stem}"))
}

/// Delete an imported scheme. Built-ins and unknown ids are left alone.
pub(crate) fn remove(id: &str) -> bool {
    remove_in(&schemes_dir(), id)
}

fn remove_in(dir: &Path, id: &str) -> bool {
    match id.strip_prefix(USER_PREFIX) {
        // Only names import could have written, so an id can't reach outside.
        Some(stem) if sanitize_stem(stem) == stem => {
            std::fs::remove_file(dir.join(format!("{stem}.{FILE_EXT}"))).is_ok()
        }
        _ => false,
    }
}

/// A safe file stem: letters, digits, spaces and `-_.()+`, at most 64 chars.
fn sanitize_stem(stem: &str) -> String {
    let clean: String = stem
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')' | '+'))
        .take(64)
        .collect();
    let clean = clean.trim().trim_matches('.').trim().to_string();
    if clean.is_empty() {
        "scheme".to_string()
    } else {
        clean
    }
}

/// Parse an iTerm2 `.itermcolors` property list. The 16 ANSI colours plus
/// background and foreground are required; cursor and selection fall back to
/// the foreground and bright black.
pub(crate) fn parse_itermcolors(xml: &str) -> Result<SchemeColors, String> {
    let mut found: std::collections::HashMap<String, [f32; 3]> = Default::default();
    let mut depth = 0usize;
    let mut outer_key: Option<String> = None; // colour name waiting for its <dict>
    let mut current: Option<(String, [f32; 3])> = None; // colour being read
    let mut inner_key: Option<String> = None; // component waiting for its value
    let mut rest = xml;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        let end = rest.find('>').ok_or("unterminated tag")?;
        let tag = &rest[..end];
        rest = &rest[end + 1..];
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        let (closing, body) = match tag.strip_prefix('/') {
            Some(b) => (true, b),
            None => (false, tag),
        };
        let self_closing = body.ends_with('/');
        let name = body
            .trim_end_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        match (name, closing) {
            ("dict", false) if self_closing => outer_key = None,
            ("dict", false) => {
                depth += 1;
                if depth == 2 {
                    current = outer_key.take().map(|k| (k, [0.0; 3]));
                    inner_key = None;
                }
            }
            ("dict", true) => {
                if depth == 2 {
                    if let Some((k, c)) = current.take() {
                        found.insert(k, c);
                    }
                }
                depth = depth.saturating_sub(1);
            }
            ("key", false) if !self_closing => {
                let close = rest.find("</key>").ok_or("unterminated <key>")?;
                let text = rest[..close].trim().to_string();
                rest = &rest[close + "</key>".len()..];
                match depth {
                    1 => outer_key = Some(text),
                    2 => inner_key = Some(text),
                    _ => {}
                }
            }
            ("real" | "integer", false) if !self_closing => {
                let close = rest.find('<').ok_or("unterminated number")?;
                let value: f32 = rest[..close]
                    .trim()
                    .parse()
                    .map_err(|_| format!("bad number {:?}", rest[..close].trim()))?;
                rest = &rest[close..];
                let slot = match inner_key.take().as_deref() {
                    Some("Red Component") => Some(0),
                    Some("Green Component") => Some(1),
                    Some("Blue Component") => Some(2),
                    _ => None,
                };
                if let (Some((_, c)), Some(i)) = (current.as_mut(), slot) {
                    c[i] = value;
                }
            }
            // Any other value (<string>, <true/>…) belongs to the pending key.
            (_, false) => match depth {
                1 => outer_key = None,
                2 => inner_key = None,
                _ => {}
            },
            _ => {}
        }
    }

    let to_rgb = |c: [f32; 3]| {
        let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        (ch(c[0]), ch(c[1]), ch(c[2]))
    };
    let get = |k: &str| found.get(k).copied().map(to_rgb);
    let need = |k: &str| get(k).ok_or_else(|| format!("missing {k}"));
    let mut ansi = [(0, 0, 0); 16];
    for (i, slot) in ansi.iter_mut().enumerate() {
        *slot = need(&format!("Ansi {i} Color"))?;
    }
    let fg = need("Foreground Color")?;
    Ok(SchemeColors {
        bg: need("Background Color")?,
        fg,
        cursor: get("Cursor Color").unwrap_or(fg),
        selection: get("Selection Color").unwrap_or(ansi[8]),
        ansi,
    })
}

// --- Active pair -----------------------------------------------------------

static GENERATION: AtomicU64 = AtomicU64::new(1);
static ACTIVE: RwLock<[Option<Arc<SchemeColors>>; 2]> = RwLock::new([None, None]);

thread_local! {
    static CACHE: RefCell<(u64, [Option<Arc<SchemeColors>>; 2])> =
        const { RefCell::new((0, [None, None])) };
}

/// Set the schemes used in dark and light mode (`None` = default colours).
/// Open terminals pick them up on their next render.
pub(crate) fn set_active(dark: Option<SchemeColors>, light: Option<SchemeColors>) {
    if let Ok(mut active) = ACTIVE.write() {
        *active = [dark.map(Arc::new), light.map(Arc::new)];
    }
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// The scheme for dark or light mode, `None` for the default colours.
pub(crate) fn active(is_dark: bool) -> Option<Arc<SchemeColors>> {
    let generation = GENERATION.load(Ordering::SeqCst);
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.0 != generation {
            cache.1 = ACTIVE.read().map(|a| a.clone()).unwrap_or_default();
            cache.0 = generation;
        }
        cache.1[usize::from(!is_dark)].clone()
    })
}

// --- Colour helpers --------------------------------------------------------

fn luminance((r, g, b): Rgb) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    (ch(a.0, b.0), ch(a.1, b.1), ch(a.2, b.2))
}

/// The selection fill to draw. MeatShell paints the selection *under* the
/// text without recolouring it, while some schemes (Catppuccin, Solarized)
/// pick a bright selection and expect inverted text. Pull such a selection
/// toward the background until the normal text on it reads (3:1).
pub(crate) fn readable_selection(c: &SchemeColors) -> Rgb {
    (0..=20)
        .map(|step| mix(c.selection, c.bg, step as f32 / 20.0))
        .find(|s| contrast(*s, c.fg) >= 3.0)
        .unwrap_or_else(|| mix(c.selection, c.bg, 0.6))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meatshell-schemes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write colours as an `.itermcolors` plist, as iTerm2 exports them.
    fn to_itermcolors(c: &SchemeColors) -> String {
        let entry = |key: String, (r, g, b): Rgb| {
            format!(
                "\t<key>{key}</key>\n\t<dict>\n\t\t<key>Alpha Component</key>\n\t\t<real>1</real>\n\
                 \t\t<key>Blue Component</key>\n\t\t<real>{}</real>\n\
                 \t\t<key>Color Space</key>\n\t\t<string>sRGB</string>\n\
                 \t\t<key>Green Component</key>\n\t\t<real>{}</real>\n\
                 \t\t<key>Red Component</key>\n\t\t<real>{}</real>\n\t</dict>\n",
                b as f32 / 255.0,
                g as f32 / 255.0,
                r as f32 / 255.0
            )
        };
        let mut body = String::new();
        for (i, a) in c.ansi.iter().enumerate() {
            body += &entry(format!("Ansi {i} Color"), *a);
        }
        body += &entry("Background Color".into(), c.bg);
        body += &entry("Foreground Color".into(), c.fg);
        body += &entry("Cursor Color".into(), c.cursor);
        body += &entry("Selection Color".into(), c.selection);
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n{body}</dict>\n</plist>\n"
        )
    }

    #[test]
    fn builtins_are_unique_readable_and_tagged_right() {
        let mut ids: Vec<&str> = BUILTINS.iter().map(|b| b.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), BUILTINS.len());
        assert!(BUILTINS.len() >= 30);
        for b in BUILTINS {
            assert_ne!(b.id, DEFAULT_ID);
            assert!(!b.id.starts_with(USER_PREFIX));
            let c = b.colors();
            assert_eq!(luminance(c.bg) < 0.5, b.dark, "{} dark tag", b.id);
            assert!(contrast(c.fg, c.bg) >= 3.0, "{} text", b.id);
            let sel = readable_selection(&c);
            assert!(contrast(sel, c.fg) >= 3.0, "{} selection", b.id);
        }
    }

    #[test]
    fn itermcolors_round_trip() {
        let dracula = BUILTINS[0].colors();
        assert_eq!(parse_itermcolors(&to_itermcolors(&dracula)), Ok(dracula));
    }

    #[test]
    fn itermcolors_optional_keys_fall_back_and_required_keys_error() {
        let mut c = BUILTINS[1].colors();
        let full = to_itermcolors(&c);
        // Drop the cursor and selection entries.
        let cut = full.find("\t<key>Cursor Color</key>").unwrap();
        let trimmed = format!("{}</dict>\n</plist>\n", &full[..cut]);
        c.cursor = c.fg;
        c.selection = c.ansi[8];
        assert_eq!(parse_itermcolors(&trimmed), Ok(c));

        let no_bg = full.replace("Background Color", "Something Else");
        assert_eq!(
            parse_itermcolors(&no_bg),
            Err("missing Background Color".to_string())
        );
        assert!(parse_itermcolors("not a plist").is_err());
    }

    #[test]
    fn import_list_lookup_and_remove() {
        let dir = temp_dir();
        let src = temp_dir().join("My Scheme!.itermcolors");
        let nord = BUILTINS.iter().find(|b| b.id == "nord").unwrap().colors();
        std::fs::write(&src, to_itermcolors(&nord)).unwrap();

        let id = import_into(&dir, &src).unwrap();
        assert_eq!(id, "user:My Scheme");
        assert_eq!(lookup_in(&dir, &id), Some(nord.clone()));

        let rows = all_in(&dir, true);
        assert_eq!(rows[0].source, SchemeSource::Default);
        let row = rows.iter().find(|r| r.id == id).unwrap();
        assert_eq!(row.source, SchemeSource::Imported);
        assert!(row.dark);

        // Broken files are refused and never copied in.
        let bad = temp_dir().join("bad.itermcolors");
        std::fs::write(&bad, "<plist><dict></dict></plist>").unwrap();
        assert!(import_into(&dir, &bad).is_err());
        assert!(lookup_in(&dir, "user:bad").is_none());

        assert!(!remove_in(&dir, "dracula"));
        assert!(!remove_in(&dir, "user:../escape"));
        assert!(remove_in(&dir, &id));
        assert!(lookup_in(&dir, &id).is_none());
    }

    #[test]
    fn unknown_and_default_ids_mean_default_colours() {
        let dir = temp_dir();
        assert_eq!(lookup_in(&dir, DEFAULT_ID), None);
        assert_eq!(lookup_in(&dir, ""), None);
        assert_eq!(lookup_in(&dir, "no-such-scheme"), None);
        assert_eq!(
            lookup_in(&dir, "dracula").map(|c| c.bg),
            Some((0x28, 0x2a, 0x36))
        );
    }
}
