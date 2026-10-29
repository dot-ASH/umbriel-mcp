use crate::input::{InputGlobals, discover_input_globals};
use anyhow::{Context, Result, anyhow};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{MemfdFlags, memfd_create};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{ErrorKind, Write as _};
use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};
use wayland_client::backend::WaylandError;
use wayland_client::protocol::wl_keyboard;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};

const KEYMAP_FORMAT_XKB_V1: u32 = 1;
const SOCKET_WRITABLE_TIMEOUT_SECONDS: i64 = 5;

const FIRST_XKB_KEYCODE: u32 = 8;
const LAST_XKB_KEYCODE: u32 = 255;
const KEYCODE_SLOTS: usize = (LAST_XKB_KEYCODE - FIRST_XKB_KEYCODE + 1) as usize;

const NAMED_KEYSYMS: [&str; 18] = [
    "Return",
    "Escape",
    "Tab",
    "space",
    "BackSpace",
    "Delete",
    "Home",
    "End",
    "Prior",
    "Next",
    "Left",
    "Right",
    "Up",
    "Down",
    "Caps_Lock",
    "Insert",
    "Print",
    "Pause",
];

const FUNCTION_KEYSYMS: [&str; 24] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14", "F15",
    "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modifier {
    Shift,
    Control,
    Alt,
    Super,
}

