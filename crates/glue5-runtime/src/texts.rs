//! Texts the game data lacks: its scripts are newer and name labels of
//! later DLC packs, which show as "<LABEL> missing". The ones GTA Online's
//! jobs show on the way (hosting, the Creator, the job lists) are given
//! here, in English.

use std::collections::HashMap;
use std::sync::Mutex;

use glue5_game::{Env, orig};

use crate::memory::Mem;

/// Labels and their texts.
const TEXTS: &[(&str, &str)] = &[
    ("FACES_WARNH2", "Alert"),
    ("QM_NO_1", "No session of this Job was found to join. Would you like to host it?"),
    ("COR_YOUR_CHAR", "Your Character"),
    ("UI_FLOW_OP_NEWSESH_M", "Find New Session"),
    ("PM_EXIT_GAME", "Exit Game"),
    ("PM_ARENA", "Arena War"),
    ("PM_RC_STUNT", "Stunt Races"),
    ("RC_TARGASSAULT", "Target Assault Races"),
    ("FMMC_RSTAR_KOT", "King of the Hill"),
    ("PM_TNEWVS", "Adversary Modes"),
    ("FMMC_SR", "Stunt Race"),
    ("FMMC_OWR", "Open Wheel Race"),
];

/// The engine's label hash (`rage::atStringHash`): Jenkins' one-at-a-time,
/// case-insensitive.
fn label_hash(label: &str) -> u32 {
    let mut h: u32 = 0;
    for b in label.bytes() {
        h = h.wrapping_add(u32::from(b.to_ascii_lowercase()));
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h = h.wrapping_add(h << 3);
    h ^= h >> 11;
    h.wrapping_add(h << 15)
}

/// The texts in the engine's heap, by hash (made when first asked for).
static PLACED: Mutex<Option<HashMap<u32, u64>>> = Mutex::new(None);

/// Hook for `CTextFile::GetInternal(unsigned int)`: the game data's text,
/// else one of these.
pub(crate) unsafe extern "C" fn text_get_internal(env: *mut Env, hash: u32) -> u64 {
    let text = unsafe { orig::text_get_internal(env, hash) };
    if text != 0 {
        return text;
    }
    let Some((_, ours)) = TEXTS.iter().find(|(label, _)| label_hash(label) == hash) else { return 0 };
    let mut placed = PLACED.lock().unwrap();
    let placed = placed.get_or_insert_default();
    if let Some(&at) = placed.get(&hash) {
        return at;
    }
    let at = unsafe { orig::builtin_malloc(env, ours.len() as u64 + 1) };
    if at != 0 {
        unsafe { Mem::of(env) }.write_str(at, ours, ours.len() + 1);
        placed.insert(hash, at);
    }
    at
}

pub(crate) fn hooks(hooks: &mut glue5_game::Hooks) {
    hooks.text_get_internal = Some(text_get_internal);
}

#[cfg(test)]
mod tests {
    #[test]
    fn label_hashes() {
        // Known joaat values.
        assert_eq!(super::label_hash("a"), 0xca2e9442);
        assert_eq!(super::label_hash("ADDER"), super::label_hash("adder"));
        assert_eq!(super::label_hash("adder"), 0xb779a091);
    }
}
