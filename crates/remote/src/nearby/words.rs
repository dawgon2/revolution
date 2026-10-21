//! The code's word list: 256 short, common English words written for this repo (ADR 0008 §3).
//!
//! Every word is 4–6 lowercase ASCII letters and no two share their first four letters, so a
//! word is recognised by its first four letters alone ("velv" is "velvet"). One word is 8 bits;
//! a code's three words are 24.

/// The words, indexed by one byte.
#[rustfmt::skip]
pub const WORDS: [&str; 256] = [
    "alarm", "alley", "amber", "angle", "apron", "arrow", "atlas", "attic",
    "badge", "bagel", "baker", "bamboo", "barn", "basket", "beach", "beard",
    "berry", "bike", "birch", "bison", "board", "boat", "book", "bottle",
    "brick", "bridge", "brush", "bucket", "cabin", "cable", "cactus", "camel",
    "candle", "canoe", "carrot", "castle", "chair", "chalk", "cheese", "cherry",
    "cider", "cloud", "clover", "coast", "comet", "coral", "cotton", "crane",
    "crown", "cube", "daisy", "desert", "domino", "donkey", "door", "dragon",
    "duck", "dune", "eagle", "echo", "elder", "ember", "engine", "fabric",
    "falcon", "fence", "fern", "ferry", "fiddle", "film", "finch", "fire",
    "flag", "flute", "forest", "fossil", "frame", "frost", "fruit", "galaxy",
    "garden", "gecko", "giant", "ginger", "glass", "glove", "goat", "goose",
    "grape", "guitar", "hammer", "harbor", "harp", "hedge", "helmet", "hill",
    "hobby", "horse", "house", "icon", "igloo", "iris", "island", "ivory",
    "jacket", "jelly", "jewel", "jigsaw", "juice", "kayak", "kettle", "kitten",
    "kite", "koala", "ladder", "lake", "lamp", "lava", "lawn", "lemon",
    "letter", "lime", "lion", "lizard", "llama", "lotus", "magnet", "mango",
    "maple", "marble", "meadow", "melon", "mint", "mirror", "monkey", "moon",
    "moss", "mouse", "napkin", "nature", "navy", "needle", "night", "noodle",
    "novel", "nutmeg", "ocean", "olive", "omelet", "onion", "orange", "orbit",
    "orchid", "otter", "oyster", "paddle", "paint", "palm", "paper", "parade",
    "parrot", "pasta", "pearl", "pebble", "pencil", "pepper", "pickle", "pigeon",
    "pillow", "pilot", "pirate", "planet", "plum", "pocket", "poppy", "potato",
    "prism", "puzzle", "quartz", "queen", "quest", "quill", "radio", "raft",
    "rain", "raven", "ribbon", "rice", "ridge", "river", "rocket", "roof",
    "rose", "ruby", "saddle", "salad", "salmon", "sand", "saucer", "seal",
    "seed", "shadow", "shell", "silver", "sketch", "sled", "slope", "soap",
    "sofa", "spark", "spider", "spring", "square", "star", "stone", "sugar",
    "summer", "sunset", "swan", "taco", "tango", "teapot", "tennis", "tiger",
    "timber", "toast", "tomato", "torch", "tower", "train", "tulip", "turtle",
    "twig", "valley", "velvet", "voyage", "wagon", "walnut", "walrus", "wave",
    "wheat", "willow", "window", "wizard", "wolf", "wonder", "wood", "yarn",
    "yellow", "yodel", "yogurt", "zero", "zigzag", "zinc", "zipper", "zone",
];

/// The index of the word whose first four letters match `typed`'s (already lowercase).
pub fn lookup(typed: &str) -> Option<u8> {
    let key = typed.get(..4)?;
    let pos = WORDS.iter().position(|w| w.get(..4) == Some(key))?;
    u8::try_from(pos).ok()
}

/// The word at `index`.
pub fn word(index: u8) -> &'static str {
    WORDS.get(usize::from(index)).copied().unwrap_or("alarm")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn list_is_256_distinct_lowercase_words() {
        assert_eq!(WORDS.len(), 256);
        let mut prefixes = HashSet::new();
        for w in WORDS {
            assert!((4..=6).contains(&w.len()), "{w}");
            assert!(w.bytes().all(|b| b.is_ascii_lowercase()), "{w}");
            assert!(prefixes.insert(&w[..4]), "two words start with {}", &w[..4]);
        }
    }

    #[test]
    fn lookup_by_prefix() {
        for (i, w) in WORDS.iter().enumerate() {
            assert_eq!(lookup(w), Some(i as u8));
            assert_eq!(lookup(&w[..4]), Some(i as u8));
            assert_eq!(word(i as u8), *w);
        }
        assert_eq!(lookup("abc"), None);
        assert_eq!(lookup("qqqq"), None);
        assert_eq!(lookup(""), None);
        assert_eq!(lookup("é"), None);
    }
}
