//! Bundled sample-text corpus shared by the book-analysis examples.
//!
//! The book examples used to download their text from Project Gutenberg over the
//! network, which made the `Examples` CI workflow flaky whenever Gutenberg
//! rate-limited the downloads. This module replaces the network fetch with a
//! deterministic, offline generator so the examples exercise exactly the same
//! orchestration principles (parallel fan-out, split/join, map-reduce, analyze →
//! rank) without depending on any website.
//!
//! It is included by each example via `#[path = "shared/book_corpus.rs"]`. Living
//! in the `examples/shared/` subdirectory keeps cargo from treating it as its own
//! example target.
#![allow(dead_code)]

use std::time::Duration;

/// One book id is deliberately absent from the corpus so the examples can still
/// demonstrate partial-failure handling (the `failed_count` path and the
/// `Percentage(0.75)` analyze-phase join). Id 174 is "The Picture of Dorian Gray",
/// which appears in the two 12-book examples but not the 3-book sequential one.
pub const UNAVAILABLE_ID: u32 = 174;

/// Prepositions the generator may emit. Restricted to the set common to every
/// example's `PREPOSITIONS` list so each analyzer recognizes everything generated.
const GEN_PREPOSITIONS: &[&str] = &[
    "about", "above", "across", "after", "against", "along", "among", "around", "at", "before",
    "behind", "below", "beneath", "beside", "between", "beyond", "by", "down", "during", "for",
    "from", "in", "into", "near", "of", "off", "on", "over", "through", "to", "under", "up",
    "with", "within",
];

/// Non-preposition filler words for body text.
const FILLER: &[&str] = &[
    "the", "a", "and", "cat", "dog", "house", "river", "light", "shadow", "time", "hand", "world",
    "word", "story", "night", "morning", "walked", "looked", "river", "garden", "letter", "voice",
    "window", "candle", "ship", "fire", "snow", "road", "field", "song", "dream", "stone",
];

/// Tiny deterministic LCG. Seeded by the book id; no `rand`/clock so output is
/// fully reproducible run-to-run.
fn lcg(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 33
}

/// Generate reproducible prose for a book. The size of the preposition pool a
/// book draws from varies with its id, so different books get distinct
/// unique-preposition diversity and the rankings are meaningful and stable.
fn generate_text(id: u32) -> String {
    let mut state = (id as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);

    // How many distinct prepositions this book favors (8..=GEN_PREPOSITIONS.len()).
    let prep_pool_size = 8 + (id as usize % (GEN_PREPOSITIONS.len() - 7));
    // ~400..=700 words; at ~6 chars/word that is comfortably over the 1000-char guard.
    let word_count = 400 + (lcg(&mut state) as usize % 300);

    let mut out = String::with_capacity(word_count * 7);
    for i in 0..word_count {
        if i % 5 == 0 {
            let p = GEN_PREPOSITIONS[lcg(&mut state) as usize % prep_pool_size];
            out.push_str(p);
        } else {
            out.push_str(FILLER[lcg(&mut state) as usize % FILLER.len()]);
        }
        // Sprinkle in sentence breaks so the text reads like prose.
        if i % 12 == 11 {
            out.push_str(".\n");
        } else {
            out.push(' ');
        }
    }
    out
}

/// Fetch a book's text from the bundled corpus.
///
/// Mirrors the old network download: it awaits a small simulated latency so the
/// parallel fan-out stays observably concurrent, and returns an error for the one
/// deliberately-missing book so the failure-handling paths still run.
pub async fn fetch_book(id: u32, title: &str) -> Result<String, String> {
    let delay_ms = 10 + (id as u64 % 30);
    tokio::time::sleep(Duration::from_millis(delay_ms)).await;

    if id == UNAVAILABLE_ID {
        return Err(format!("sample corpus has no entry for {title} (id {id})"));
    }
    Ok(generate_text(id))
}