impl Modifier {
    pub fn mask(self) -> u32 {
        match self {
            Modifier::Shift => 1 << 0,
            Modifier::Control => 1 << 2,
            Modifier::Alt => 1 << 3,
            Modifier::Super => 1 << 6,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Some(Modifier::Control),
            "shift" => Some(Modifier::Shift),
            "alt" => Some(Modifier::Alt),
            "super" | "meta" | "win" | "logo" | "command" => Some(Modifier::Super),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum KeyId {
    Named(&'static str),
    Letter(char),
    Character(char),
}

impl KeyId {
    fn xkb_type(&self) -> &'static str {
        match self {
            KeyId::Letter(_) => "ALPHABETIC",
            _ => "ONE_LEVEL",
        }
    }

    fn keysym_names(&self) -> Vec<String> {
        match self {
            KeyId::Named(keysym) => vec![(*keysym).to_owned()],
            KeyId::Character(character) => vec![keysym_name_for_character(*character)],
            KeyId::Letter(lowercase) => {
                let uppercase = lowercase.to_uppercase().next().unwrap_or(*lowercase);
                vec![
                    keysym_name_for_character(*lowercase),
                    keysym_name_for_character(uppercase),
                ]
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Chord {
    pub modifiers: Vec<Modifier>,
    pub key: KeyId,
}

pub struct Keymap {
    text: String,
    evdev_codes: HashMap<KeyId, u32>,
}

fn fixed_keys() -> Vec<KeyId> {
    NAMED_KEYSYMS
        .into_iter()
        .map(KeyId::Named)
        .chain(FUNCTION_KEYSYMS.into_iter().map(KeyId::Named))
        .collect()
}

pub fn combined_modifier_mask(modifiers: &[Modifier]) -> u32 {
    modifiers
        .iter()
        .fold(0, |mask, modifier| mask | modifier.mask())
}

fn single_character(name: &str) -> Option<char> {
    let mut characters = name.chars();
    let first = characters.next()?;
    characters.next().is_none().then_some(first)
}

fn lowercase_and_uppercase(character: char) -> Option<(char, char)> {
    let mut lowercase = character.to_lowercase();
    let mut uppercase = character.to_uppercase();
    let (lower, upper) = (lowercase.next()?, uppercase.next()?);
    let each_is_one_character = lowercase.next().is_none() && uppercase.next().is_none();
    (each_is_one_character && lower != upper).then_some((lower, upper))
}

fn key_for_character(character: char) -> KeyId {
    match lowercase_and_uppercase(character) {
        Some((lowercase, _)) => KeyId::Letter(lowercase),
        None => KeyId::Character(character),
    }
}

fn named_key(name: &str) -> Option<KeyId> {
    let lowercase = name.to_ascii_lowercase();
    let function_number = lowercase
        .strip_prefix('f')
        .and_then(|digits| digits.parse::<usize>().ok())
        .filter(|number| (1..=FUNCTION_KEYSYMS.len()).contains(number));
    if let Some(number) = function_number {
        return Some(KeyId::Named(FUNCTION_KEYSYMS[number - 1]));
    }
    let keysym = match lowercase.as_str() {
        "enter" | "return" => "Return",
        "esc" | "escape" => "Escape",
        "tab" => "Tab",
        "space" => "space",
        "backspace" => "BackSpace",
        "delete" | "del" => "Delete",
        "home" => "Home",
        "end" => "End",
        "pageup" | "page_up" => "Prior",
        "pagedown" | "page_down" => "Next",
        "left" | "leftarrow" => "Left",
        "right" | "rightarrow" => "Right",
        "up" | "uparrow" => "Up",
        "down" | "downarrow" => "Down",
        "capslock" => "Caps_Lock",
        "insert" => "Insert",
        "printscreen" => "Print",
        "pause" => "Pause",
        _ => return None,
    };
    Some(KeyId::Named(keysym))
}

fn parse_key(name: &str) -> Result<KeyId> {
    match single_character(name) {
        Some(character) => Ok(key_for_character(character)),
        None => named_key(name).ok_or_else(|| anyhow!("unknown key {name:?} in key chord")),
    }
}

fn is_uppercase_letter(name: &str) -> bool {
    single_character(name)
        .and_then(lowercase_and_uppercase)
        .zip(single_character(name))
        .is_some_and(|((_, upper), typed)| typed == upper)
}

pub fn parse_chord(chord: &str) -> Result<Chord> {
    let mut modifiers = Vec::new();
    let mut key = None;
    let mut shift_implied_by_uppercase = false;
    for part in chord
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if let Some(modifier) = Modifier::from_name(part) {
            if !modifiers.contains(&modifier) {
                modifiers.push(modifier);
            }
        } else if key.replace(parse_key(part)?).is_some() {
            return Err(anyhow!(
                "key chord {chord:?} has more than one non-modifier key"
            ));
        } else {
            shift_implied_by_uppercase = is_uppercase_letter(part);
        }
    }
    let key = key.ok_or_else(|| anyhow!("key chord {chord:?} does not contain a key"))?;
    if shift_implied_by_uppercase && !modifiers.contains(&Modifier::Shift) {
        modifiers.push(Modifier::Shift);
    }
    Ok(Chord { modifiers, key })
}

pub fn keys_for_text(text: &str) -> Result<Vec<KeyId>> {
    let mut keys = Vec::new();
    for character in text.chars() {
        match character {
            '\n' => keys.push(KeyId::Named("Return")),
            '\t' => keys.push(KeyId::Named("Tab")),
            '\r' => {}
            control if control.is_control() => {
                return Err(anyhow!(
                    "cannot type control character U+{:04X}",
                    u32::from(control)
                ));
            }
            printable => keys.push(KeyId::Character(printable)),
        }
    }
    Ok(keys)
}

pub fn split_into_keymap_sized_chunks(text: &str) -> Vec<&str> {
    let capacity = Keymap::character_capacity();
    let mut chunks = Vec::new();
    let mut distinct_in_chunk = HashSet::new();
    let mut chunk_start = 0;
    for (offset, character) in text.char_indices() {
        let starts_new_chunk =
            !distinct_in_chunk.contains(&character) && distinct_in_chunk.len() == capacity;
        if starts_new_chunk {
            chunks.push(&text[chunk_start..offset]);
            chunk_start = offset;
            distinct_in_chunk.clear();
        }
        distinct_in_chunk.insert(character);
    }
    if chunk_start < text.len() {
        chunks.push(&text[chunk_start..]);
    }
    chunks
}

pub fn keysym_name_for_character(character: char) -> String {
    format!("U{:04X}", u32::from(character))
}

impl Keymap {
    pub fn character_capacity() -> usize {
        KEYCODE_SLOTS - fixed_keys().len()
    }

    pub fn build(extra_keys: &[KeyId]) -> Result<Self> {
        let mut keys = fixed_keys();
        for key in extra_keys {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
        if keys.len() > KEYCODE_SLOTS {
            return Err(anyhow!(
                "{} keys do not fit in one keymap (at most {KEYCODE_SLOTS})",
                keys.len()
            ));
        }
        let evdev_codes = keys
            .iter()
            .enumerate()
            .map(|(index, key)| (key.clone(), index as u32))
            .collect();
        Ok(Self {
            text: render_keymap(&keys),
            evdev_codes,
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn evdev_code(&self, key: &KeyId) -> Option<u32> {
        self.evdev_codes.get(key).copied()
    }
}

fn render_keymap(keys: &[KeyId]) -> String {
    let last_keycode = FIRST_XKB_KEYCODE + keys.len() as u32 - 1;
    let mut text = String::from("xkb_keymap {\n");
    let _ = writeln!(text, "xkb_keycodes \"umbriel\" {{");
    let _ = writeln!(text, "minimum = {FIRST_XKB_KEYCODE};");
    let _ = writeln!(text, "maximum = {last_keycode};");
    for index in 0..keys.len() as u32 {
        let keycode = FIRST_XKB_KEYCODE + index;
        let _ = writeln!(text, "<K{keycode}> = {keycode};");
    }
    text.push_str("};\n");
    text.push_str(
        "xkb_types \"umbriel\" {\n\
         type \"ONE_LEVEL\" {\n\
         modifiers = None;\n\
         map[None] = Level1;\n\
         level_name[Level1] = \"Any\";\n\
         };\n\
         type \"ALPHABETIC\" {\n\
         modifiers = Shift+Lock;\n\
         map[Shift] = Level2;\n\
         map[Lock] = Level2;\n\
         level_name[Level1] = \"Base\";\n\
         level_name[Level2] = \"Caps\";\n\
         };\n\
         };\n",
    );
    text.push_str(
        "xkb_compat \"umbriel\" {\n\
         interpret Caps_Lock { action = LockMods(modifiers = Lock); };\n\
         };\n",
    );
    let _ = writeln!(text, "xkb_symbols \"umbriel\" {{");
    for (index, key) in keys.iter().enumerate() {
        let keycode = FIRST_XKB_KEYCODE + index as u32;
        let _ = writeln!(
            text,
            "key <K{keycode}> {{ type[Group1] = \"{}\", symbols[Group1] = [ {} ] }};",
            key.xkb_type(),
            key.keysym_names().join(", ")
        );
    }
    text.push_str("};\n};\n");
    text
}

impl Dispatch<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1, ()> for InputGlobals {
    fn event(
        _: &mut Self,
        _: &zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
        _: zwp_virtual_keyboard_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

struct VirtualKeyboard {
    connection: Connection,
    queue: EventQueue<InputGlobals>,
    globals: InputGlobals,
    keyboard: zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
    installed_keymap: Option<Keymap>,
    started_at: Instant,
}

impl VirtualKeyboard {
    fn connect() -> Result<Self> {
        let (connection, queue, globals) = discover_input_globals()?;
        let seat = globals
            .seat
            .as_ref()
            .ok_or_else(|| anyhow!("Umbriel does not advertise the Wayland seat protocol"))?;
        let manager: &zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1 =
            globals.keyboard_manager.as_ref().ok_or_else(|| {
                anyhow!(
                    "Umbriel does not expose zwp_virtual_keyboard_manager_v1; check for a restricted Wayland session"
                )
            })?;
        let keyboard = manager.create_virtual_keyboard(seat, &queue.handle(), ());
        Ok(Self {
            connection,
            queue,
            globals,
            keyboard,
            installed_keymap: None,
            started_at: Instant::now(),
        })
    }

    fn time(&self) -> u32 {
        self.started_at.elapsed().as_millis().min(u32::MAX as u128) as u32
    }

    fn synchronize(&mut self, failure: &str) -> Result<()> {
        self.queue
            .roundtrip(&mut self.globals)
            .map(|_| ())
            .context(failure.to_owned())
    }

    fn install(&mut self, keymap: Keymap) -> Result<()> {
        let (descriptor, size) = keymap_descriptor(keymap.text())?;
        self.keyboard
            .keymap(KEYMAP_FORMAT_XKB_V1, descriptor.as_fd(), size);
        self.synchronize("Umbriel rejected the virtual keyboard keymap")?;
        self.installed_keymap = Some(keymap);
        Ok(())
    }

    fn wait_until_socket_is_writable(&self) -> Result<()> {
        let backend = self.connection.backend();
        let socket = backend.poll_fd();
        let mut watched = [PollFd::new(&socket, PollFlags::OUT)];
        let timeout = Timespec {
            tv_sec: SOCKET_WRITABLE_TIMEOUT_SECONDS,
            tv_nsec: 0,
        };
        let ready = poll(&mut watched, Some(&timeout))
            .context("could not wait for Umbriel to accept more input")?;
        if ready == 0 {
            return Err(anyhow!(
                "Umbriel stopped accepting keyboard input for {SOCKET_WRITABLE_TIMEOUT_SECONDS} seconds"
            ));
        }
        Ok(())
    }

    fn flush_waiting_for_room(&self, failure: &str) -> Result<()> {
        loop {
            match self.connection.flush() {
                Ok(()) => return Ok(()),
                Err(WaylandError::Io(error)) if error.kind() == ErrorKind::WouldBlock => {
                    self.wait_until_socket_is_writable()?;
                }
                Err(error) => return Err(error).context(failure.to_owned()),
            }
        }
    }

    fn send(&self, key: &KeyId, state: wl_keyboard::KeyState) -> Result<()> {
        let code = self
            .installed_keymap
            .as_ref()
            .and_then(|keymap| keymap.evdev_code(key))
            .ok_or_else(|| anyhow!("key {key:?} is missing from the installed keymap"))?;
        self.keyboard.key(self.time(), code, state as u32);
        self.flush_waiting_for_room("could not send the key event to Umbriel")
    }

    fn tap(&self, key: &KeyId) -> Result<()> {
        self.send(key, wl_keyboard::KeyState::Pressed)?;
        self.send(key, wl_keyboard::KeyState::Released)
    }

    fn hold_modifiers(&self, modifiers: &[Modifier]) -> Result<()> {
        self.keyboard
            .modifiers(combined_modifier_mask(modifiers), 0, 0, 0);
        self.flush_waiting_for_room("could not send the modifier state to Umbriel")
    }

    fn press_chord(&self, chord: &Chord) -> Result<()> {
        let outcome = self
            .hold_modifiers(&chord.modifiers)
            .and_then(|()| self.tap(&chord.key));
        let release_outcome = self.hold_modifiers(&[]);
        outcome.and(release_outcome)
    }
}

impl Drop for VirtualKeyboard {
    fn drop(&mut self) {
        self.keyboard.destroy();
        let _ = self.connection.flush();
    }
}

fn keymap_descriptor(keymap_text: &str) -> Result<(OwnedFd, u32)> {
    let descriptor = memfd_create("umbriel-mcp-keymap", MemfdFlags::CLOEXEC)
        .context("could not create a keymap buffer")?;
    let mut file = File::from(descriptor);
    file.write_all(keymap_text.as_bytes())
        .and_then(|()| file.write_all(&[0]))
        .context("could not write the keymap buffer")?;
    let size = u32::try_from(keymap_text.len() + 1).context("keymap is too large")?;
    Ok((file.into(), size))
}

pub fn type_text(text: &str, delay_ms: u32) -> Result<()> {
    let keys_per_chunk = split_into_keymap_sized_chunks(text)
        .into_iter()
        .map(keys_for_text)
        .collect::<Result<Vec<_>>>()?;
    let mut keyboard = VirtualKeyboard::connect()?;
    for keys in keys_per_chunk {
        keyboard.install(Keymap::build(&keys)?)?;
        for key in &keys {
            keyboard.tap(key)?;
            if delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(u64::from(delay_ms)));
            }
        }
    }
    keyboard.synchronize("could not confirm the typed text reached Umbriel")
}

pub fn press_keys(chord: &str) -> Result<()> {
    let chord = parse_chord(chord)?;
    let mut keyboard = VirtualKeyboard::connect()?;
    keyboard.install(Keymap::build(std::slice::from_ref(&chord.key))?)?;
    keyboard.press_chord(&chord)?;
    keyboard.synchronize("could not confirm the key press reached Umbriel")
}

#[cfg(test)]
mod tests {
    use super::{
        KeyId, Keymap, Modifier, combined_modifier_mask, keys_for_text, keysym_name_for_character,
        parse_chord, split_into_keymap_sized_chunks,
    };
    use std::collections::HashSet;

    fn distinct_cjk_text(count: usize) -> String {
        (0..count as u32)
            .map(|offset| char::from_u32(0x4E00 + offset).expect("valid CJK code point"))
            .collect()
    }

    #[test]
    fn a_chord_has_modifiers_then_one_key() {
        let chord = parse_chord("ctrl+d").unwrap();
        assert_eq!(chord.modifiers, vec![Modifier::Control]);
        assert_eq!(chord.key, KeyId::Letter('d'));

        let chord = parse_chord("Ctrl+Shift+Tab").unwrap();
        assert_eq!(chord.modifiers, vec![Modifier::Control, Modifier::Shift]);
        assert_eq!(chord.key, KeyId::Named("Tab"));
    }

    #[test]
    fn modifier_masks_use_the_standard_xkb_modifier_bits() {
        assert_eq!(Modifier::Shift.mask(), 1 << 0);
        assert_eq!(Modifier::Control.mask(), 1 << 2);
        assert_eq!(Modifier::Alt.mask(), 1 << 3);
        assert_eq!(Modifier::Super.mask(), 1 << 6);
        assert_eq!(
            combined_modifier_mask(&[Modifier::Control, Modifier::Shift]),
            0b101
        );
        assert_eq!(combined_modifier_mask(&[]), 0);
    }

    #[test]
    fn modifier_names_have_common_aliases() {
        for alias in ["super", "Meta", "WIN", "logo", "command"] {
            let chord = parse_chord(&format!("{alias}+q")).unwrap();
            assert_eq!(chord.modifiers, vec![Modifier::Super], "{alias}");
        }
        assert_eq!(
            parse_chord("control+alt+x").unwrap().modifiers,
            vec![Modifier::Control, Modifier::Alt]
        );
    }

    #[test]
    fn named_keys_have_common_aliases_and_function_keys_reach_f24() {
        let named = |name: &str| parse_chord(name).unwrap().key;
        assert_eq!(named("enter"), KeyId::Named("Return"));
        assert_eq!(named("Return"), KeyId::Named("Return"));
        assert_eq!(named("esc"), KeyId::Named("Escape"));
        assert_eq!(named("page_down"), KeyId::Named("Next"));
        assert_eq!(named("PageUp"), KeyId::Named("Prior"));
        assert_eq!(named("LeftArrow"), KeyId::Named("Left"));
        assert_eq!(named("del"), KeyId::Named("Delete"));
        assert_eq!(named("F1"), KeyId::Named("F1"));
        assert_eq!(named("f24"), KeyId::Named("F24"));
        assert!(parse_chord("f25").is_err());
        assert!(parse_chord("f0").is_err());
    }

    #[test]
    fn an_uppercase_letter_implies_shift_once() {
        let chord = parse_chord("ctrl+A").unwrap();
        assert_eq!(chord.modifiers, vec![Modifier::Control, Modifier::Shift]);
        assert_eq!(chord.key, KeyId::Letter('a'));

        let chord = parse_chord("ctrl+shift+A").unwrap();
        assert_eq!(chord.modifiers, vec![Modifier::Control, Modifier::Shift]);
    }

    #[test]
    fn punctuation_keys_are_plain_characters() {
        assert_eq!(parse_chord("ctrl+/").unwrap().key, KeyId::Character('/'));
        assert_eq!(parse_chord("alt+1").unwrap().key, KeyId::Character('1'));
    }

    #[test]
    fn malformed_chords_are_rejected_with_the_chord_in_the_message() {
        for chord in [
            "",
            "ctrl",
            "ctrl+shift",
            "a+b",
            "not-a-key",
            "ctrl+nonsense",
        ] {
            let error = parse_chord(chord).unwrap_err().to_string();
            assert!(
                error.contains("key"),
                "chord {chord:?} should explain the key problem, got: {error}"
            );
        }
    }

    #[test]
    fn text_becomes_character_keys_with_newline_and_tab_as_named_keys() {
        assert_eq!(
            keys_for_text("a\nb\t").unwrap(),
            vec![
                KeyId::Character('a'),
                KeyId::Named("Return"),
                KeyId::Character('b'),
                KeyId::Named("Tab"),
            ]
        );
    }

    #[test]
    fn carriage_returns_are_dropped_so_windows_newlines_type_one_return() {
        assert_eq!(
            keys_for_text("x\r\ny").unwrap(),
            vec![
                KeyId::Character('x'),
                KeyId::Named("Return"),
                KeyId::Character('y'),
            ]
        );
    }

    #[test]
    fn other_control_characters_cannot_be_typed() {
        let error = keys_for_text("bell\u{7}").unwrap_err().to_string();
        assert!(error.contains("control character"), "{error}");
    }

    #[test]
    fn unicode_keysyms_are_named_by_code_point() {
        assert_eq!(keysym_name_for_character('A'), "U0041");
        assert_eq!(keysym_name_for_character(' '), "U0020");
        assert_eq!(keysym_name_for_character('é'), "U00E9");
        assert_eq!(keysym_name_for_character('✓'), "U2713");
        assert_eq!(keysym_name_for_character('😀'), "U1F600");
    }

    #[test]
    fn the_keymap_is_a_complete_xkb_description() {
        let keymap = Keymap::build(&[]).unwrap();
        let text = keymap.text();
        assert!(text.starts_with("xkb_keymap {"));
        for section in ["xkb_keycodes", "xkb_types", "xkb_compat", "xkb_symbols"] {
            assert!(text.contains(section), "missing {section}");
        }
        for keysym in ["Return", "F24", "Prior", "Caps_Lock"] {
            assert!(text.contains(keysym), "missing {keysym}");
        }
    }

    #[test]
    fn characters_added_to_the_keymap_get_their_own_keycodes() {
        let keys = [
            KeyId::Character('é'),
            KeyId::Character('😀'),
            KeyId::Letter('d'),
        ];
        let keymap = Keymap::build(&keys).unwrap();
        assert!(keymap.text().contains("U00E9"));
        assert!(keymap.text().contains("U1F600"));
        assert!(
            keymap.text().contains("U0064, U0044"),
            "a letter key must carry both cases"
        );

        let mut codes = HashSet::new();
        for key in keys.iter().chain(&[KeyId::Named("Return")]) {
            let code = keymap.evdev_code(key).unwrap_or_else(|| panic!("{key:?}"));
            assert!(code <= 247, "evdev code {code} does not fit the xkb range");
            assert!(codes.insert(code), "{key:?} shares a keycode");
        }
    }

    #[test]
    fn duplicate_keys_share_one_keycode() {
        let keymap = Keymap::build(&[KeyId::Character('x'), KeyId::Character('x')]).unwrap();
        assert!(keymap.evdev_code(&KeyId::Character('x')).is_some());
    }

    #[test]
    fn a_keymap_refuses_more_characters_than_it_has_keycodes() {
        let too_many: Vec<KeyId> = distinct_cjk_text(Keymap::character_capacity() + 1)
            .chars()
            .map(KeyId::Character)
            .collect();
        assert!(Keymap::build(&too_many).is_err());

        let exactly_fitting: Vec<KeyId> = distinct_cjk_text(Keymap::character_capacity())
            .chars()
            .map(KeyId::Character)
            .collect();
        assert!(Keymap::build(&exactly_fitting).is_ok());
    }

    #[test]
    fn short_text_is_a_single_chunk() {
        assert_eq!(
            split_into_keymap_sized_chunks("hello hello"),
            vec!["hello hello"]
        );
        assert_eq!(split_into_keymap_sized_chunks(""), Vec::<&str>::new());
    }

    #[test]
    fn long_varied_text_is_split_into_chunks_that_each_fit_one_keymap() {
        let text = format!(
            "{}{}",
            distinct_cjk_text(Keymap::character_capacity() * 2 + 10),
            "tail"
        );
        let chunks = split_into_keymap_sized_chunks(&text);

        assert!(chunks.len() >= 3);
        assert_eq!(chunks.concat(), text);
        for chunk in chunks {
            let distinct: HashSet<char> = chunk.chars().collect();
            assert!(distinct.len() <= Keymap::character_capacity());
        }
    }
}
